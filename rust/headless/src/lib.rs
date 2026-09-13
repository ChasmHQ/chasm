use revm::primitives::{Address, Bytes, U256};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sevm_revm_core::{
    AccountSpec, Breakpoint, CommandValue, DEFAULT_CALLER, DebugCommand, DebugEngine, DebugEvent,
    PauseReason, SessionConfig, SessionError, Snapshot,
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

fn default_gas_limit() -> u64 {
    100_000
}

fn default_timeout_ms() -> u64 {
    5_000
}

pub struct ProtocolServer {
    engine: Option<DebugEngine>,
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
                        "start",
                        "wait_event",
                        "snapshot",
                        "set_stack",
                        "write_memory",
                        "set_gas",
                        "set_pc",
                        "read_storage",
                        "write_storage",
                        "evaluate",
                        "resume",
                        "close",
                        "shutdown"
                    ]
                }))
            }
            "start" => self.start(parse_params(params)?),
            "wait_event" => self.wait_event(parse_params(params)?),
            "snapshot" => {
                empty_params(params)?;
                self.execute(DebugCommand::Snapshot)
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
            "evaluate" => {
                let params: EvaluateParams = parse_params(params)?;
                self.execute(DebugCommand::Evaluate {
                    code: decode_bytes(&params.bytecode)?,
                    keep: params.keep,
                })
            }
            "resume" => {
                empty_params(params)?;
                self.execute(DebugCommand::Resume)
            }
            "close" => {
                empty_params(params)?;
                self.engine.take();
                Ok(Value::Null)
            }
            "shutdown" => {
                empty_params(params)?;
                self.engine.take();
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
        let breakpoints = params
            .breakpoints
            .into_iter()
            .map(|point| {
                Ok(Breakpoint {
                    address: parse_address(&point.address)?,
                    pc: point.pc,
                })
            })
            .collect::<Result<Vec<_>, ProtocolError>>()?;
        self.engine = Some(DebugEngine::start(SessionConfig {
            entry,
            caller,
            gas_limit: params.gas_limit,
            accounts,
            breakpoints,
        }));
        Ok(json!({ "started": true }))
    }

    fn wait_event(&mut self, params: WaitParams) -> Result<Value, ProtocolError> {
        let event = self
            .engine()?
            .wait(Duration::from_millis(params.timeout_ms))
            .map_err(engine_error)?;
        let terminal = matches!(event, DebugEvent::Finished(_) | DebugEvent::Failed(_));
        let value = event_json(event);
        if terminal {
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
        CommandValue::Snapshot(snapshot) => snapshot_json(snapshot),
        CommandValue::Word(value) => Value::String(word(value)),
        CommandValue::Number(value) => Value::from(value),
        CommandValue::Bytes(value) => Value::String(hex_bytes(&value)),
    }
}

fn event_json(event: DebugEvent) -> Value {
    match event {
        DebugEvent::Paused(snapshot) => json!({
            "type": "paused",
            "snapshot": snapshot_json(snapshot),
        }),
        DebugEvent::Finished(finished) => json!({
            "type": "finished",
            "success": finished.success,
            "gas_used": finished.gas_used,
            "output": hex_bytes(&finished.output),
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
        },
        "address": format!("{:#x}", snapshot.address),
        "depth": snapshot.depth,
        "pc": snapshot.pc,
        "opcode": snapshot.opcode,
        "gas_remaining": snapshot.gas_remaining,
        "stack": snapshot.stack.into_iter().map(word).collect::<Vec<_>>(),
        "memory": hex_bytes(&snapshot.memory),
    })
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
