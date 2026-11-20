use crate::fork::{ForkConfig, RemoteDb, transaction_from_json};
use crate::protocol::{
    ChainConfig, CommandValue, DebugCommand, DebugEvent, Evaluation, Finished, FrameContext,
    FrameKind, HostCall, LogEntry, OpcodeExecution, PauseReason, PrankConfig, SessionConfig,
    SessionError, Snapshot, StateCommand, StorageSlot, TransactionKind, TransactionRequest,
};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, bounded, unbounded};
use revm::{
    Context, ExecuteCommitEvm, ExecuteEvm, InspectEvm, Inspector, MainBuilder, MainContext,
    context::{BlockEnv, CfgEnv, TxEnv},
    context_interface::{
        ContextTr, CreateScheme, JournalTr, LocalContextTr,
        journaled_state::account::JournaledAccountTr,
    },
    database::CacheDB,
    handler::instructions::EthInstructions,
    handler::{
        CallFrame, EthFrame, EthPrecompiles, FrameData, Handler, ItemOrResult, MainnetHandler,
        system_call::SYSTEM_ADDRESS,
    },
    interpreter::{
        CallInput, CallInputs, CallOutcome, CallScheme, FrameInput, Gas, InstructionContext,
        InstructionResult, Interpreter, InterpreterAction, InterpreterResult,
        interpreter::{EthInterpreter, ExtBytecode, InputsImpl, SharedMemory},
        interpreter_types::{InputsTr, Jumps, LegacyBytecode, LoopControl},
    },
    primitives::{Address, Bytes, TxKind, U256, address, hardfork::SpecId},
    state::{AccountInfo, Bytecode},
};
use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};

pub const DEFAULT_TARGET: Address = address!("1000000000000000000000000000000000000001");
pub const DEFAULT_CALLER: Address = address!("2000000000000000000000000000000000000002");
pub const CHEATCODE_ADDRESS: Address = address!("7109709ECfa91a80626fF3989D68f67F5b1DD12D");
pub const CONSOLE_ADDRESS: Address = address!("000000000000000000636F6e736F6c652e6c6f67");

const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
const BEACON_ROOTS_ADDRESS: Address = address!("000F3df6D732807Ef1319fB7B8bB8522d0Beac02");
const STATE_IDLE: u8 = 0;
const STATE_RUNNING: u8 = 1;
const STATE_PAUSED: u8 = 2;
const STATE_STOPPED: u8 = 3;

type ChasmContext = Context<BlockEnv, TxEnv, CfgEnv, CacheDB<RemoteDb>>;

enum EngineCommand {
    Transact(TransactionRequest),
    TransactEnv(TxEnv, bool),
    BeaconRoot(Bytes),
    SetBreakpoints(Vec<crate::Breakpoint>),
    Debug(DebugCommand),
    Abort,
}

struct Command {
    action: EngineCommand,
    reply: Sender<Result<CommandValue, SessionError>>,
}

#[derive(Clone)]
struct StepBackup {
    pc: usize,
    stack: Vec<U256>,
    memory: Vec<u8>,
    gas: Gas,
}

struct ChasmInspector {
    commands: Receiver<Command>,
    events: Sender<DebugEvent>,
    breakpoints: HashSet<(Address, usize)>,
    frames: Vec<FrameContext>,
    origin: Address,
    step_index: u64,
    backup: Option<StepBackup>,
    instruction_checkpoint: Option<revm::context_interface::journaled_state::JournalCheckpoint>,
    skip_breakpoint_once: bool,
    prank: Option<PrankConfig>,
    origin_restore: Vec<Option<Address>>,
    state: Arc<AtomicU8>,
    abort_requested: bool,
    steps_until_pause: Option<usize>,
    strict: bool,
}

impl ChasmInspector {
    fn snapshot(&self, interpreter: &Interpreter<EthInterpreter>, reason: PauseReason) -> Snapshot {
        let address = interpreter.input.target_address();
        let code_address = interpreter
            .input
            .bytecode_address()
            .copied()
            .unwrap_or(address);
        let opcode = interpreter.bytecode.opcode();
        let calldata = self
            .frames
            .last()
            .map(|frame| frame.calldata.clone())
            .unwrap_or_default();
        let is_static = self.frames.last().is_some_and(|frame| frame.is_static);
        let mut frames = self.frames.clone();
        if let Some(frame) = frames.last_mut() {
            Self::update_frame(frame, interpreter);
        }
        Snapshot {
            reason,
            step: self.step_index,
            address,
            code_address,
            caller: interpreter.input.caller_address(),
            origin: self.origin,
            value: interpreter.input.call_value(),
            calldata,
            is_static,
            depth: interpreter.input.depth(),
            pc: interpreter.bytecode.pc(),
            opcode,
            mnemonic: revm::bytecode::OpCode::name_by_op(opcode).to_owned(),
            gas_limit: interpreter.gas.limit(),
            gas_remaining: interpreter.gas.remaining(),
            gas_used: interpreter.gas.total_gas_spent(),
            gas_refund: interpreter.gas.refunded(),
            stack: interpreter.stack.data().iter().rev().copied().collect(),
            memory_size: interpreter.memory.len(),
            memory: Bytes::copy_from_slice(&interpreter.memory.context_memory()),
            frames,
        }
    }

    fn update_frame(frame: &mut FrameContext, interpreter: &Interpreter<EthInterpreter>) {
        frame.depth = interpreter.input.depth();
        frame.address = interpreter.input.target_address();
        frame.code_address = interpreter
            .input
            .bytecode_address()
            .copied()
            .unwrap_or(frame.address);
        frame.caller = interpreter.input.caller_address();
        frame.value = interpreter.input.call_value();
        frame.pc = interpreter.bytecode.pc();
        frame.opcode = interpreter.bytecode.opcode();
        frame.gas_limit = interpreter.gas.limit();
        frame.gas_remaining = interpreter.gas.remaining();
        frame.code = Bytes::copy_from_slice(interpreter.bytecode.bytecode_slice());
    }

    fn pause(
        &mut self,
        interpreter: &mut Interpreter<EthInterpreter>,
        context: &mut ChasmContext,
        reason: PauseReason,
    ) -> bool {
        self.state.store(STATE_PAUSED, Ordering::Release);
        if self
            .events
            .send(DebugEvent::Paused(Box::new(
                self.snapshot(interpreter, reason),
            )))
            .is_err()
        {
            return false;
        }

        let mut gas_changed = false;
        while let Ok(command) = self.commands.recv() {
            let mut resume = false;
            let result = match command.action {
                EngineCommand::Debug(DebugCommand::Snapshot) => Ok(CommandValue::Snapshot(
                    Box::new(self.snapshot(interpreter, reason)),
                )),
                EngineCommand::Debug(DebugCommand::SetStack { index, value }) => interpreter
                    .stack
                    .set(index, value)
                    .map(|()| CommandValue::Word(value))
                    .map_err(|error| SessionError::InvalidCommand(format!("{error:?}"))),
                EngineCommand::Debug(DebugCommand::WriteMemory { offset, data }) => {
                    let end = offset.saturating_add(data.len());
                    if end < offset {
                        Err(SessionError::InvalidCommand(
                            "memory range overflow".to_owned(),
                        ))
                    } else {
                        if end > interpreter.memory.len() {
                            interpreter.memory.resize(end);
                        }
                        interpreter.memory.set(offset, &data);
                        Ok(CommandValue::Number(data.len() as u64))
                    }
                }
                EngineCommand::Debug(DebugCommand::SetGas(value)) => {
                    interpreter.gas.set_remaining(value);
                    gas_changed = true;
                    Ok(CommandValue::Number(value))
                }
                EngineCommand::Debug(DebugCommand::SetPc(value)) => {
                    let code = interpreter.bytecode.bytecode_slice();
                    if code.get(value).copied() != Some(revm::bytecode::opcode::JUMPDEST) {
                        Err(SessionError::InvalidCommand(format!(
                            "0x{value:x} is not a JUMPDEST"
                        )))
                    } else {
                        interpreter.bytecode.absolute_jump(value);
                        Ok(CommandValue::Number(value as u64))
                    }
                }
                EngineCommand::Debug(DebugCommand::ReadStorage(key)) => {
                    let mut isolated = context.clone();
                    let result = isolated
                        .journal_mut()
                        .sload(interpreter.input.target_address(), key)
                        .map(|value| CommandValue::Word(value.data))
                        .map_err(|error| SessionError::InvalidCommand(format!("{error:?}")));
                    result
                }
                EngineCommand::Debug(DebugCommand::WriteStorage { key, value }) => context
                    .journal_mut()
                    .sstore(interpreter.input.target_address(), key, value)
                    .map(|_| CommandValue::Word(value))
                    .map_err(|error| SessionError::InvalidCommand(format!("{error:?}"))),
                EngineCommand::Debug(DebugCommand::State(command)) => {
                    execute_state_command(context, command)
                }
                EngineCommand::Debug(DebugCommand::SetPrank(prank)) => {
                    self.prank = prank;
                    Ok(CommandValue::None)
                }
                EngineCommand::Debug(DebugCommand::Evaluate { code, keep }) => execute_evaluation(
                    context,
                    interpreter,
                    code,
                    Bytes::new(),
                    interpreter.input.caller_address(),
                    U256::ZERO,
                    100_000,
                    keep,
                )
                .and_then(|result| {
                    if result.success {
                        Ok(CommandValue::Bytes(result.output))
                    } else {
                        Err(SessionError::InvalidCommand(
                            "speculative execution reverted".to_owned(),
                        ))
                    }
                }),
                EngineCommand::Debug(DebugCommand::EvaluateCall {
                    code,
                    data,
                    caller,
                    value,
                    gas_limit,
                    keep,
                }) => execute_evaluation(
                    context,
                    interpreter,
                    code,
                    data,
                    caller,
                    value,
                    gas_limit,
                    keep,
                )
                .map(CommandValue::Evaluation),
                EngineCommand::Debug(DebugCommand::ExecuteOpcode {
                    opcode,
                    arguments,
                    outputs,
                }) => execute_opcode(context, interpreter, opcode, arguments, outputs)
                    .map(CommandValue::OpcodeExecution),
                EngineCommand::Debug(DebugCommand::RespondHost { .. }) => {
                    Err(SessionError::InvalidCommand(
                        "no host call is waiting for a response".to_owned(),
                    ))
                }
                EngineCommand::Debug(DebugCommand::Step { count }) => {
                    self.steps_until_pause = Some(count.max(1));
                    resume = true;
                    Ok(CommandValue::None)
                }
                EngineCommand::Debug(DebugCommand::Resume) => {
                    self.steps_until_pause = None;
                    resume = true;
                    Ok(CommandValue::None)
                }
                EngineCommand::SetBreakpoints(breakpoints) => {
                    self.breakpoints = breakpoints
                        .into_iter()
                        .map(|point| (point.address, point.pc))
                        .collect();
                    Ok(CommandValue::Number(self.breakpoints.len() as u64))
                }
                EngineCommand::Transact(_)
                | EngineCommand::TransactEnv(_, _)
                | EngineCommand::BeaconRoot(_) => Err(SessionError::InvalidCommand(
                    "a transaction is already running".to_owned(),
                )),
                EngineCommand::Abort => {
                    self.abort_requested = true;
                    interpreter.halt(InstructionResult::Stop);
                    resume = true;
                    Ok(CommandValue::None)
                }
            };
            let _ = command.reply.send(result);
            if resume {
                self.state.store(STATE_RUNNING, Ordering::Release);
                return gas_changed;
            }
        }
        false
    }

    fn is_out_of_gas(result: InstructionResult) -> bool {
        matches!(
            result,
            InstructionResult::OutOfGas
                | InstructionResult::MemoryOOG
                | InstructionResult::MemoryLimitOOG
                | InstructionResult::PrecompileOOG
                | InstructionResult::InvalidOperandOOG
                | InstructionResult::ReentrancySentryOOG
        )
    }

    fn host_call(&mut self, context: &mut ChasmContext, inputs: &CallInputs) -> CallOutcome {
        self.state.store(STATE_PAUSED, Ordering::Release);
        let event = HostCall {
            address: inputs.bytecode_address,
            caller: inputs.caller,
            data: inputs.input.bytes(context),
            gas_limit: inputs.gas_limit,
        };
        if self
            .events
            .send(DebugEvent::HostCall(Box::new(event)))
            .is_err()
        {
            return call_result(inputs, Bytes::new(), true);
        }

        while let Ok(command) = self.commands.recv() {
            let response = match command.action {
                EngineCommand::Debug(DebugCommand::RespondHost { output, revert }) => {
                    let _ = command.reply.send(Ok(CommandValue::None));
                    self.state.store(STATE_RUNNING, Ordering::Release);
                    return call_result(inputs, output, revert);
                }
                EngineCommand::SetBreakpoints(breakpoints) => {
                    self.breakpoints = breakpoints
                        .into_iter()
                        .map(|point| (point.address, point.pc))
                        .collect();
                    Ok(CommandValue::Number(self.breakpoints.len() as u64))
                }
                EngineCommand::Debug(DebugCommand::State(command)) => {
                    execute_state_command(context, command)
                }
                EngineCommand::Debug(DebugCommand::SetPrank(prank)) => {
                    self.prank = prank;
                    Ok(CommandValue::None)
                }
                EngineCommand::Debug(_) => Err(SessionError::InvalidCommand(
                    "the engine is waiting for a host-call response".to_owned(),
                )),
                EngineCommand::Transact(_)
                | EngineCommand::TransactEnv(_, _)
                | EngineCommand::BeaconRoot(_) => Err(SessionError::InvalidCommand(
                    "a transaction is already running".to_owned(),
                )),
                EngineCommand::Abort => {
                    self.abort_requested = true;
                    let _ = command.reply.send(Ok(CommandValue::None));
                    self.state.store(STATE_RUNNING, Ordering::Release);
                    return call_result(inputs, Bytes::new(), true);
                }
            };
            let _ = command.reply.send(response);
        }
        call_result(inputs, Bytes::new(), true)
    }

    fn apply_prank(&mut self, context: &mut ChasmContext, inputs: &mut CallInputs) {
        let Some(prank) = self.prank.clone() else {
            return;
        };
        let is_delegate = inputs.scheme.is_delegate_call();
        if is_delegate && !prank.delegate {
            return;
        }
        let caller = if is_delegate {
            inputs.target_address
        } else {
            inputs.caller
        };
        if prank.caller.is_some_and(|expected| expected != caller) {
            return;
        }

        inputs.caller = prank.new_sender;
        if is_delegate {
            inputs.target_address = prank.new_sender;
        }
        if let Some(origin) = prank.new_origin {
            let previous = context.tx.caller;
            context.tx.caller = origin;
            self.origin = origin;
            if let Some(restore) = self.origin_restore.last_mut() {
                *restore = Some(previous);
            }
        }
        if !prank.persistent {
            self.prank = None;
        }
    }
}

fn call_result(inputs: &CallInputs, output: Bytes, revert: bool) -> CallOutcome {
    CallOutcome::new(
        InterpreterResult::new(
            if revert {
                InstructionResult::Revert
            } else {
                InstructionResult::Return
            },
            output,
            Gas::new(inputs.gas_limit),
        ),
        inputs.return_memory_offset.clone(),
    )
}

fn execute_state_command(
    context: &mut ChasmContext,
    command: StateCommand,
) -> Result<CommandValue, SessionError> {
    if matches!(
        command,
        StateCommand::ReadBalance(_)
            | StateCommand::ReadCode(_)
            | StateCommand::ReadNonce(_)
            | StateCommand::ReadStorage { .. }
    ) {
        return execute_state_command_inner(&mut context.clone(), command);
    }
    execute_state_command_inner(context, command)
}

fn execute_state_command_inner(
    context: &mut ChasmContext,
    command: StateCommand,
) -> Result<CommandValue, SessionError> {
    let invalid = |error| SessionError::InvalidCommand(format!("{error:?}"));
    match command {
        StateCommand::ReadBalance(address) => context
            .journal_mut()
            .load_account(address)
            .map(|account| CommandValue::Word(account.info.balance))
            .map_err(invalid),
        StateCommand::WriteBalance { address, value } => {
            let mut account = context
                .journal_mut()
                .load_account_mut(address)
                .map_err(invalid)?
                .data;
            account.set_balance(value);
            Ok(CommandValue::Word(value))
        }
        StateCommand::ReadCode(address) => context
            .journal_mut()
            .code(address)
            .map(|code| CommandValue::Bytes(code.data))
            .map_err(invalid),
        StateCommand::WriteCode { address, code } => {
            let mut account = context
                .journal_mut()
                .load_account_with_code_mut(address)
                .map_err(invalid)?
                .data;
            account.set_code_and_hash_slow(Bytecode::new_raw(code));
            Ok(CommandValue::None)
        }
        StateCommand::ReadNonce(address) => context
            .journal_mut()
            .load_account(address)
            .map(|account| CommandValue::Number(account.info.nonce))
            .map_err(invalid),
        StateCommand::WriteNonce { address, value } => {
            let mut account = context
                .journal_mut()
                .load_account_mut(address)
                .map_err(invalid)?
                .data;
            account.set_nonce(value);
            Ok(CommandValue::Number(value))
        }
        StateCommand::ReadStorage { address, key } => {
            let journal = context.journal_mut();
            journal.load_account(address).map_err(invalid)?;
            journal
                .sload(address, key)
                .map(|value| CommandValue::Word(value.data))
                .map_err(invalid)
        }
        StateCommand::WriteStorage {
            address,
            key,
            value,
        } => {
            let journal = context.journal_mut();
            journal.load_account(address).map_err(invalid)?;
            journal
                .sstore(address, key, value)
                .map(|_| CommandValue::Word(value))
                .map_err(invalid)
        }
        StateCommand::ReadTransient { address, key } => Ok(CommandValue::Word(
            context.journal_mut().tload(address, key),
        )),
        StateCommand::WriteTransient {
            address,
            key,
            value,
        } => {
            context.journal_mut().tstore(address, key, value);
            Ok(CommandValue::Word(value))
        }
        StateCommand::WarmStorage { address, key } => {
            let journal = context.journal_mut();
            journal.load_account(address).map_err(invalid)?;
            journal
                .sload(address, key)
                .map(|_| CommandValue::None)
                .map_err(invalid)
        }
        StateCommand::IsStorageWarm { address, key } => {
            let journal = context.journal();
            let loaded_warm = journal
                .state
                .get(&address)
                .and_then(|account| account.storage.get(&key))
                .is_some_and(|slot| !slot.is_cold_transaction_id(journal.transaction_id));
            Ok(CommandValue::Bool(
                loaded_warm || journal.warm_addresses.is_storage_warm(&address, &key),
            ))
        }
        StateCommand::Logs => Ok(CommandValue::Logs(
            context
                .journal()
                .logs()
                .iter()
                .map(|log| LogEntry {
                    address: log.address,
                    topics: log.topics().to_vec(),
                    data: log.data.data.clone(),
                })
                .collect(),
        )),
        StateCommand::ReadBlockNumber => Ok(CommandValue::Word(context.block.number)),
        StateCommand::WriteBlockNumber(value) => {
            context.block.number = value;
            Ok(CommandValue::Word(value))
        }
        StateCommand::ReadTimestamp => Ok(CommandValue::Word(context.block.timestamp)),
        StateCommand::WriteTimestamp(value) => {
            context.block.timestamp = value;
            Ok(CommandValue::Word(value))
        }
        StateCommand::ReadBaseFee => Ok(CommandValue::Number(context.block.basefee)),
        StateCommand::WriteBaseFee(value) => {
            context.block.basefee = value;
            Ok(CommandValue::Number(value))
        }
        StateCommand::ReadChainId => Ok(CommandValue::Number(context.cfg.chain_id)),
        StateCommand::WriteChainId(value) => {
            context.cfg.chain_id = value;
            Ok(CommandValue::Number(value))
        }
        StateCommand::ReadCoinbase => Ok(CommandValue::Bytes(Bytes::copy_from_slice(
            context.block.beneficiary.as_slice(),
        ))),
        StateCommand::WriteCoinbase(value) => {
            context.block.beneficiary = value;
            Ok(CommandValue::Bytes(Bytes::copy_from_slice(
                value.as_slice(),
            )))
        }
        StateCommand::ReadPrevrandao => Ok(CommandValue::Bytes(Bytes::copy_from_slice(
            context.block.prevrandao.unwrap_or_default().as_slice(),
        ))),
        StateCommand::WritePrevrandao(value) => {
            context.block.prevrandao = Some(value);
            Ok(CommandValue::Bytes(Bytes::copy_from_slice(
                value.as_slice(),
            )))
        }
        StateCommand::ReadDifficulty => Ok(CommandValue::Word(context.block.difficulty)),
        StateCommand::WriteDifficulty(value) => {
            context.block.difficulty = value;
            Ok(CommandValue::Word(value))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_evaluation(
    context: &mut ChasmContext,
    interpreter: &Interpreter<EthInterpreter>,
    code: Bytes,
    data: Bytes,
    caller: Address,
    value: U256,
    gas_limit: u64,
    keep: bool,
) -> Result<Evaluation, SessionError> {
    if gas_limit > 30_000_000 || interpreter.input.depth() >= 1023 {
        return Err(SessionError::InvalidCommand(
            "unsupported_evaluation: gas/depth bound exceeded".to_owned(),
        ));
    }
    let mut isolated = context.clone();
    // LocalContext clones share their memory arena; eval must not alias live calldata/memory.
    isolated.local = Default::default();
    isolated
        .journaled_state
        .database
        .db
        .limit_reads_until(std::time::Instant::now() + Duration::from_secs(8));
    let checkpoint = isolated.journal_mut().checkpoint();
    let nested = Interpreter::new(
        SharedMemory::new_with_buffer(isolated.local.shared_memory_buffer().clone()),
        ExtBytecode::new(Bytecode::new_raw(code)),
        InputsImpl {
            target_address: interpreter.input.target_address(),
            bytecode_address: interpreter.input.bytecode_address().copied(),
            caller_address: caller,
            input: CallInput::Bytes(data),
            call_value: value,
            depth: interpreter.input.depth() + 1,
        },
        interpreter.runtime_flag.is_static,
        context.cfg.spec,
        gas_limit,
    );
    let frame = EthFrame {
        data: FrameData::Call(CallFrame {
            return_memory_range: 0..0,
        }),
        input: FrameInput::Empty,
        depth: interpreter.input.depth() + 1,
        checkpoint,
        interpreter: nested,
        is_finished: false,
    };
    let result = run_evaluation_frames(&mut isolated, frame)?;
    let evaluation = Evaluation {
        success: result.result.is_ok(),
        gas_used: result.gas.total_gas_spent(),
        output: result.output,
    };
    if keep && evaluation.success {
        isolated.journaled_state.database.db.clear_read_limit();
        context.journaled_state = isolated.journaled_state;
    }
    Ok(evaluation)
}

fn run_evaluation_frames(
    context: &mut ChasmContext,
    root: EthFrame,
) -> Result<InterpreterResult, SessionError> {
    use revm::context_interface::{ContextError, local::OutFrame};
    let invalid = |error: ContextError<crate::ForkError>| {
        SessionError::InvalidCommand(format!("evaluation_failed: {error:?}"))
    };
    let instructions =
        EthInstructions::<EthInterpreter, ChasmContext>::new_mainnet_with_spec(context.cfg.spec);
    let mut precompiles = EthPrecompiles::new(context.cfg.spec);
    let mut frames = vec![root];
    let started = std::time::Instant::now();
    let mut transitions = 0usize;
    loop {
        transitions += 1;
        if transitions > 4096 || started.elapsed() > Duration::from_secs(8) {
            return Err(SessionError::InvalidCommand(
                "unsupported_evaluation: frame/time budget exceeded".to_owned(),
            ));
        }
        let frame = frames.last_mut().expect("evaluation root exists");
        let action = frame.interpreter.run_plain(
            instructions.instruction_table(),
            instructions.gas_table(),
            context,
        );
        if let Err(error) = std::mem::replace(&mut context.error, Ok(())) {
            return Err(invalid(error));
        }
        let next = frame
            .process_next_action::<_, ContextError<crate::ForkError>>(context, action)
            .map_err(invalid)?;
        let result = match next {
            ItemOrResult::Item(init) => {
                if frames.len() >= 64 || init.depth >= 1024 {
                    return Err(SessionError::InvalidCommand(
                        "unsupported_evaluation: nested depth bound exceeded".to_owned(),
                    ));
                }
                if let FrameInput::Call(inputs) = &init.frame_input {
                    if matches!(inputs.bytecode_address, CHEATCODE_ADDRESS | CONSOLE_ADDRESS) {
                        return Err(SessionError::InvalidCommand(
                            "unsupported_evaluation: host callbacks are unavailable".to_owned(),
                        ));
                    }
                }
                let mut child = EthFrame::default();
                match EthFrame::init_with_context(
                    OutFrame::new_init(&mut child),
                    context,
                    &mut precompiles,
                    init,
                )
                .map_err(invalid)?
                {
                    ItemOrResult::Item(_) => {
                        frames.push(child);
                        continue;
                    }
                    ItemOrResult::Result(result) => result,
                }
            }
            ItemOrResult::Result(result) => {
                frames.pop();
                result
            }
        };
        if let Some(parent) = frames.last_mut() {
            parent
                .return_result::<_, ContextError<crate::ForkError>>(context, result)
                .map_err(invalid)?;
        } else {
            return Ok(result.into_interpreter_result());
        }
    }
}

fn execute_opcode(
    context: &mut ChasmContext,
    interpreter: &mut Interpreter<EthInterpreter>,
    opcode: u8,
    arguments: Vec<U256>,
    outputs: usize,
) -> Result<OpcodeExecution, SessionError> {
    if outputs > 1 {
        return Err(SessionError::InvalidCommand(
            "opcode execution supports at most one output".to_owned(),
        ));
    }
    let input = Bytes::copy_from_slice(interpreter.input.input().as_bytes(context).as_ref());
    let live_memory = interpreter.memory.context_memory().to_vec();
    let mut memory = SharedMemory::new();
    memory.resize(live_memory.len());
    memory.set(0, &live_memory);
    let mut nested = Interpreter::new(
        memory,
        ExtBytecode::new(Bytecode::new_raw(Bytes::copy_from_slice(
            interpreter.bytecode.bytecode_slice(),
        ))),
        InputsImpl {
            target_address: interpreter.input.target_address(),
            bytecode_address: interpreter.input.bytecode_address().copied(),
            caller_address: interpreter.input.caller_address(),
            input: CallInput::Bytes(input),
            call_value: interpreter.input.call_value(),
            depth: interpreter.input.depth(),
        },
        interpreter.runtime_flag.is_static,
        context.cfg.spec,
        interpreter.gas.remaining(),
    );
    nested.return_data = interpreter.return_data.clone();
    nested.stack.data_mut().extend(arguments.into_iter().rev());

    let original = context.clone();
    context.journal_mut().checkpoint();
    let instructions =
        EthInstructions::<EthInterpreter, ChasmContext>::new_mainnet_with_spec(context.cfg.spec);
    let result = (|| {
        let static_gas = instructions.gas_table()[opcode as usize] as u64;
        if !nested.gas.record_regular_cost(static_gas) {
            return Err(SessionError::InvalidCommand(
                "opcode execution ran out of gas".to_owned(),
            ));
        }
        instructions.instruction_table()[opcode as usize]
            .execute(InstructionContext {
                interpreter: &mut nested,
                host: context,
            })
            .map_err(|error| SessionError::InvalidCommand(format!("{error:?}")))?;
        if let Err(error) = std::mem::replace(&mut context.error, Ok(())) {
            return Err(SessionError::InvalidCommand(format!("{error:?}")));
        }
        if nested.bytecode.action().is_some() {
            return Err(SessionError::InvalidCommand(
                "opcode execution cannot enter or end a frame".to_owned(),
            ));
        }
        let value = if outputs == 0 {
            None
        } else {
            Some(*nested.stack.data().last().ok_or_else(|| {
                SessionError::InvalidCommand("opcode produced no result".to_owned())
            })?)
        };
        Ok(OpcodeExecution {
            value,
            gas_used: nested.gas.total_gas_spent(),
        })
    })();
    if result.is_ok() {
        context.journal_mut().checkpoint_commit();
        let nested_memory = nested.memory.context_memory();
        let memory_len = live_memory.len().max(nested_memory.len());
        interpreter.memory.resize(memory_len);
        interpreter.memory.set(0, &live_memory);
        interpreter.memory.set(0, &nested_memory);
    } else {
        *context = original;
    }
    result
}

impl Inspector<ChasmContext> for ChasmInspector {
    fn frame_start(
        &mut self,
        context: &mut ChasmContext,
        frame_input: &mut FrameInput,
    ) -> Option<revm::handler::FrameResult> {
        let depth = self.frames.len();
        let frame = match frame_input {
            FrameInput::Call(inputs) => FrameContext {
                depth,
                kind: match inputs.scheme {
                    CallScheme::Call => FrameKind::Call,
                    CallScheme::CallCode => FrameKind::CallCode,
                    CallScheme::DelegateCall => FrameKind::DelegateCall,
                    CallScheme::StaticCall => FrameKind::StaticCall,
                },
                address: inputs.target_address,
                code_address: inputs.bytecode_address,
                caller: inputs.caller,
                value: inputs.call_value(),
                calldata: inputs.input.bytes(context),
                is_static: inputs.is_static,
                pc: 0,
                opcode: 0,
                gas_limit: inputs.gas_limit,
                gas_remaining: inputs.gas_limit,
                code: Bytes::new(),
            },
            FrameInput::Create(inputs) => FrameContext {
                depth,
                kind: match inputs.scheme() {
                    CreateScheme::Create | CreateScheme::Custom { .. } => FrameKind::Create,
                    CreateScheme::Create2 { .. } => FrameKind::Create2,
                },
                address: Address::ZERO,
                code_address: Address::ZERO,
                caller: inputs.caller(),
                value: inputs.value(),
                calldata: inputs.init_code().clone(),
                is_static: false,
                pc: 0,
                opcode: 0,
                gas_limit: inputs.gas_limit(),
                gas_remaining: inputs.gas_limit(),
                code: inputs.init_code().clone(),
            },
            FrameInput::Empty => return None,
        };
        self.frames.push(frame);
        self.origin_restore.push(None);
        None
    }

    fn initialize_interp(
        &mut self,
        interpreter: &mut Interpreter<EthInterpreter>,
        _context: &mut ChasmContext,
    ) {
        if let Some(frame) = self.frames.last_mut() {
            Self::update_frame(frame, interpreter);
        }
    }

    fn frame_end(
        &mut self,
        context: &mut ChasmContext,
        _frame_input: &FrameInput,
        _frame_result: &mut revm::handler::FrameResult,
    ) {
        self.frames.pop();
        if let Some(Some(origin)) = self.origin_restore.pop() {
            context.tx.caller = origin;
            self.origin = origin;
        }
    }

    fn step(&mut self, interpreter: &mut Interpreter<EthInterpreter>, context: &mut ChasmContext) {
        self.step_index = self.step_index.saturating_add(1);
        if let Some(frame) = self.frames.last_mut() {
            Self::update_frame(frame, interpreter);
        }
        let address = interpreter.input.target_address();
        let pc = interpreter.bytecode.pc();
        let mut paused_here = false;
        if self.skip_breakpoint_once {
            self.skip_breakpoint_once = false;
        } else if self.breakpoints.contains(&(address, pc))
            || self.breakpoints.contains(&(Address::ZERO, pc))
        {
            self.pause(interpreter, context, PauseReason::Breakpoint);
            paused_here = true;
        }
        if !paused_here && interpreter.bytecode.opcode() == revm::bytecode::opcode::REVERT {
            self.pause(interpreter, context, PauseReason::Revert);
            paused_here = true;
        }
        if !paused_here && let Some(remaining) = self.steps_until_pause {
            if remaining <= 1 {
                self.steps_until_pause = None;
                self.pause(interpreter, context, PauseReason::Step);
            } else {
                self.steps_until_pause = Some(remaining - 1);
            }
        }

        self.backup = Some(StepBackup {
            pc: interpreter.bytecode.pc(),
            stack: interpreter.stack.data().to_vec(),
            memory: interpreter.memory.context_memory().to_vec(),
            gas: interpreter.gas,
        });
        self.instruction_checkpoint = Some(context.journal_mut().checkpoint());
    }

    fn step_end(
        &mut self,
        interpreter: &mut Interpreter<EthInterpreter>,
        context: &mut ChasmContext,
    ) {
        let result = interpreter
            .bytecode
            .action
            .as_ref()
            .and_then(InterpreterAction::instruction_result);
        if !self.strict && result.is_some_and(Self::is_out_of_gas) {
            let checkpoint = self
                .instruction_checkpoint
                .take()
                .expect("step must open an instruction checkpoint");
            context.journal_mut().checkpoint_revert(checkpoint);
            let backup = self
                .backup
                .take()
                .expect("step must capture interpreter state");
            interpreter.stack.data_mut().clear();
            interpreter
                .stack
                .data_mut()
                .extend_from_slice(&backup.stack);
            interpreter.memory.resize(backup.memory.len());
            interpreter.memory.set(0, &backup.memory);
            interpreter.gas = backup.gas;
            interpreter.bytecode.absolute_jump(backup.pc);
            interpreter.bytecode.action = None;
            interpreter.bytecode.reset_action();

            if self.pause(interpreter, context, PauseReason::OutOfGas) {
                self.skip_breakpoint_once = true;
            } else {
                interpreter.halt(result.expect("checked above"));
            }
            return;
        }

        if self.instruction_checkpoint.take().is_some() {
            context.journal_mut().checkpoint_commit();
        }
        self.backup = None;
    }

    fn call(&mut self, context: &mut ChasmContext, inputs: &mut CallInputs) -> Option<CallOutcome> {
        if !self.strict && matches!(inputs.bytecode_address, CHEATCODE_ADDRESS | CONSOLE_ADDRESS) {
            return Some(self.host_call(context, inputs));
        }
        self.apply_prank(context, inputs);
        None
    }
}

pub struct DebugEngine {
    commands: Sender<Command>,
    events: Receiver<DebugEvent>,
    state: Arc<AtomicU8>,
    worker: Option<JoinHandle<()>>,
    backing: RemoteDb,
}

impl DebugEngine {
    pub fn start(config: SessionConfig) -> Self {
        let transaction = TransactionRequest {
            caller: config.caller,
            kind: TransactionKind::Call(config.entry),
            gas_limit: config.gas_limit,
            value: U256::ZERO,
            data: Bytes::new(),
            commit: true,
        };
        let engine = Self::new(ChainConfig {
            accounts: config.accounts,
            breakpoints: config.breakpoints,
        });
        engine
            .transact(transaction)
            .expect("a newly created REVM worker must accept its first transaction");
        engine
    }

    pub fn new(config: ChainConfig) -> Self {
        Self::spawn(config, RemoteDb::default(), None)
    }

    pub fn fork(config: ForkConfig) -> Result<Self, SessionError> {
        let invalid = |error: crate::ForkError| SessionError::InvalidCommand(error.to_string());
        let backing = RemoteDb::new(&config).map_err(invalid)?;
        let block = config.block_env().map_err(invalid)?;
        Ok(Self::spawn(ChainConfig::default(), backing, Some(block)))
    }

    pub fn export_fork(&self) -> Result<String, SessionError> {
        self.backing
            .export()
            .map_err(|error| SessionError::InvalidCommand(error.to_string()))
    }

    pub fn transact_json(&self, request: &str) -> Result<(), SessionError> {
        let (tx, commit) = transaction_from_json(request)
            .map_err(|error| SessionError::InvalidCommand(error.to_string()))?;
        self.submit(EngineCommand::TransactEnv(tx, commit))
    }

    pub fn apply_beacon_root(&self, root: Bytes) -> Result<(), SessionError> {
        if root.len() != 32 {
            return Err(SessionError::InvalidCommand(
                "beacon root must contain 32 bytes".to_owned(),
            ));
        }
        if self.state.load(Ordering::Acquire) != STATE_IDLE {
            return Err(SessionError::InvalidCommand(
                "beacon root requires idle engine".to_owned(),
            ));
        }
        self.command(EngineCommand::BeaconRoot(root)).map(|_| ())
    }

    fn spawn(config: ChainConfig, backing: RemoteDb, block: Option<BlockEnv>) -> Self {
        let (command_tx, command_rx) = unbounded();
        let (event_tx, event_rx) = unbounded();
        let state = Arc::new(AtomicU8::new(STATE_IDLE));
        let worker_state = Arc::clone(&state);
        let worker_events = event_tx.clone();
        let worker_backing = backing.clone();
        let worker = std::thread::spawn(move || {
            if let Err(error) = run_worker(
                config,
                worker_backing,
                block,
                command_rx,
                event_tx,
                &worker_state,
            ) {
                let _ = worker_events.send(DebugEvent::Failed(error));
            }
            worker_state.store(STATE_STOPPED, Ordering::Release);
        });
        Self {
            commands: command_tx,
            events: event_rx,
            state,
            worker: Some(worker),
            backing,
        }
    }

    pub fn transact(&self, transaction: TransactionRequest) -> Result<(), SessionError> {
        self.submit(EngineCommand::Transact(transaction))
    }

    fn submit(&self, action: EngineCommand) -> Result<(), SessionError> {
        self.state
            .compare_exchange(
                STATE_IDLE,
                STATE_RUNNING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|state| match state {
                STATE_RUNNING | STATE_PAUSED => {
                    SessionError::InvalidCommand("a transaction is already running".to_owned())
                }
                _ => SessionError::EngineStopped,
            })?;
        match self.command(action) {
            Ok(_) => Ok(()),
            Err(error) => {
                self.state.store(STATE_IDLE, Ordering::Release);
                Err(error)
            }
        }
    }

    pub fn set_breakpoints(
        &self,
        breakpoints: Vec<crate::Breakpoint>,
    ) -> Result<usize, SessionError> {
        match self.state.load(Ordering::Acquire) {
            STATE_IDLE | STATE_PAUSED => {}
            STATE_RUNNING => {
                return Err(SessionError::InvalidCommand(
                    "cannot change breakpoints while a transaction is running".to_owned(),
                ));
            }
            _ => return Err(SessionError::EngineStopped),
        }
        match self.command(EngineCommand::SetBreakpoints(breakpoints))? {
            CommandValue::Number(value) => Ok(value as usize),
            _ => unreachable!(),
        }
    }

    pub fn wait(&self, timeout: Duration) -> Result<DebugEvent, SessionError> {
        match self.events.recv_timeout(timeout) {
            Ok(event) => Ok(event),
            Err(RecvTimeoutError::Timeout) => Err(SessionError::Timeout(timeout)),
            Err(RecvTimeoutError::Disconnected) => Err(SessionError::EngineStopped),
        }
    }

    pub fn snapshot(&self) -> Result<Snapshot, SessionError> {
        match self.execute(DebugCommand::Snapshot)? {
            CommandValue::Snapshot(snapshot) => Ok(*snapshot),
            _ => unreachable!(),
        }
    }

    pub fn set_stack(&self, index: usize, value: U256) -> Result<U256, SessionError> {
        match self.execute(DebugCommand::SetStack { index, value })? {
            CommandValue::Word(value) => Ok(value),
            _ => unreachable!(),
        }
    }

    pub fn write_memory(&self, offset: usize, data: Bytes) -> Result<usize, SessionError> {
        match self.execute(DebugCommand::WriteMemory { offset, data })? {
            CommandValue::Number(value) => Ok(value as usize),
            _ => unreachable!(),
        }
    }

    pub fn set_gas(&self, value: u64) -> Result<u64, SessionError> {
        match self.execute(DebugCommand::SetGas(value))? {
            CommandValue::Number(value) => Ok(value),
            _ => unreachable!(),
        }
    }

    pub fn set_pc(&self, value: usize) -> Result<usize, SessionError> {
        match self.execute(DebugCommand::SetPc(value))? {
            CommandValue::Number(value) => Ok(value as usize),
            _ => unreachable!(),
        }
    }

    pub fn read_storage(&self, key: U256) -> Result<U256, SessionError> {
        match self.execute(DebugCommand::ReadStorage(key))? {
            CommandValue::Word(value) => Ok(value),
            _ => unreachable!(),
        }
    }

    pub fn write_storage(&self, key: U256, value: U256) -> Result<U256, SessionError> {
        match self.execute(DebugCommand::WriteStorage { key, value })? {
            CommandValue::Word(value) => Ok(value),
            _ => unreachable!(),
        }
    }

    pub fn state(&self, command: StateCommand) -> Result<CommandValue, SessionError> {
        if self.state.load(Ordering::Acquire) == STATE_IDLE {
            self.command(EngineCommand::Debug(DebugCommand::State(command)))
        } else {
            self.execute(DebugCommand::State(command))
        }
    }

    pub fn set_prank(&self, prank: Option<PrankConfig>) -> Result<(), SessionError> {
        self.execute(DebugCommand::SetPrank(prank)).map(|_| ())
    }

    pub fn evaluate(&self, code: Bytes, keep: bool) -> Result<Bytes, SessionError> {
        match self.execute(DebugCommand::Evaluate { code, keep })? {
            CommandValue::Bytes(value) => Ok(value),
            _ => unreachable!(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn evaluate_call(
        &self,
        code: Bytes,
        data: Bytes,
        caller: Address,
        value: U256,
        gas_limit: u64,
        keep: bool,
    ) -> Result<Evaluation, SessionError> {
        match self.execute(DebugCommand::EvaluateCall {
            code,
            data,
            caller,
            value,
            gas_limit,
            keep,
        })? {
            CommandValue::Evaluation(value) => Ok(value),
            _ => unreachable!(),
        }
    }

    pub fn execute_opcode(
        &self,
        opcode: u8,
        arguments: Vec<U256>,
        outputs: usize,
    ) -> Result<OpcodeExecution, SessionError> {
        match self.execute(DebugCommand::ExecuteOpcode {
            opcode,
            arguments,
            outputs,
        })? {
            CommandValue::OpcodeExecution(value) => Ok(value),
            _ => unreachable!(),
        }
    }

    pub fn resume(&self) -> Result<(), SessionError> {
        self.execute(DebugCommand::Resume).map(|_| ())
    }

    pub fn step(&self, count: usize) -> Result<(), SessionError> {
        self.execute(DebugCommand::Step { count }).map(|_| ())
    }

    pub fn respond_host(&self, output: Bytes, revert: bool) -> Result<(), SessionError> {
        self.execute(DebugCommand::RespondHost { output, revert })
            .map(|_| ())
    }

    pub fn execute(&self, command: DebugCommand) -> Result<CommandValue, SessionError> {
        if self.state.load(Ordering::Acquire) != STATE_PAUSED {
            return Err(SessionError::NotPaused);
        }
        self.command(EngineCommand::Debug(command))
    }

    fn command(&self, action: EngineCommand) -> Result<CommandValue, SessionError> {
        let (reply_tx, reply_rx) = bounded(1);
        self.commands
            .send(Command {
                action,
                reply: reply_tx,
            })
            .map_err(|_| SessionError::EngineStopped)?;
        match reply_rx.recv_timeout(COMMAND_TIMEOUT) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => Err(SessionError::Timeout(COMMAND_TIMEOUT)),
            Err(RecvTimeoutError::Disconnected) => Err(SessionError::EngineStopped),
        }
    }
}

impl Drop for DebugEngine {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            let (reply, _) = bounded(1);
            let _ = self.commands.send(Command {
                action: EngineCommand::Abort,
                reply,
            });
            let _ = worker.join();
        }
    }
}

pub type PrototypeSession = DebugEngine;

fn run_worker(
    config: ChainConfig,
    backing: RemoteDb,
    block: Option<BlockEnv>,
    commands: Receiver<Command>,
    events: Sender<DebugEvent>,
    state: &Arc<AtomicU8>,
) -> Result<(), String> {
    let strict = backing.is_fork();
    let mut db = CacheDB::new(backing);
    for account in config.accounts {
        db.insert_account_info(
            account.address,
            AccountInfo {
                balance: account.balance,
                nonce: account.nonce,
                code: Some(Bytecode::new_raw(account.code)),
                ..Default::default()
            },
        );
        for (key, value) in account.storage {
            db.insert_account_storage(account.address, key, value)
                .map_err(|error| format!("{error:?}"))?;
        }
    }
    let inspector = ChasmInspector {
        commands: commands.clone(),
        events: events.clone(),
        breakpoints: config
            .breakpoints
            .into_iter()
            .map(|point| (point.address, point.pc))
            .collect(),
        frames: Vec::new(),
        origin: Address::ZERO,
        step_index: 0,
        backup: None,
        instruction_checkpoint: None,
        skip_breakpoint_once: false,
        prank: None,
        origin_restore: Vec::new(),
        state: Arc::clone(state),
        abort_requested: false,
        steps_until_pause: None,
        strict,
    };
    let context = Context::mainnet()
        .with_db(db)
        .with_block(block.unwrap_or_default())
        .modify_cfg_chained(|cfg| {
            cfg.set_spec_and_mainnet_gas_params(SpecId::CANCUN);
            cfg.chain_id = if strict { 1 } else { cfg.chain_id };
            cfg.disable_nonce_check = !strict;
        });
    let mut evm = context.build_mainnet_with_inspector(inspector);

    while let Ok(command) = commands.recv() {
        let action = match command.action {
            EngineCommand::Transact(transaction) => {
                let commit = transaction.commit;
                match transaction_env(transaction) {
                    Ok(tx) => EngineCommand::TransactEnv(tx, commit),
                    Err(error) => {
                        state.store(STATE_IDLE, Ordering::Release);
                        let _ = command.reply.send(Err(SessionError::InvalidCommand(error)));
                        continue;
                    }
                }
            }
            other => other,
        };
        match action {
            EngineCommand::TransactEnv(tx, commit) => {
                evm.inspector.origin = tx.caller;
                let original = evm.ctx.clone();
                evm.inspector.abort_requested = false;
                let _ = command.reply.send(Ok(CommandValue::None));
                let outcome = evm.inspect_tx(tx).map_err(|error| format!("{error:?}"));
                if evm.inspector.abort_requested {
                    break;
                }
                state.store(STATE_IDLE, Ordering::Release);
                match outcome {
                    Ok(outcome) => {
                        let finished = finished_event(&outcome.result, &outcome.state);
                        if commit {
                            evm.commit(outcome.state);
                        } else {
                            evm.ctx = original;
                        }
                        events
                            .send(DebugEvent::Finished(finished))
                            .map_err(|_| "debug event receiver closed".to_owned())?;
                    }
                    Err(error) => {
                        evm.ctx = original;
                        events
                            .send(DebugEvent::Failed(error))
                            .map_err(|_| "debug event receiver closed".to_owned())?;
                    }
                }
            }
            EngineCommand::BeaconRoot(root) => {
                let original = evm.ctx.clone();
                // REVM's default system gas includes post-Cancun reservoir gas; EIP-4788 uses exactly 30M.
                evm.ctx.tx = TxEnv {
                    caller: SYSTEM_ADDRESS,
                    kind: TxKind::Call(BEACON_ROOTS_ADDRESS),
                    data: root,
                    gas_limit: 30_000_000,
                    ..Default::default()
                };
                let result = MainnetHandler::default().run_system_call(&mut evm).map_err(
                    |error: revm::context::result::EVMError<crate::ForkError>| {
                        SessionError::InvalidCommand(format!("{error:?}"))
                    },
                );
                match result {
                    Ok(_) => {
                        let changes = evm.finalize();
                        evm.commit(changes);
                        evm.ctx.tx = original.tx;
                        let _ = command.reply.send(Ok(CommandValue::None));
                    }
                    Err(error) => {
                        evm.ctx = original;
                        let _ = command.reply.send(Err(error));
                    }
                }
            }
            EngineCommand::Transact(_) => unreachable!(),
            EngineCommand::Debug(DebugCommand::State(state_command)) => {
                let result = execute_state_command(&mut evm.ctx, state_command);
                let _ = command.reply.send(result);
            }
            EngineCommand::Debug(_) => {
                let _ = command.reply.send(Err(SessionError::NotPaused));
            }
            EngineCommand::SetBreakpoints(breakpoints) => {
                evm.inspector.breakpoints = breakpoints
                    .into_iter()
                    .map(|point| (point.address, point.pc))
                    .collect();
                let _ = command.reply.send(Ok(CommandValue::Number(
                    evm.inspector.breakpoints.len() as u64,
                )));
            }
            EngineCommand::Abort => {
                let _ = command.reply.send(Ok(CommandValue::None));
                break;
            }
        }
    }
    Ok(())
}

fn transaction_env(request: TransactionRequest) -> Result<TxEnv, String> {
    let kind = match request.kind {
        TransactionKind::Call(address) => TxKind::Call(address),
        TransactionKind::Create => TxKind::Create,
    };
    TxEnv::builder()
        .caller(request.caller)
        .kind(kind)
        .gas_limit(request.gas_limit)
        .value(request.value)
        .data(request.data)
        .build()
        .map_err(|error| format!("{error:?}"))
}

fn finished_event(
    result: &revm::context::result::ExecutionResult,
    state: &revm::state::EvmState,
) -> Finished {
    let storage = state
        .iter()
        .flat_map(|(address, account)| {
            account.storage.iter().map(|(key, value)| StorageSlot {
                address: *address,
                key: *key,
                value: value.present_value,
            })
        })
        .collect();
    Finished {
        success: result.is_success(),
        gas_used: result.tx_gas_used(),
        output: result.output().cloned().unwrap_or_default(),
        created_address: result.created_address(),
        storage,
        logs: result
            .logs()
            .iter()
            .map(|log| LogEntry {
                address: log.address,
                topics: log.topics().to_vec(),
                data: log.data.data.clone(),
            })
            .collect(),
    }
}
