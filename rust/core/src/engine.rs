use crate::protocol::{
    CommandValue, DebugEvent, Finished, PauseReason, SessionConfig, SessionError, Snapshot,
    StorageSlot,
};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, bounded, unbounded};
use revm::{
    Context, InspectEvm, Inspector, MainBuilder, MainContext,
    context::{ContextTr, TxEnv},
    context_interface::JournalTr,
    database::InMemoryDB,
    handler::instructions::EthInstructions,
    interpreter::{
        CallInput, CallInputs, CallOutcome, Gas, InstructionResult, Interpreter, InterpreterAction,
        InterpreterResult,
        interpreter::{EthInterpreter, ExtBytecode, InputsImpl, SharedMemory},
        interpreter_types::{InputsTr, Jumps, LegacyBytecode, LoopControl},
    },
    primitives::{Address, Bytes, TxKind, U256, address, hardfork::SpecId, keccak256},
    state::{AccountInfo, Bytecode},
};
use std::{collections::HashSet, thread::JoinHandle, time::Duration};

pub const DEFAULT_TARGET: Address = address!("1000000000000000000000000000000000000001");
pub const DEFAULT_CALLER: Address = address!("2000000000000000000000000000000000000002");
pub const CHEATCODE_ADDRESS: Address = address!("7109709ECfa91a80626fF3989D68f67F5b1DD12D");

const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

enum Action {
    Snapshot,
    SetStack { index: usize, value: U256 },
    WriteMemory { offset: usize, data: Bytes },
    SetGas(u64),
    SetPc(usize),
    ReadStorage(U256),
    WriteStorage { key: U256, value: U256 },
    Evaluate { code: Bytes, keep: bool },
    Resume,
    Abort,
}

struct Command {
    action: Action,
    reply: Sender<Result<CommandValue, SessionError>>,
}

#[derive(Clone)]
struct StepBackup {
    pc: usize,
    stack: Vec<U256>,
    memory: Vec<u8>,
    gas: Gas,
}

struct SevmInspector {
    commands: Receiver<Command>,
    events: Sender<DebugEvent>,
    breakpoints: HashSet<(Address, usize)>,
    depth: usize,
    backup: Option<StepBackup>,
    instruction_checkpoint: Option<revm::context_interface::journaled_state::JournalCheckpoint>,
    skip_breakpoint_once: bool,
    next_prank: Option<Address>,
}

impl SevmInspector {
    fn snapshot(&self, interpreter: &Interpreter<EthInterpreter>, reason: PauseReason) -> Snapshot {
        Snapshot {
            reason,
            address: interpreter.input.target_address(),
            depth: self.depth,
            pc: interpreter.bytecode.pc(),
            opcode: interpreter.bytecode.opcode(),
            gas_remaining: interpreter.gas.remaining(),
            stack: interpreter.stack.data().iter().rev().copied().collect(),
            memory: Bytes::copy_from_slice(&interpreter.memory.context_memory()),
        }
    }

    fn pause<CTX>(
        &mut self,
        interpreter: &mut Interpreter<EthInterpreter>,
        context: &mut CTX,
        reason: PauseReason,
    ) -> bool
    where
        CTX: ContextTr + revm::interpreter::Host,
        CTX::Journal: JournalTr,
        <CTX::Journal as JournalTr>::Database: revm::Database,
    {
        if self
            .events
            .send(DebugEvent::Paused(self.snapshot(interpreter, reason)))
            .is_err()
        {
            return false;
        }

        let mut gas_changed = false;
        while let Ok(command) = self.commands.recv() {
            let mut resume = false;
            let result = match command.action {
                Action::Snapshot => Ok(CommandValue::Snapshot(self.snapshot(interpreter, reason))),
                Action::SetStack { index, value } => interpreter
                    .stack
                    .set(index, value)
                    .map(|()| CommandValue::Word(value))
                    .map_err(|error| SessionError::InvalidCommand(format!("{error:?}"))),
                Action::WriteMemory { offset, data } => {
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
                Action::SetGas(value) => {
                    interpreter.gas.set_remaining(value);
                    gas_changed = true;
                    Ok(CommandValue::Number(value))
                }
                Action::SetPc(value) => {
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
                Action::ReadStorage(key) => {
                    let checkpoint = context.journal_mut().checkpoint();
                    let result = context
                        .journal_mut()
                        .sload(interpreter.input.target_address(), key)
                        .map(|value| CommandValue::Word(value.data))
                        .map_err(|error| SessionError::InvalidCommand(format!("{error:?}")));
                    context.journal_mut().checkpoint_revert(checkpoint);
                    result
                }
                Action::WriteStorage { key, value } => context
                    .journal_mut()
                    .sstore(interpreter.input.target_address(), key, value)
                    .map(|stored| CommandValue::Word(stored.data.present_value))
                    .map_err(|error| SessionError::InvalidCommand(format!("{error:?}"))),
                Action::Evaluate { code, keep } => {
                    let checkpoint = context.journal_mut().checkpoint();
                    let mut nested = Interpreter::new(
                        SharedMemory::new(),
                        ExtBytecode::new(Bytecode::new_raw(code)),
                        InputsImpl {
                            target_address: interpreter.input.target_address(),
                            bytecode_address: interpreter.input.bytecode_address().copied(),
                            caller_address: interpreter.input.caller_address(),
                            input: CallInput::default(),
                            call_value: U256::ZERO,
                            depth: interpreter.input.depth() + 1,
                        },
                        false,
                        SpecId::CANCUN,
                        100_000,
                    );
                    let instructions =
                        EthInstructions::<EthInterpreter, CTX>::new_mainnet_with_spec(
                            SpecId::CANCUN,
                        );
                    let action = nested.run_plain(
                        instructions.instruction_table(),
                        instructions.gas_table(),
                        context,
                    );
                    let result = action.into_result_return().ok_or_else(|| {
                        SessionError::InvalidCommand(
                            "speculative execution suspended on a nested call".to_owned(),
                        )
                    });
                    let result = match result {
                        Ok(result) if result.result.is_ok() => {
                            Ok(CommandValue::Bytes(result.output))
                        }
                        Ok(result) => Err(SessionError::InvalidCommand(format!(
                            "speculative execution halted: {:?}",
                            result.result
                        ))),
                        Err(error) => Err(error),
                    };
                    if keep && result.is_ok() {
                        context.journal_mut().checkpoint_commit();
                    } else {
                        context.journal_mut().checkpoint_revert(checkpoint);
                    }
                    result
                }
                Action::Resume => {
                    resume = true;
                    Ok(CommandValue::None)
                }
                Action::Abort => {
                    interpreter.halt(InstructionResult::Stop);
                    resume = true;
                    Ok(CommandValue::None)
                }
            };
            let _ = command.reply.send(result);
            if resume {
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
}

impl<CTX> Inspector<CTX> for SevmInspector
where
    CTX: ContextTr + revm::interpreter::Host,
    CTX::Journal: JournalTr,
    <CTX::Journal as JournalTr>::Database: revm::Database,
{
    fn frame_start(
        &mut self,
        _context: &mut CTX,
        _frame_input: &mut revm::interpreter::FrameInput,
    ) -> Option<revm::handler::FrameResult> {
        self.depth += 1;
        None
    }

    fn frame_end(
        &mut self,
        _context: &mut CTX,
        _frame_input: &revm::interpreter::FrameInput,
        _frame_result: &mut revm::handler::FrameResult,
    ) {
        self.depth = self.depth.saturating_sub(1);
    }

    fn step(&mut self, interpreter: &mut Interpreter<EthInterpreter>, context: &mut CTX) {
        let address = interpreter.input.target_address();
        let pc = interpreter.bytecode.pc();
        if self.skip_breakpoint_once {
            self.skip_breakpoint_once = false;
        } else if self.breakpoints.contains(&(address, pc)) {
            self.pause(interpreter, context, PauseReason::Breakpoint);
        }

        self.backup = Some(StepBackup {
            pc: interpreter.bytecode.pc(),
            stack: interpreter.stack.data().to_vec(),
            memory: interpreter.memory.context_memory().to_vec(),
            gas: interpreter.gas,
        });
        self.instruction_checkpoint = Some(context.journal_mut().checkpoint());
    }

    fn step_end(&mut self, interpreter: &mut Interpreter<EthInterpreter>, context: &mut CTX) {
        let result = interpreter
            .bytecode
            .action
            .as_ref()
            .and_then(InterpreterAction::instruction_result);
        if result.is_some_and(Self::is_out_of_gas) {
            let checkpoint = self
                .instruction_checkpoint
                .take()
                .expect("step must open an instruction checkpoint");
            context.journal_mut().checkpoint_revert(checkpoint);
            let backup = self
                .backup
                .take()
                .expect("step must capture interpreter state");
            *interpreter.stack.data_mut() = backup.stack;
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

    fn call(&mut self, context: &mut CTX, inputs: &mut CallInputs) -> Option<CallOutcome> {
        if inputs.bytecode_address == CHEATCODE_ADDRESS {
            let data = inputs.input.bytes(context);
            let selector = &keccak256("prank(address)")[..4];
            if data.len() == 36 && data[..4] == *selector {
                self.next_prank = Some(Address::from_slice(&data[16..36]));
                return Some(CallOutcome::new(
                    InterpreterResult::new(
                        InstructionResult::Return,
                        Bytes::new(),
                        Gas::new(inputs.gas_limit),
                    ),
                    inputs.return_memory_offset.clone(),
                ));
            }
        } else if let Some(caller) = self.next_prank.take() {
            inputs.caller = caller;
        }
        None
    }
}

pub struct PrototypeSession {
    commands: Sender<Command>,
    events: Receiver<DebugEvent>,
    worker: Option<JoinHandle<()>>,
}

impl PrototypeSession {
    pub fn start(config: SessionConfig) -> Self {
        let (command_tx, command_rx) = unbounded();
        let (event_tx, event_rx) = unbounded();
        let worker_events = event_tx.clone();
        let worker = std::thread::spawn(move || {
            if let Err(error) = run_worker(config, command_rx, event_tx) {
                let _ = worker_events.send(DebugEvent::Failed(error));
            }
        });
        Self {
            commands: command_tx,
            events: event_rx,
            worker: Some(worker),
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
        match self.command(Action::Snapshot)? {
            CommandValue::Snapshot(snapshot) => Ok(snapshot),
            _ => unreachable!(),
        }
    }

    pub fn set_stack(&self, index: usize, value: U256) -> Result<U256, SessionError> {
        match self.command(Action::SetStack { index, value })? {
            CommandValue::Word(value) => Ok(value),
            _ => unreachable!(),
        }
    }

    pub fn write_memory(&self, offset: usize, data: Bytes) -> Result<usize, SessionError> {
        match self.command(Action::WriteMemory { offset, data })? {
            CommandValue::Number(value) => Ok(value as usize),
            _ => unreachable!(),
        }
    }

    pub fn set_gas(&self, value: u64) -> Result<u64, SessionError> {
        match self.command(Action::SetGas(value))? {
            CommandValue::Number(value) => Ok(value),
            _ => unreachable!(),
        }
    }

    pub fn set_pc(&self, value: usize) -> Result<usize, SessionError> {
        match self.command(Action::SetPc(value))? {
            CommandValue::Number(value) => Ok(value as usize),
            _ => unreachable!(),
        }
    }

    pub fn read_storage(&self, key: U256) -> Result<U256, SessionError> {
        match self.command(Action::ReadStorage(key))? {
            CommandValue::Word(value) => Ok(value),
            _ => unreachable!(),
        }
    }

    pub fn write_storage(&self, key: U256, value: U256) -> Result<U256, SessionError> {
        match self.command(Action::WriteStorage { key, value })? {
            CommandValue::Word(value) => Ok(value),
            _ => unreachable!(),
        }
    }

    pub fn evaluate(&self, code: Bytes, keep: bool) -> Result<Bytes, SessionError> {
        match self.command(Action::Evaluate { code, keep })? {
            CommandValue::Bytes(value) => Ok(value),
            _ => unreachable!(),
        }
    }

    pub fn resume(&self) -> Result<(), SessionError> {
        self.command(Action::Resume).map(|_| ())
    }

    fn command(&self, action: Action) -> Result<CommandValue, SessionError> {
        let (reply_tx, reply_rx) = bounded(1);
        self.commands
            .send(Command {
                action,
                reply: reply_tx,
            })
            .map_err(|_| SessionError::EngineStopped)?;
        match reply_rx.recv_timeout(COMMAND_TIMEOUT) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => Err(SessionError::NotPaused),
            Err(RecvTimeoutError::Disconnected) => Err(SessionError::EngineStopped),
        }
    }
}

impl Drop for PrototypeSession {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            let (reply, _) = bounded(1);
            let _ = self.commands.send(Command {
                action: Action::Abort,
                reply,
            });
            let _ = worker.join();
        }
    }
}

fn run_worker(
    config: SessionConfig,
    commands: Receiver<Command>,
    events: Sender<DebugEvent>,
) -> Result<(), String> {
    let mut db = InMemoryDB::default();
    for account in config.accounts {
        db.insert_account_info(
            account.address,
            AccountInfo {
                balance: account.balance,
                code: Some(Bytecode::new_raw(account.code)),
                ..Default::default()
            },
        );
        for (key, value) in account.storage {
            db.insert_account_storage(account.address, key, value)
                .map_err(|error| format!("{error:?}"))?;
        }
    }
    db.insert_account_info(
        config.caller,
        AccountInfo {
            balance: U256::MAX,
            ..Default::default()
        },
    );

    let inspector = SevmInspector {
        commands,
        events: events.clone(),
        breakpoints: config
            .breakpoints
            .into_iter()
            .map(|point| (point.address, point.pc))
            .collect(),
        depth: 0,
        backup: None,
        instruction_checkpoint: None,
        skip_breakpoint_once: false,
        next_prank: None,
    };
    let context = Context::mainnet()
        .with_db(db)
        .modify_cfg_chained(|cfg| cfg.set_spec_and_mainnet_gas_params(SpecId::CANCUN));
    let mut evm = context.build_mainnet_with_inspector(inspector);
    let outcome = evm
        .inspect_tx(
            TxEnv::builder()
                .caller(config.caller)
                .kind(TxKind::Call(config.entry))
                .gas_limit(config.gas_limit)
                .build()
                .map_err(|error| format!("{error:?}"))?,
        )
        .map_err(|error| format!("{error:?}"))?;

    let mut storage = Vec::new();
    for (address, account) in outcome.state {
        for (key, value) in account.storage {
            storage.push(StorageSlot {
                address,
                key,
                value: value.present_value,
            });
        }
    }
    let finished = Finished {
        success: outcome.result.is_success(),
        gas_used: outcome.result.tx_gas_used(),
        output: outcome.result.into_output().unwrap_or_default(),
        storage,
    };
    events
        .send(DebugEvent::Finished(finished))
        .map_err(|_| "debug event receiver closed".to_owned())
}
