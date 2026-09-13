use revm::primitives::{Address, B256, Bytes, U256};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sevm_revm_core::{
    AccountSpec, Breakpoint, ChainConfig, CommandValue, DEFAULT_CALLER, DebugCommand, DebugEngine,
    DebugEvent, FrameContext, FrameKind, PauseReason, SessionConfig, SessionError, Snapshot,
    StateCommand, TransactionKind, TransactionRequest,
};
use std::{
    fmt,
    io::{self, BufRead, Write},
    str::FromStr,
    time::Duration,
};

pub const PROTOCOL_VERSION: &str = "sevm-debugger/1";

const PARSE_ERROR: i32 = -32700;
const INVALID_REQUEST: i32 = -32600;
const METHOD_NOT_FOUND: i32 = -32601;
const INVALID_PARAMS: i32 = -32602;
const ENGINE_ERROR: i32 = -32000;
const SESSION_STATE_ERROR: i32 = -32001;

#[derive(Debug, Deserialize)]
struct RpcRequest {
    jsonrpc: String,
    id: Value,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct RpcError {
    code: i32,
    message: String,
}

#[derive(Debug)]
struct ProtocolError {
    code: i32,
    message: String,
}

impl ProtocolError {
    fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(INVALID_PARAMS, message)
    }
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StartParams {
    entry: String,
    #[serde(default)]
    caller: Option<String>,
    #[serde(default = "default_gas_limit")]
    gas_limit: u64,
    accounts: Vec<AccountParams>,
    #[serde(default)]
    breakpoints: Vec<BreakpointParams>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenParams {
    #[serde(default)]
    accounts: Vec<AccountParams>,
    #[serde(default)]
    breakpoints: Vec<BreakpointParams>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TransactParams {
    #[serde(default)]
    caller: Option<String>,
    #[serde(default)]
    to: Option<String>,
    #[serde(default = "empty_bytes")]
    data: String,
    #[serde(default)]
    value: Option<String>,
    #[serde(default = "default_transaction_gas_limit")]
    gas_limit: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AccountParams {
    address: String,
    code: String,
    #[serde(default)]
    balance: Option<String>,
    #[serde(default)]
    storage: Vec<StorageParams>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StorageParams {
    key: String,
    value: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BreakpointParams {
    address: String,
    pc: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BreakpointsParams {
    breakpoints: Vec<BreakpointParams>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitParams {
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StackParams {
    index: usize,
    value: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MemoryParams {
    offset: usize,
    data: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GasParams {
    gas: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PcParams {
    pc: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StepParams {
    #[serde(default = "default_step_count")]
    count: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StorageKeyParams {
    key: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StorageWriteParams {
    key: String,
    value: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EvaluateParams {
    bytecode: String,
    #[serde(default)]
    keep: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HostResponseParams {
    #[serde(default = "empty_bytes")]
    output: String,
    #[serde(default)]
    revert: bool,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum StateParams {
    ReadBalance {
        address: String,
    },
    WriteBalance {
        address: String,
        value: String,
    },
    ReadCode {
        address: String,
    },
    WriteCode {
        address: String,
        code: String,
    },
    ReadNonce {
        address: String,
    },
    WriteNonce {
        address: String,
        value: u64,
    },
    ReadStorage {
        address: String,
        key: String,
    },
    WriteStorage {
        address: String,
        key: String,
        value: String,
    },
    ReadTransient {
        address: String,
        key: String,
    },
    WriteTransient {
        address: String,
        key: String,
        value: String,
    },
    WarmStorage {
        address: String,
        key: String,
    },
    ReadBlockNumber,
    WriteBlockNumber {
        value: String,
    },
    ReadTimestamp,
    WriteTimestamp {
        value: String,
    },
    ReadBaseFee,
    WriteBaseFee {
        value: u64,
    },
    ReadChainId,
    WriteChainId {
        value: u64,
    },
    ReadCoinbase,
    WriteCoinbase {
        value: String,
    },
    ReadPrevrandao,
    WritePrevrandao {
        value: String,
    },
    ReadDifficulty,
    WriteDifficulty {
        value: String,
    },
}

fn default_gas_limit() -> u64 {
    100_000
}

fn default_timeout_ms() -> u64 {
    5_000
}

fn default_step_count() -> usize {
    1
}

fn default_transaction_gas_limit() -> u64 {
    30_000_000
}

fn empty_bytes() -> String {
    "0x".to_owned()
}

pub struct ProtocolServer {
    engine: Option<DebugEngine>,
    persistent: bool,
    shutdown: bool,
}

impl Default for ProtocolServer {
    fn default() -> Self {
        Self::new()
    }
}

impl ProtocolServer {
    pub fn new() -> Self {
        Self {
            engine: None,
            persistent: false,
            shutdown: false,
        }
    }

    pub fn handle(&mut self, request: Value) -> Value {
        let parsed = serde_json::from_value::<RpcRequest>(request);
        let request = match parsed {
            Ok(request)
                if request.jsonrpc == "2.0"
                    && (request.id.is_string() || request.id.is_number()) =>
            {
                request
            }
            _ => return error_response(Value::Null, INVALID_REQUEST, "invalid JSON-RPC request"),
        };
        let id = request.id.clone();
        match self.dispatch(&request.method, request.params) {
            Ok(result) => success_response(id, result),
            Err(error) => error_response(id, error.code, error.message),
        }
    }

    pub fn should_shutdown(&self) -> bool {
        self.shutdown
    }

    fn dispatch(&mut self, method: &str, params: Value) -> Result<Value, ProtocolError> {
        match method {
            "hello" => {
                empty_params(params)?;
                Ok(json!({
                    "protocol": PROTOCOL_VERSION,
                    "transport": "jsonl-stdio",
                    "methods": [
                        "hello",
                        "open",
                        "start",
                        "transact",
                        "wait_event",
                        "snapshot",
                        "set_breakpoints",
                        "set_stack",
                        "write_memory",
                        "set_gas",
                        "set_pc",
                        "step",
                        "read_storage",
                        "write_storage",
                        "state",
                        "evaluate",
                        "respond_host",
                        "resume",
                        "close",
                        "shutdown"
                    ]
                }))
            }
            "open" => self.open(parse_params(params)?),
            "start" => self.start(parse_params(params)?),
            "transact" => self.transact(parse_params(params)?),
            "wait_event" => self.wait_event(parse_params(params)?),
            "snapshot" => {
                empty_params(params)?;
                self.execute(DebugCommand::Snapshot)
            }
            "set_breakpoints" => {
                let params: BreakpointsParams = parse_params(params)?;
                self.engine()?
                    .set_breakpoints(breakpoint_specs(params.breakpoints)?)
                    .map(Value::from)
                    .map_err(engine_error)
            }
            "set_stack" => {
                let params: StackParams = parse_params(params)?;
                self.execute(DebugCommand::SetStack {
                    index: params.index,
                    value: parse_word(&params.value)?,
                })
            }
            "write_memory" => {
                let params: MemoryParams = parse_params(params)?;
                self.execute(DebugCommand::WriteMemory {
                    offset: params.offset,
                    data: decode_bytes(&params.data)?,
                })
            }
            "set_gas" => {
                let params: GasParams = parse_params(params)?;
                self.execute(DebugCommand::SetGas(params.gas))
            }
            "set_pc" => {
                let params: PcParams = parse_params(params)?;
                self.execute(DebugCommand::SetPc(params.pc))
            }
            "step" => {
                let params: StepParams = parse_params(params)?;
                self.execute(DebugCommand::Step {
                    count: params.count,
                })
            }
            "read_storage" => {
                let params: StorageKeyParams = parse_params(params)?;
                self.execute(DebugCommand::ReadStorage(parse_word(&params.key)?))
            }
            "write_storage" => {
                let params: StorageWriteParams = parse_params(params)?;
                self.execute(DebugCommand::WriteStorage {
                    key: parse_word(&params.key)?,
                    value: parse_word(&params.value)?,
                })
            }
            "state" => {
                let params: StateParams = parse_params(params)?;
                self.execute(DebugCommand::State(state_command(params)?))
            }
            "evaluate" => {
                let params: EvaluateParams = parse_params(params)?;
                self.execute(DebugCommand::Evaluate {
                    code: decode_bytes(&params.bytecode)?,
                    keep: params.keep,
                })
            }
            "respond_host" => {
                let params: HostResponseParams = parse_params(params)?;
                self.execute(DebugCommand::RespondHost {
                    output: decode_bytes(&params.output)?,
                    revert: params.revert,
                })
            }
            "resume" => {
                empty_params(params)?;
                self.execute(DebugCommand::Resume)
            }
            "close" => {
                empty_params(params)?;
                self.engine.take();
                self.persistent = false;
                Ok(Value::Null)
            }
            "shutdown" => {
                empty_params(params)?;
                self.engine.take();
                self.persistent = false;
                self.shutdown = true;
                Ok(Value::Null)
            }
            _ => Err(ProtocolError::new(
                METHOD_NOT_FOUND,
                format!("method not found: {method}"),
            )),
        }
    }

    fn start(&mut self, params: StartParams) -> Result<Value, ProtocolError> {
        if self.engine.is_some() {
            return Err(ProtocolError::new(
                SESSION_STATE_ERROR,
                "a debug session is already active",
            ));
        }
        let entry = parse_address(&params.entry)?;
        let caller = params
            .caller
            .as_deref()
            .map(parse_address)
            .transpose()?
            .unwrap_or(DEFAULT_CALLER);
        let accounts = params
            .accounts
            .into_iter()
            .map(account_spec)
            .collect::<Result<Vec<_>, _>>()?;
        if !accounts.iter().any(|account| account.address == entry) {
            return Err(ProtocolError::invalid_params(
                "entry must identify one of the configured accounts",
            ));
        }
        let breakpoints = breakpoint_specs(params.breakpoints)?;
        self.engine = Some(DebugEngine::start(SessionConfig {
            entry,
            caller,
            gas_limit: params.gas_limit,
            accounts,
            breakpoints,
        }));
        self.persistent = false;
        Ok(json!({ "started": true }))
    }

    fn open(&mut self, params: OpenParams) -> Result<Value, ProtocolError> {
        if self.engine.is_some() {
            return Err(ProtocolError::new(
                SESSION_STATE_ERROR,
                "a debug session is already active",
            ));
        }
        self.engine = Some(DebugEngine::new(ChainConfig {
            accounts: params
                .accounts
                .into_iter()
                .map(account_spec)
                .collect::<Result<Vec<_>, _>>()?,
            breakpoints: breakpoint_specs(params.breakpoints)?,
        }));
        self.persistent = true;
        Ok(json!({ "opened": true }))
    }

    fn transact(&self, params: TransactParams) -> Result<Value, ProtocolError> {
        if !self.persistent {
            return Err(ProtocolError::new(
                SESSION_STATE_ERROR,
                "transact requires a persistent session created by open",
            ));
        }
        let kind = params
            .to
            .as_deref()
            .map(parse_address)
            .transpose()?
            .map_or(TransactionKind::Create, TransactionKind::Call);
        self.engine()?
            .transact(TransactionRequest {
                caller: params
                    .caller
                    .as_deref()
                    .map(parse_address)
                    .transpose()?
                    .unwrap_or(DEFAULT_CALLER),
                kind,
                gas_limit: params.gas_limit,
                value: params
                    .value
                    .as_deref()
                    .map(parse_word)
                    .transpose()?
                    .unwrap_or(U256::ZERO),
                data: decode_bytes(&params.data)?,
            })
            .map_err(engine_error)?;
        Ok(json!({ "started": true }))
    }

    fn wait_event(&mut self, params: WaitParams) -> Result<Value, ProtocolError> {
        let event = self
            .engine()?
            .wait(Duration::from_millis(params.timeout_ms))
            .map_err(engine_error)?;
        let terminal = matches!(event, DebugEvent::Finished(_) | DebugEvent::Failed(_));
        let value = event_json(event);
        if terminal && !self.persistent {
            self.engine.take();
        }
        Ok(value)
    }

    fn execute(&self, command: DebugCommand) -> Result<Value, ProtocolError> {
        self.engine()?
            .execute(command)
            .map(command_value_json)
            .map_err(engine_error)
    }

    fn engine(&self) -> Result<&DebugEngine, ProtocolError> {
        self.engine
            .as_ref()
            .ok_or_else(|| ProtocolError::new(SESSION_STATE_ERROR, "no debug session is active"))
    }
}

pub fn serve(reader: impl BufRead, mut writer: impl Write) -> io::Result<()> {
    let mut server = ProtocolServer::new();
    for line in reader.lines() {
        let response = match serde_json::from_str::<Value>(&line?) {
            Ok(request) => server.handle(request),
            Err(error) => error_response(Value::Null, PARSE_ERROR, format!("parse error: {error}")),
        };
        serde_json::to_writer(&mut writer, &response)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
        if server.should_shutdown() {
            break;
        }
    }
    Ok(())
}

fn parse_params<T: DeserializeOwned>(params: Value) -> Result<T, ProtocolError> {
    let params = if params.is_null() { json!({}) } else { params };
    serde_json::from_value(params).map_err(|error| ProtocolError::invalid_params(error.to_string()))
}

fn empty_params(params: Value) -> Result<(), ProtocolError> {
    if params.is_null() || params.as_object().is_some_and(serde_json::Map::is_empty) {
        Ok(())
    } else {
        Err(ProtocolError::invalid_params(
            "this method does not accept parameters",
        ))
    }
}

fn breakpoint_specs(params: Vec<BreakpointParams>) -> Result<Vec<Breakpoint>, ProtocolError> {
    params
        .into_iter()
        .map(|point| {
            Ok(Breakpoint {
                address: parse_address(&point.address)?,
                pc: point.pc,
            })
        })
        .collect()
}

fn account_spec(params: AccountParams) -> Result<AccountSpec, ProtocolError> {
    Ok(AccountSpec {
        address: parse_address(&params.address)?,
        code: decode_bytes(&params.code)?,
        balance: params
            .balance
            .as_deref()
            .map(parse_word)
            .transpose()?
            .unwrap_or(U256::ZERO),
        storage: params
            .storage
            .into_iter()
            .map(|slot| Ok((parse_word(&slot.key)?, parse_word(&slot.value)?)))
            .collect::<Result<Vec<_>, ProtocolError>>()?,
    })
}

fn state_command(params: StateParams) -> Result<StateCommand, ProtocolError> {
    Ok(match params {
        StateParams::ReadBalance { address } => StateCommand::ReadBalance(parse_address(&address)?),
        StateParams::WriteBalance { address, value } => StateCommand::WriteBalance {
            address: parse_address(&address)?,
            value: parse_word(&value)?,
        },
        StateParams::ReadCode { address } => StateCommand::ReadCode(parse_address(&address)?),
        StateParams::WriteCode { address, code } => StateCommand::WriteCode {
            address: parse_address(&address)?,
            code: decode_bytes(&code)?,
        },
        StateParams::ReadNonce { address } => StateCommand::ReadNonce(parse_address(&address)?),
        StateParams::WriteNonce { address, value } => StateCommand::WriteNonce {
            address: parse_address(&address)?,
            value,
        },
        StateParams::ReadStorage { address, key } => StateCommand::ReadStorage {
            address: parse_address(&address)?,
            key: parse_word(&key)?,
        },
        StateParams::WriteStorage {
            address,
            key,
            value,
        } => StateCommand::WriteStorage {
            address: parse_address(&address)?,
            key: parse_word(&key)?,
            value: parse_word(&value)?,
        },
        StateParams::ReadTransient { address, key } => StateCommand::ReadTransient {
            address: parse_address(&address)?,
            key: parse_word(&key)?,
        },
        StateParams::WriteTransient {
            address,
            key,
            value,
        } => StateCommand::WriteTransient {
            address: parse_address(&address)?,
            key: parse_word(&key)?,
            value: parse_word(&value)?,
        },
        StateParams::WarmStorage { address, key } => StateCommand::WarmStorage {
            address: parse_address(&address)?,
            key: parse_word(&key)?,
        },
        StateParams::ReadBlockNumber => StateCommand::ReadBlockNumber,
        StateParams::WriteBlockNumber { value } => {
            StateCommand::WriteBlockNumber(parse_word(&value)?)
        }
        StateParams::ReadTimestamp => StateCommand::ReadTimestamp,
        StateParams::WriteTimestamp { value } => StateCommand::WriteTimestamp(parse_word(&value)?),
        StateParams::ReadBaseFee => StateCommand::ReadBaseFee,
        StateParams::WriteBaseFee { value } => StateCommand::WriteBaseFee(value),
        StateParams::ReadChainId => StateCommand::ReadChainId,
        StateParams::WriteChainId { value } => StateCommand::WriteChainId(value),
        StateParams::ReadCoinbase => StateCommand::ReadCoinbase,
        StateParams::WriteCoinbase { value } => StateCommand::WriteCoinbase(parse_address(&value)?),
        StateParams::ReadPrevrandao => StateCommand::ReadPrevrandao,
        StateParams::WritePrevrandao { value } => {
            StateCommand::WritePrevrandao(parse_hash(&value)?)
        }
        StateParams::ReadDifficulty => StateCommand::ReadDifficulty,
        StateParams::WriteDifficulty { value } => {
            StateCommand::WriteDifficulty(parse_word(&value)?)
        }
    })
}

fn parse_address(value: &str) -> Result<Address, ProtocolError> {
    Address::from_str(value)
        .map_err(|error| ProtocolError::invalid_params(format!("invalid address: {error}")))
}

fn parse_word(value: &str) -> Result<U256, ProtocolError> {
    let (digits, radix) = value
        .strip_prefix("0x")
        .map_or((value, 10), |digits| (digits, 16));
    if digits.is_empty() {
        return Err(ProtocolError::invalid_params("invalid EVM word"));
    }
    U256::from_str_radix(digits, radix)
        .map_err(|error| ProtocolError::invalid_params(format!("invalid EVM word: {error}")))
}

fn parse_hash(value: &str) -> Result<B256, ProtocolError> {
    let bytes = decode_bytes(value)?;
    if bytes.len() != 32 {
        return Err(ProtocolError::invalid_params(
            "EVM hashes must contain 32 bytes",
        ));
    }
    Ok(B256::from_slice(&bytes))
}

fn decode_bytes(value: &str) -> Result<Bytes, ProtocolError> {
    let digits = value.strip_prefix("0x").unwrap_or(value);
    if !digits.len().is_multiple_of(2) {
        return Err(ProtocolError::invalid_params(
            "hex byte strings must contain an even number of digits",
        ));
    }
    let bytes = digits
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = hex_nibble(pair[0])?;
            let low = hex_nibble(pair[1])?;
            Ok((high << 4) | low)
        })
        .collect::<Result<Vec<_>, ProtocolError>>()?;
    Ok(Bytes::from(bytes))
}

fn hex_nibble(value: u8) -> Result<u8, ProtocolError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err(ProtocolError::invalid_params("invalid hex byte string")),
    }
}

fn engine_error(error: SessionError) -> ProtocolError {
    ProtocolError::new(ENGINE_ERROR, error.to_string())
}

fn command_value_json(value: CommandValue) -> Value {
    match value {
        CommandValue::None => Value::Null,
        CommandValue::Snapshot(snapshot) => snapshot_json(*snapshot),
        CommandValue::Word(value) => Value::String(word(value)),
        CommandValue::Number(value) => Value::from(value),
        CommandValue::Bytes(value) => Value::String(hex_bytes(&value)),
    }
}

fn event_json(event: DebugEvent) -> Value {
    match event {
        DebugEvent::Paused(snapshot) => json!({
            "type": "paused",
            "snapshot": snapshot_json(*snapshot),
        }),
        DebugEvent::HostCall(call) => json!({
            "type": "host_call",
            "address": format!("{:#x}", call.address),
            "caller": format!("{:#x}", call.caller),
            "data": hex_bytes(&call.data),
            "gas_limit": call.gas_limit,
        }),
        DebugEvent::Finished(finished) => json!({
            "type": "finished",
            "success": finished.success,
            "gas_used": finished.gas_used,
            "output": hex_bytes(&finished.output),
            "created_address": finished.created_address.map(|address| format!("{address:#x}")),
            "storage": finished.storage.into_iter().map(|slot| json!({
                "address": format!("{:#x}", slot.address),
                "key": word(slot.key),
                "value": word(slot.value),
            })).collect::<Vec<_>>(),
        }),
        DebugEvent::Failed(message) => json!({
            "type": "failed",
            "message": message,
        }),
    }
}

fn snapshot_json(snapshot: Snapshot) -> Value {
    json!({
        "reason": match snapshot.reason {
            PauseReason::Breakpoint => "breakpoint",
            PauseReason::OutOfGas => "out_of_gas",
            PauseReason::Step => "step",
        },
        "step": snapshot.step,
        "address": format!("{:#x}", snapshot.address),
        "code_address": format!("{:#x}", snapshot.code_address),
        "caller": format!("{:#x}", snapshot.caller),
        "origin": format!("{:#x}", snapshot.origin),
        "value": word(snapshot.value),
        "calldata": hex_bytes(&snapshot.calldata),
        "is_static": snapshot.is_static,
        "depth": snapshot.depth,
        "pc": snapshot.pc,
        "opcode": snapshot.opcode,
        "mnemonic": snapshot.mnemonic,
        "gas_limit": snapshot.gas_limit,
        "gas_remaining": snapshot.gas_remaining,
        "gas_used": snapshot.gas_used,
        "gas_refund": snapshot.gas_refund,
        "stack": snapshot.stack.into_iter().map(word).collect::<Vec<_>>(),
        "memory_size": snapshot.memory_size,
        "memory": hex_bytes(&snapshot.memory),
        "frames": snapshot.frames.into_iter().map(frame_json).collect::<Vec<_>>(),
    })
}

fn frame_json(frame: FrameContext) -> Value {
    json!({
        "depth": frame.depth,
        "kind": frame_kind(frame.kind),
        "address": format!("{:#x}", frame.address),
        "code_address": format!("{:#x}", frame.code_address),
        "caller": format!("{:#x}", frame.caller),
        "value": word(frame.value),
        "calldata": hex_bytes(&frame.calldata),
        "is_static": frame.is_static,
        "pc": frame.pc,
        "opcode": frame.opcode,
        "gas_limit": frame.gas_limit,
        "gas_remaining": frame.gas_remaining,
        "code": hex_bytes(&frame.code),
    })
}

fn frame_kind(kind: FrameKind) -> &'static str {
    match kind {
        FrameKind::Call => "call",
        FrameKind::CallCode => "callcode",
        FrameKind::DelegateCall => "delegatecall",
        FrameKind::StaticCall => "staticcall",
        FrameKind::Create => "create",
        FrameKind::Create2 => "create2",
    }
}

fn word(value: U256) -> String {
    format!("0x{value:x}")
}

fn hex_bytes(value: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(2 + value.len() * 2);
    result.push_str("0x");
    for byte in value {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 0x0f) as usize] as char);
    }
    result
}

fn success_response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error_response(id: Value, code: i32, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": RpcError {
            code,
            message: message.into(),
        }
    })
}
