use revm::primitives::{Address, B256, Bytes, U256};
use std::{fmt, time::Duration};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Breakpoint {
    pub address: Address,
    pub pc: usize,
}

impl Breakpoint {
    pub fn at_any_address(pc: usize) -> Self {
        Self {
            address: Address::ZERO,
            pc,
        }
    }
}

#[derive(Clone, Debug)]
pub struct AccountSpec {
    pub address: Address,
    pub code: Bytes,
    pub balance: U256,
    pub storage: Vec<(U256, U256)>,
}

impl AccountSpec {
    pub fn new(address: Address, code: impl Into<Bytes>) -> Self {
        Self {
            address,
            code: code.into(),
            balance: U256::ZERO,
            storage: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ChainConfig {
    pub accounts: Vec<AccountSpec>,
    pub breakpoints: Vec<Breakpoint>,
}

#[derive(Clone, Debug)]
pub struct SessionConfig {
    pub entry: Address,
    pub caller: Address,
    pub gas_limit: u64,
    pub accounts: Vec<AccountSpec>,
    pub breakpoints: Vec<Breakpoint>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransactionKind {
    Call(Address),
    Create,
}

#[derive(Clone, Debug)]
pub struct TransactionRequest {
    pub caller: Address,
    pub kind: TransactionKind,
    pub gas_limit: u64,
    pub value: U256,
    pub data: Bytes,
    pub commit: bool,
}

impl TransactionRequest {
    pub fn call(caller: Address, target: Address, data: impl Into<Bytes>) -> Self {
        Self {
            caller,
            kind: TransactionKind::Call(target),
            gas_limit: 100_000,
            value: U256::ZERO,
            data: data.into(),
            commit: true,
        }
    }

    pub fn create(caller: Address, init_code: impl Into<Bytes>) -> Self {
        Self {
            caller,
            kind: TransactionKind::Create,
            gas_limit: 3_000_000,
            value: U256::ZERO,
            data: init_code.into(),
            commit: true,
        }
    }
}

impl SessionConfig {
    pub fn new(entry: Address, code: impl Into<Bytes>) -> Self {
        Self {
            entry,
            caller: crate::DEFAULT_CALLER,
            gas_limit: 100_000,
            accounts: vec![AccountSpec::new(entry, code)],
            breakpoints: Vec::new(),
        }
    }

    pub fn with_account(mut self, account: AccountSpec) -> Self {
        self.accounts.push(account);
        self
    }

    pub fn with_breakpoint(mut self, address: Address, pc: usize) -> Self {
        self.breakpoints.push(Breakpoint { address, pc });
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PauseReason {
    Breakpoint,
    OutOfGas,
    Revert,
    Step,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameKind {
    Call,
    CallCode,
    DelegateCall,
    StaticCall,
    Create,
    Create2,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameContext {
    pub depth: usize,
    pub kind: FrameKind,
    pub address: Address,
    pub code_address: Address,
    pub caller: Address,
    pub value: U256,
    pub calldata: Bytes,
    pub is_static: bool,
    pub pc: usize,
    pub opcode: u8,
    pub gas_limit: u64,
    pub gas_remaining: u64,
    pub code: Bytes,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub reason: PauseReason,
    pub step: u64,
    pub address: Address,
    pub code_address: Address,
    pub caller: Address,
    pub origin: Address,
    pub value: U256,
    pub calldata: Bytes,
    pub is_static: bool,
    pub depth: usize,
    pub pc: usize,
    pub opcode: u8,
    pub mnemonic: String,
    pub gas_limit: u64,
    pub gas_remaining: u64,
    pub gas_used: u64,
    pub gas_refund: i64,
    pub stack: Vec<U256>,
    pub memory_size: usize,
    pub memory: Bytes,
    pub frames: Vec<FrameContext>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageSlot {
    pub address: Address,
    pub key: U256,
    pub value: U256,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogEntry {
    pub address: Address,
    pub topics: Vec<B256>,
    pub data: Bytes,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finished {
    pub success: bool,
    pub gas_used: u64,
    pub output: Bytes,
    pub created_address: Option<Address>,
    pub storage: Vec<StorageSlot>,
    pub logs: Vec<LogEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Evaluation {
    pub success: bool,
    pub output: Bytes,
    pub gas_used: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpcodeExecution {
    pub value: Option<U256>,
    pub gas_used: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostCall {
    pub address: Address,
    pub caller: Address,
    pub data: Bytes,
    pub gas_limit: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrankConfig {
    pub caller: Option<Address>,
    pub new_sender: Address,
    pub persistent: bool,
    pub new_origin: Option<Address>,
    pub delegate: bool,
}

impl Finished {
    pub fn storage_at(&self, address: Address, key: U256) -> U256 {
        self.storage
            .iter()
            .find(|slot| slot.address == address && slot.key == key)
            .map_or(U256::ZERO, |slot| slot.value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DebugEvent {
    Paused(Box<Snapshot>),
    HostCall(Box<HostCall>),
    Finished(Finished),
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DebugCommand {
    Snapshot,
    SetStack {
        index: usize,
        value: U256,
    },
    WriteMemory {
        offset: usize,
        data: Bytes,
    },
    SetGas(u64),
    SetPc(usize),
    ReadStorage(U256),
    WriteStorage {
        key: U256,
        value: U256,
    },
    State(StateCommand),
    SetPrank(Option<PrankConfig>),
    Evaluate {
        code: Bytes,
        keep: bool,
    },
    EvaluateCall {
        code: Bytes,
        data: Bytes,
        caller: Address,
        value: U256,
        gas_limit: u64,
        keep: bool,
    },
    ExecuteOpcode {
        opcode: u8,
        arguments: Vec<U256>,
        outputs: usize,
    },
    RespondHost {
        output: Bytes,
        revert: bool,
    },
    Step {
        count: usize,
    },
    Resume,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StateCommand {
    ReadBalance(Address),
    WriteBalance {
        address: Address,
        value: U256,
    },
    ReadCode(Address),
    WriteCode {
        address: Address,
        code: Bytes,
    },
    ReadNonce(Address),
    WriteNonce {
        address: Address,
        value: u64,
    },
    ReadStorage {
        address: Address,
        key: U256,
    },
    WriteStorage {
        address: Address,
        key: U256,
        value: U256,
    },
    ReadTransient {
        address: Address,
        key: U256,
    },
    WriteTransient {
        address: Address,
        key: U256,
        value: U256,
    },
    WarmStorage {
        address: Address,
        key: U256,
    },
    IsStorageWarm {
        address: Address,
        key: U256,
    },
    Logs,
    ReadBlockNumber,
    WriteBlockNumber(U256),
    ReadTimestamp,
    WriteTimestamp(U256),
    ReadBaseFee,
    WriteBaseFee(u64),
    ReadChainId,
    WriteChainId(u64),
    ReadCoinbase,
    WriteCoinbase(Address),
    ReadPrevrandao,
    WritePrevrandao(B256),
    ReadDifficulty,
    WriteDifficulty(U256),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandValue {
    None,
    Snapshot(Box<Snapshot>),
    Word(U256),
    Number(u64),
    Bytes(Bytes),
    Evaluation(Evaluation),
    OpcodeExecution(OpcodeExecution),
    Bool(bool),
    Logs(Vec<LogEntry>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionError {
    NotPaused,
    Timeout(Duration),
    EngineStopped,
    InvalidCommand(String),
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotPaused => write!(f, "the REVM worker is not paused"),
            Self::Timeout(timeout) => write!(f, "REVM command timed out after {timeout:?}"),
            Self::EngineStopped => write!(f, "the REVM worker has stopped"),
            Self::InvalidCommand(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for SessionError {}
