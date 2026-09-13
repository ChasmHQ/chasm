use revm::primitives::{Address, Bytes, U256};
use std::{fmt, time::Duration};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Breakpoint {
    pub address: Address,
    pub pc: usize,
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

#[derive(Clone, Debug)]
pub struct SessionConfig {
    pub entry: Address,
    pub caller: Address,
    pub gas_limit: u64,
    pub accounts: Vec<AccountSpec>,
    pub breakpoints: Vec<Breakpoint>,
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub reason: PauseReason,
    pub address: Address,
    pub depth: usize,
    pub pc: usize,
    pub opcode: u8,
    pub gas_remaining: u64,
    pub stack: Vec<U256>,
    pub memory: Bytes,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageSlot {
    pub address: Address,
    pub key: U256,
    pub value: U256,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finished {
    pub success: bool,
    pub gas_used: u64,
    pub output: Bytes,
    pub storage: Vec<StorageSlot>,
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
    Paused(Snapshot),
    Finished(Finished),
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DebugCommand {
    Snapshot,
    SetStack { index: usize, value: U256 },
    WriteMemory { offset: usize, data: Bytes },
    SetGas(u64),
    SetPc(usize),
    ReadStorage(U256),
    WriteStorage { key: U256, value: U256 },
    Evaluate { code: Bytes, keep: bool },
    Resume,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandValue {
    None,
    Snapshot(Snapshot),
    Word(U256),
    Number(u64),
    Bytes(Bytes),
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
