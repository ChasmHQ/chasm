use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ethers::types::U256;
use futures::future::BoxFuture;
use serde::Serialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;


const MAX_STEPS: usize = 128;
const MAX_PROBES: usize = 32;
const MAX_VALUE_BYTES: usize = 8 * 1024 * 1024;
const MAX_TOTAL_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_ELEMENTS: usize = 4096;
const MAX_DEPTH: usize = 32;
const MAX_DELAY_MS: u64 = 60 * 60 * 1000;
const MAX_SWEEP_ATTEMPTS: u16 = 512;
static NEXT_RUN_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct RunId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct StepId(pub u64);

/// RPC identity is an opaque session handle, never an endpoint URL.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExecutionToken {
    pub workspace_epoch: u64,
    pub network_epoch: u64,
    pub rpc_token: u64,
}

/// Handles must resolve to a stable in-process signer for the lifetime of a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignerAlias {
    Active,
    Handle(u64),
}

#[derive(Clone)]
pub enum Input {
    Literal(Value),
    Previous { step: StepId, pointer: String },
}

#[derive(Clone)]
pub struct RawTransactionInput {
    pub to: Input,
    pub data: Input,
    pub value: Input,
    pub from: Option<Input>,
}

#[derive(Clone)]
pub struct AbiInput {
    pub address: Input,
    pub abi: Input,
    pub signature: String,
    pub arguments: Input,
    pub value: Input,
    pub from: Option<Input>,
    pub gas: Option<Input>,
    /// `None` is a pinned read; `Some` is a signed mutation.
    pub signer: Option<SignerAlias>,
}

#[derive(Clone)]
pub struct StorageProbe {
    pub address: Input,
    pub slot: Input,
}

#[derive(Clone)]
pub enum GasLimit {
    Exact(u64),
    FromSweep(StepId),
}

#[derive(Clone)]
pub enum SweepPredicate {
    Equals(Value),
    Zero,
    NonZero,
    Revert,
    Success,
}

/// Equality uses exact JSON types. Zero/nonzero accept uint256 quantities or
/// nonempty hex byte strings, true/false require JSON booleans, and HashEq
/// hashes either hex bytes or recursively key-sorted compact JSON.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssertComparator {
    Eq,
    NotEq,
    Zero,
    NonZero,
    True,
    False,
    HashEq,
}

#[derive(Clone)]
pub enum Operation {
    Deploy {
        creation: Input,
        value: Input,
        gas: Input,
        signer: SignerAlias,
    },
    AbiCall(AbiInput),
    RawCall {
        transaction: RawTransactionInput,
        gas: Option<Input>,
    },
    RawSend {
        transaction: RawTransactionInput,
        gas: GasLimit,
        signer: SignerAlias,
    },
    Storage(StorageProbe),
    Snapshot {
        probes: Vec<StorageProbe>,
    },
    Revert {
        snapshot: StepId,
    },
    Assert {
        left: Input,
        comparator: AssertComparator,
        right: Option<Input>,
    },
    Delay {
        milliseconds: u64,
    },
    GasSweep {
        transaction: RawTransactionInput,
        start: u64,
        end: u64,
        increment: u64,
        max_attempts: u16,
        predicate: SweepPredicate,
    },
    ProbeGasSweep {
        probe: Input,
        router: Input,
        data: Input,
        token: Input,
        treasury: Input,
        from: Input,
        start: u64,
        end: u64,
        increment: u64,
        max_attempts: u16,
        outer_gas_margin: u64,
        final_gas_margin: u64,
    },
}

#[derive(Clone)]
pub struct PlanStep {
    /// IDs must be consecutive starting with one. References may only point backward.
    /// The adapter must retain a stable mapping from model string IDs to these
    /// execution IDs, including disabled-step handling, and back to UI results.
    pub id: StepId,
    pub operation: Operation,
}

#[derive(Clone)]
pub struct ExecutionPlan {
    pub token: ExecutionToken,
    pub dry_run: bool,
    pub steps: Vec<PlanStep>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SourceIdentity {
    pub chain_id: String,
    pub genesis_hash: String,
    pub block_number: u64,
    pub block_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawTransaction {
    pub to: String,
    pub data: String,
    /// Canonical decimal wei.
    pub value: String,
    pub from: Option<String>,
}

#[derive(Clone)]
pub struct ProbeGasRequest {
    pub probe: String,
    pub router: String,
    pub data: String,
    pub token: String,
    pub treasury: String,
    pub from: String,
    pub gas: u64,
    pub outer_gas: u64,
    pub source: SourceIdentity,
}

#[derive(Clone)]
pub struct AbiRequest {
    pub address: String,
    pub abi: Value,
    pub signature: String,
    pub arguments: Value,
    pub value: String,
    pub from: Option<String>,
    /// Mandatory for signed calls. The backend must sign this exact limit.
    pub gas: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct RawCallRequest {
    pub transaction: RawTransaction,
    pub gas: Option<u64>,
    /// The backend must issue the call at this block, not at `latest`.
    pub source: SourceIdentity,
}

#[derive(Clone, Debug)]
pub struct RawSendRequest {
    pub transaction: RawTransaction,
    /// This exact limit must be signed and submitted without estimation or replacement.
    pub exact_gas: u64,
    pub signer: SignerAlias,
    /// For a sweep-selected gas limit, the backend must recheck this immediately
    /// before signing and submission, and require the current head to match.
    pub expected_source: Option<SourceIdentity>,
}

#[derive(Clone, Debug)]
pub struct StorageRequest {
    pub address: String,
    pub slot: String,
    pub source: SourceIdentity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCode {
    InvalidPlan,
    InvalidInput,
    InvalidReference,
    InvalidBackendResult,
    StaleContext,
    Unauthorized,
    Unavailable,
    Transport,
    Rpc,
    Reverted,
    SourceChanged,
    VerificationFailed,
    RevertRejected,
    AssertionFailed,
    LimitExceeded,
    NoMatch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownReason {
    #[cfg(test)]
    Submission,
    Receipt,
    Control,
    InvalidConfirmation,
}

/// A successful call returns even-length `0x`-prefixed bytes. Reverts are
/// distinct from transport errors so a sweep can test them.
pub enum ReadOutcome {
    Value(String),
    Revert,
    Failed(FailureCode),
}

/// Only public fields enter references or the run output digest.
pub struct ConfirmedMutation {
    pub tx_hash: String,
    pub contract_address: Option<String>,
    pub reverted: bool,
    /// Optional verified receipt quantities. Missing fields stay unavailable to references.
    pub block_number: Option<String>,
    pub gas_used: Option<String>,
}

/// `Cancelled` and `Failed` mean the backend knows no submission is ambiguous.
/// Any potentially submitted transaction without a verified receipt is `Unknown`.
pub enum MutationOutcome {
    Confirmed(ConfirmedMutation),
    Failed(FailureCode),
    Unknown {
        tx_hash: Option<String>,
        reason: UnknownReason,
    },
    Cancelled,
}

pub enum ControlOutcome<T> {
    Confirmed(T),
    Failed(FailureCode),
    Unknown(UnknownReason),
    Cancelled,
}

/// A held lease excludes every other application-owned mutation of the node.
/// Dropping the lease releases that exclusion.
pub trait MutationLease: Send {}
impl<T: Send> MutationLease for T {}

/// Trusted in-process service boundary. Implementations must validate the token
/// and owned-node capability immediately before each side effect, bind signer
/// aliases to the run, and never return credentials, URLs, signed payloads, or
/// provider error text. They must enforce timeouts and pinned-block reads.
/// Cancellation is only returned when the backend knows no mutation occurred;
/// an in-flight mutation must finish as confirmed, failed, or unknown.
/// Raw sends must check that the resolved signer matches any supplied `from`.
pub trait MacroBackend: Send + Sync {
    fn is_current(&self, token: &ExecutionToken) -> bool;

    /// The returned lease is held for the lifetime of `execute`, including reads
    /// between mutations. Native deploy, send, snapshot, revert, warp, and roll
    /// must all acquire this same exclusive gate. A runner-only lock is insufficient.
    fn acquire_mutation_lease<'a>(
        &'a self,
        token: &'a ExecutionToken,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<Box<dyn MutationLease + 'a>, FailureCode>>;

    /// `None` reads the head; `Some(n)` reads that exact numbered block.
    fn source<'a>(
        &'a self,
        token: &'a ExecutionToken,
        block: Option<u64>,
    ) -> BoxFuture<'a, Result<SourceIdentity, FailureCode>>;

    /// Signs `exact_gas` unchanged; estimation or substitution is not permitted.
    fn deploy<'a>(
        &'a self,
        token: &'a ExecutionToken,
        creation: &'a str,
        value: &'a str,
        exact_gas: u64,
        signer: SignerAlias,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, MutationOutcome>;

    fn abi_call<'a>(
        &'a self,
        token: &'a ExecutionToken,
        request: &'a AbiRequest,
        source: &'a SourceIdentity,
    ) -> BoxFuture<'a, ReadOutcome>;

    /// Signs `request.gas` unchanged; estimation or substitution is not permitted.
    fn abi_send<'a>(
        &'a self,
        token: &'a ExecutionToken,
        request: &'a AbiRequest,
        signer: SignerAlias,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, MutationOutcome>;

    fn raw_call<'a>(
        &'a self,
        token: &'a ExecutionToken,
        request: &'a RawCallRequest,
    ) -> BoxFuture<'a, ReadOutcome>;

    fn probe_gas<'a>(
        &'a self,
        token: &'a ExecutionToken,
        request: &'a ProbeGasRequest,
    ) -> BoxFuture<'a, ReadOutcome>;

    fn raw_send<'a>(
        &'a self,
        token: &'a ExecutionToken,
        request: &'a RawSendRequest,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, MutationOutcome>;

    /// Returns one exact 32-byte storage word.
    fn storage<'a>(
        &'a self,
        token: &'a ExecutionToken,
        request: &'a StorageRequest,
    ) -> BoxFuture<'a, Result<String, FailureCode>>;

    fn snapshot<'a>(
        &'a self,
        token: &'a ExecutionToken,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, ControlOutcome<String>>;

    fn revert<'a>(
        &'a self,
        token: &'a ExecutionToken,
        snapshot: &'a str,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, ControlOutcome<bool>>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    Deploy,
    AbiCall,
    RawCall,
    RawSend,
    Storage,
    Snapshot,
    Revert,
    Assert,
    Delay,
    GasSweep,
    ProbeGasSweep,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    Running,
    Succeeded,
    RevertAcknowledgedUnverified,
    Failed,
    Unknown,
    Cancelled,
    Skipped,
    RollbackFailed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    DryRunMutation,
    DryRunDependency,
    Stopped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionState {
    Confirmed,
    Unknown,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct OutputSummary {
    pub digest: Option<String>,
    pub encoded_bytes: Option<usize>,
    pub transaction_hash: Option<String>,
    pub submission: Option<SubmissionState>,
    pub selected_gas: Option<u64>,
    pub attempts: Option<u16>,
    pub verified_probes: Option<usize>,
    pub source: Option<SourceIdentity>,
}

#[derive(Clone, Debug, Serialize)]
pub struct StepReport {
    pub id: StepId,
    pub kind: StepKind,
    pub status: StepStatus,
    pub failure: Option<FailureCode>,
    pub unknown: Option<UnknownReason>,
    pub skipped: Option<SkipReason>,
    pub output: OutputSummary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Completed,
    PreflightComplete,
    InvalidPlan,
    Failed,
    Unknown,
    Cancelled,
    RevertAcknowledgedUnverified,
    RollbackFailed,
}

#[derive(Clone, Debug, Serialize)]
pub struct RunReport {
    pub id: RunId,
    pub token: ExecutionToken,
    pub dry_run: bool,
    pub status: RunStatus,
    pub failure: Option<FailureCode>,
    pub steps: Vec<StepReport>,
}

/// Progress reported to the run observer; tests assert on every field.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub enum RunEvent {
    Started {
        id: RunId,
        token: ExecutionToken,
        dry_run: bool,
    },
    Step {
        id: RunId,
        step: StepReport,
    },
    Finished {
        id: RunId,
        status: RunStatus,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunIdsExhausted;

#[derive(Clone)]
struct Checkpoint {
    id: String,
    source: SourceIdentity,
    probes: Vec<(String, String, String)>,
}

#[derive(Clone)]
struct SweepSelection {
    transaction: RawTransaction,
    source: SourceIdentity,
    gas: u64,
}

struct State {
    outputs: HashMap<StepId, Value>,
    skipped: HashSet<StepId>,
    snapshots: HashMap<StepId, Checkpoint>,
    sweeps: HashMap<StepId, SweepSelection>,
    remaining_output: usize,
}

#[derive(Clone, Copy)]
enum Stop {
    Failed(FailureCode),
    Cancelled,
    DependencySkipped,
}

impl From<FailureCode> for Stop {
    fn from(error: FailureCode) -> Self {
        Self::Failed(error)
    }
}

fn kind(operation: &Operation) -> StepKind {
    match operation {
        Operation::Deploy { .. } => StepKind::Deploy,
        Operation::AbiCall(_) => StepKind::AbiCall,
        Operation::RawCall { .. } => StepKind::RawCall,
        Operation::RawSend { .. } => StepKind::RawSend,
        Operation::Storage(_) => StepKind::Storage,
        Operation::Snapshot { .. } => StepKind::Snapshot,
        Operation::Revert { .. } => StepKind::Revert,
        Operation::Assert { .. } => StepKind::Assert,
        Operation::Delay { .. } => StepKind::Delay,
        Operation::GasSweep { .. } => StepKind::GasSweep,
        Operation::ProbeGasSweep { .. } => StepKind::ProbeGasSweep,
    }
}

fn mutates(operation: &Operation) -> bool {
    matches!(
        operation,
        Operation::Deploy { .. }
            | Operation::RawSend { .. }
            | Operation::Snapshot { .. }
            | Operation::Revert { .. }
    ) || matches!(operation, Operation::AbiCall(input) if input.signer.is_some())
}

fn check_value(value: &Value) -> Result<usize, FailureCode> {
    fn visit(
        value: &Value,
        depth: usize,
        elements: &mut usize,
        bytes: &mut usize,
    ) -> Result<(), FailureCode> {
        if depth > MAX_DEPTH || *elements == 0 {
            return Err(FailureCode::LimitExceeded);
        }
        *elements -= 1;
        match value {
            Value::String(text) => {
                *bytes = bytes
                    .checked_add(text.len())
                    .ok_or(FailureCode::LimitExceeded)?
            }
            Value::Array(items) => {
                for item in items {
                    visit(item, depth + 1, elements, bytes)?;
                }
            }
            Value::Object(items) => {
                for (key, item) in items {
                    *bytes = bytes
                        .checked_add(key.len())
                        .ok_or(FailureCode::LimitExceeded)?;
                    visit(item, depth + 1, elements, bytes)?;
                }
            }
            _ => *bytes = bytes.checked_add(32).ok_or(FailureCode::LimitExceeded)?,
        }
        if *bytes > MAX_VALUE_BYTES {
            return Err(FailureCode::LimitExceeded);
        }
        Ok(())
    }
    let mut elements = MAX_ELEMENTS;
    let mut estimated_bytes = 0;
    visit(value, 0, &mut elements, &mut estimated_bytes)?;
    let bytes = serde_json::to_vec(value).map_err(|_| FailureCode::InvalidInput)?;
    if bytes.len() > MAX_VALUE_BYTES {
        return Err(FailureCode::LimitExceeded);
    }
    Ok(bytes.len())
}

fn validate_input(input: &Input, index: usize, total: &mut usize) -> Result<(), FailureCode> {
    match input {
        Input::Literal(value) => {
            *total = total
                .checked_add(check_value(value)?)
                .ok_or(FailureCode::LimitExceeded)?;
            if *total > MAX_TOTAL_OUTPUT_BYTES {
                return Err(FailureCode::LimitExceeded);
            }
        }
        Input::Previous { step, pointer } => {
            if step.0 == 0
                || step.0 >= index as u64 + 1
                || pointer.len() > 512
                || (!pointer.is_empty() && !pointer.starts_with('/'))
                || pointer
                    .as_bytes()
                    .windows(2)
                    .any(|pair| pair[0] == b'~' && pair[1] != b'0' && pair[1] != b'1')
                || pointer.ends_with('~')
            {
                return Err(FailureCode::InvalidReference);
            }
        }
    }
    Ok(())
}

fn validate_transaction(
    tx: &RawTransactionInput,
    index: usize,
    total: &mut usize,
) -> Result<(), FailureCode> {
    for input in [&tx.to, &tx.data, &tx.value] {
        validate_input(input, index, total)?;
    }
    if let Some(input) = &tx.from {
        validate_input(input, index, total)?;
    }
    Ok(())
}

fn validate_probe(
    probe: &StorageProbe,
    index: usize,
    total: &mut usize,
) -> Result<(), FailureCode> {
    validate_input(&probe.address, index, total)?;
    validate_input(&probe.slot, index, total)
}

fn validate_plan(plan: &ExecutionPlan) -> Result<(), FailureCode> {
    if plan.steps.is_empty() || plan.steps.len() > MAX_STEPS {
        return Err(FailureCode::InvalidPlan);
    }
    let mut total = 0;
    for (index, step) in plan.steps.iter().enumerate() {
        if step.id.0 != index as u64 + 1 {
            return Err(FailureCode::InvalidPlan);
        }
        match &step.operation {
            Operation::Deploy {
                creation,
                value,
                gas,
                ..
            } => {
                validate_input(creation, index, &mut total)?;
                validate_input(value, index, &mut total)?;
                validate_input(gas, index, &mut total)?;
            }
            Operation::AbiCall(input) => {
                if input.signature.is_empty()
                    || input.signature.len() > 512
                    || (input.signer.is_some() && input.gas.is_none())
                {
                    return Err(FailureCode::InvalidInput);
                }
                for value in [&input.address, &input.abi, &input.arguments, &input.value] {
                    validate_input(value, index, &mut total)?;
                }
                for value in [input.from.as_ref(), input.gas.as_ref()]
                    .into_iter()
                    .flatten()
                {
                    validate_input(value, index, &mut total)?;
                }
            }
            Operation::RawCall { transaction, gas } => {
                validate_transaction(transaction, index, &mut total)?;
                if let Some(value) = gas {
                    validate_input(value, index, &mut total)?;
                }
            }
            Operation::RawSend {
                transaction, gas, ..
            } => {
                validate_transaction(transaction, index, &mut total)?;
                match gas {
                    GasLimit::Exact(0) => return Err(FailureCode::InvalidInput),
                    GasLimit::Exact(_) => {}
                    GasLimit::FromSweep(id) => {
                        let prior =
                            id.0.checked_sub(1)
                                .and_then(|n| usize::try_from(n).ok())
                                .filter(|n| *n < index)
                                .and_then(|n| plan.steps.get(n));
                        if !matches!(
                            prior.map(|step| &step.operation),
                            Some(Operation::GasSweep { .. } | Operation::ProbeGasSweep { .. })
                        ) {
                            return Err(FailureCode::InvalidReference);
                        }
                    }
                }
            }
            Operation::Storage(probe) => validate_probe(probe, index, &mut total)?,
            Operation::Snapshot { probes } => {
                if probes.len() > MAX_PROBES {
                    return Err(FailureCode::InvalidInput);
                }
                for probe in probes {
                    validate_probe(probe, index, &mut total)?;
                }
            }
            Operation::Revert { snapshot } => {
                let prior = snapshot
                    .0
                    .checked_sub(1)
                    .and_then(|n| usize::try_from(n).ok())
                    .filter(|n| *n < index)
                    .and_then(|n| plan.steps.get(n));
                if !matches!(
                    prior.map(|step| &step.operation),
                    Some(Operation::Snapshot { .. })
                ) {
                    return Err(FailureCode::InvalidReference);
                }
            }
            Operation::Assert {
                left,
                comparator,
                right,
            } => {
                validate_input(left, index, &mut total)?;
                let binary = matches!(
                    comparator,
                    AssertComparator::Eq | AssertComparator::NotEq | AssertComparator::HashEq
                );
                if binary != right.is_some() {
                    return Err(FailureCode::InvalidInput);
                }
                if let Some(right) = right {
                    validate_input(right, index, &mut total)?;
                }
            }
            Operation::Delay { milliseconds } => {
                if *milliseconds > MAX_DELAY_MS {
                    return Err(FailureCode::LimitExceeded);
                }
            }
            Operation::ProbeGasSweep {
                probe, router, data, token, treasury, from,
                start, end, increment, max_attempts, outer_gas_margin, final_gas_margin,
            } => {
                for input in [probe, router, data, token, treasury, from] {
                    validate_input(input, index, &mut total)?;
                }
                let attempts = end.checked_sub(*start)
                    .and_then(|span| span.checked_div(*increment))
                    .and_then(|count| count.checked_add(1));
                if *start == 0 || *increment == 0 || *max_attempts == 0
                    || *max_attempts > MAX_SWEEP_ATTEMPTS
                    || !attempts.is_some_and(|count| count <= u64::from(*max_attempts))
                    || !(100_000..=4_000_000).contains(outer_gas_margin)
                    || *final_gas_margin > 65_536
                    || end.checked_add(*outer_gas_margin).is_none()
                    || end.checked_add(*final_gas_margin).is_none()
                {
                    return Err(FailureCode::InvalidInput);
                }
            }
            Operation::GasSweep {
                transaction,
                start,
                end,
                increment,
                max_attempts,
                predicate,
            } => {
                validate_transaction(transaction, index, &mut total)?;
                if *start == 0
                    || start > end
                    || *increment == 0
                    || *max_attempts == 0
                    || *max_attempts > MAX_SWEEP_ATTEMPTS
                {
                    return Err(FailureCode::InvalidInput);
                }
                if let SweepPredicate::Equals(value) = predicate {
                    let text = value.as_str().ok_or(FailureCode::InvalidInput)?;
                    hex_bytes(text, true, None)?;
                    total = total
                        .checked_add(check_value(value)?)
                        .ok_or(FailureCode::LimitExceeded)?;
                    if total > MAX_TOTAL_OUTPUT_BYTES {
                        return Err(FailureCode::LimitExceeded);
                    }
                }
            }
        }
    }
    Ok(())
}

fn resolve(input: &Input, state: &State) -> Result<Value, Stop> {
    match input {
        Input::Literal(value) => Ok(value.clone()),
        Input::Previous { step, pointer } => match state.outputs.get(step) {
            Some(value) => value
                .pointer(pointer)
                .cloned()
                .ok_or(Stop::Failed(FailureCode::InvalidReference)),
            None if state.skipped.contains(step) => Err(Stop::DependencySkipped),
            None => Err(Stop::Failed(FailureCode::InvalidReference)),
        },
    }
}

fn string(value: Value) -> Result<String, Stop> {
    value
        .as_str()
        .map(str::to_owned)
        .ok_or(Stop::Failed(FailureCode::InvalidInput))
}

fn hex_bytes(text: &str, allow_empty: bool, exact: Option<usize>) -> Result<String, FailureCode> {
    let digits = text.strip_prefix("0x").ok_or(FailureCode::InvalidInput)?;
    if (!allow_empty && digits.is_empty())
        || digits.len() % 2 != 0
        || exact.is_some_and(|length| digits.len() != length * 2)
        || digits.len() > MAX_VALUE_BYTES - 2
        || !digits.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(FailureCode::InvalidInput);
    }
    Ok(format!("0x{}", digits.to_ascii_lowercase()))
}

fn quantity(value: Value) -> Result<U256, Stop> {
    let text = match value {
        Value::String(text) => text,
        Value::Number(number) => number.to_string(),
        _ => return Err(Stop::Failed(FailureCode::InvalidInput)),
    };
    let parsed = if let Some(hex) = text.strip_prefix("0x") {
        if hex.is_empty() || hex.len() > 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            None
        } else {
            U256::from_str_radix(hex, 16).ok()
        }
    } else if !text.is_empty() && text.len() <= 78 && text.bytes().all(|b| b.is_ascii_digit()) {
        U256::from_dec_str(&text).ok()
    } else {
        None
    };
    parsed.ok_or(Stop::Failed(FailureCode::InvalidInput))
}

fn resolve_address(input: &Input, state: &State) -> Result<String, Stop> {
    Ok(hex_bytes(
        &string(resolve(input, state)?)?,
        false,
        Some(20),
    )?)
}

fn resolve_gas(input: &Input, state: &State) -> Result<u64, Stop> {
    let value = quantity(resolve(input, state)?)?;
    if value.is_zero() || value > U256::from(u64::MAX) {
        return Err(Stop::Failed(FailureCode::InvalidInput));
    }
    Ok(value.as_u64())
}

fn resolve_transaction(input: &RawTransactionInput, state: &State) -> Result<RawTransaction, Stop> {
    Ok(RawTransaction {
        to: resolve_address(&input.to, state)?,
        data: hex_bytes(&string(resolve(&input.data, state)?)?, true, None)?,
        value: quantity(resolve(&input.value, state)?)?.to_string(),
        from: input
            .from
            .as_ref()
            .map(|from| resolve_address(from, state))
            .transpose()?,
    })
}

fn resolve_abi(input: &AbiInput, state: &State) -> Result<AbiRequest, Stop> {
    let abi = resolve(&input.abi, state)?;
    let arguments = resolve(&input.arguments, state)?;
    if !abi.is_array() || !arguments.is_array() {
        return Err(Stop::Failed(FailureCode::InvalidInput));
    }
    Ok(AbiRequest {
        address: resolve_address(&input.address, state)?,
        abi,
        signature: input.signature.clone(),
        arguments,
        value: quantity(resolve(&input.value, state)?)?.to_string(),
        from: input
            .from
            .as_ref()
            .map(|from| resolve_address(from, state))
            .transpose()?,
        gas: input
            .gas
            .as_ref()
            .map(|gas| resolve_gas(gas, state))
            .transpose()?,
    })
}

fn resolve_probe(input: &StorageProbe, state: &State) -> Result<(String, String), Stop> {
    Ok((
        resolve_address(&input.address, state)?,
        format!("{:#x}", quantity(resolve(&input.slot, state)?)?),
    ))
}

fn canonical_source(mut source: SourceIdentity) -> Result<SourceIdentity, FailureCode> {
    source.chain_id = format!(
        "{:#x}",
        quantity(Value::String(source.chain_id)).map_err(|_| FailureCode::InvalidInput)?
    );
    source.genesis_hash = hex_bytes(&source.genesis_hash, false, Some(32))?;
    source.block_hash = hex_bytes(&source.block_hash, false, Some(32))?;
    Ok(source)
}

async fn read_future<T>(
    future: BoxFuture<'_, T>,
    cancel: Option<&CancellationToken>,
) -> Result<T, Stop> {
    match cancel {
        Some(cancel) => {
            tokio::select! { biased; _ = cancel.cancelled() => Err(Stop::Cancelled), value = future => Ok(value) }
        }
        None => Ok(future.await),
    }
}

async fn source<B: MacroBackend + ?Sized>(
    backend: &B,
    token: &ExecutionToken,
    block: Option<u64>,
    cancel: Option<&CancellationToken>,
) -> Result<SourceIdentity, Stop> {
    if !backend.is_current(token) {
        return Err(Stop::Failed(FailureCode::StaleContext));
    }
    let value = read_future(backend.source(token, block), cancel).await??;
    if !backend.is_current(token) {
        return Err(Stop::Failed(FailureCode::StaleContext));
    }
    let value = canonical_source(value)?;
    if block.is_some_and(|number| number != value.block_number) {
        return Err(Stop::Failed(FailureCode::SourceChanged));
    }
    Ok(value)
}

async fn verify_source<B: MacroBackend + ?Sized>(
    backend: &B,
    token: &ExecutionToken,
    expected: &SourceIdentity,
    cancel: Option<&CancellationToken>,
) -> Result<(), Stop> {
    if source(backend, token, Some(expected.block_number), cancel).await? != *expected {
        return Err(Stop::Failed(FailureCode::SourceChanged));
    }
    Ok(())
}

async fn raw_read<B: MacroBackend + ?Sized>(
    backend: &B,
    token: &ExecutionToken,
    request: &RawCallRequest,
    cancel: &CancellationToken,
) -> Result<ReadOutcome, Stop> {
    verify_source(backend, token, &request.source, Some(cancel)).await?;
    let result = read_future(backend.raw_call(token, request), Some(cancel)).await?;
    verify_source(backend, token, &request.source, Some(cancel)).await?;
    normalize_read(result)
}

async fn probe_read<B: MacroBackend + ?Sized>(
    backend: &B,
    token: &ExecutionToken,
    request: &ProbeGasRequest,
    cancel: &CancellationToken,
) -> Result<ReadOutcome, Stop> {
    verify_source(backend, token, &request.source, Some(cancel)).await?;
    let result = read_future(backend.probe_gas(token, request), Some(cancel)).await?;
    verify_source(backend, token, &request.source, Some(cancel)).await?;
    normalize_read(result)
}

fn normalize_read(result: ReadOutcome) -> Result<ReadOutcome, Stop> {
    match result {
        ReadOutcome::Value(value) => hex_bytes(&value, true, None)
            .map(ReadOutcome::Value)
            .map_err(|_| Stop::Failed(FailureCode::InvalidBackendResult)),
        other => Ok(other),
    }
}

async fn storage_read<B: MacroBackend + ?Sized>(
    backend: &B,
    token: &ExecutionToken,
    request: &StorageRequest,
    cancel: Option<&CancellationToken>,
) -> Result<String, Stop> {
    verify_source(backend, token, &request.source, cancel).await?;
    let word = read_future(backend.storage(token, request), cancel).await??;
    verify_source(backend, token, &request.source, cancel).await?;
    Ok(hex_bytes(&word, false, Some(32))?)
}

fn digest(bytes: &[u8]) -> String {
    format!(
        "0x{}",
        ethers::utils::hex::encode(ethers::utils::keccak256(bytes))
    )
}

fn canonical_json(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            out.push('{');
            let mut keys: Vec<_> = map.keys().collect();
            keys.sort_unstable();
            for (index, key) in keys.iter().enumerate() {
                if index != 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(key).unwrap_or_default());
                out.push(':');
                canonical_json(&map[*key], out);
            }
            out.push('}');
        }
        Value::Array(values) => {
            out.push('[');
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    out.push(',');
                }
                canonical_json(value, out);
            }
            out.push(']');
        }
        _ => out.push_str(&serde_json::to_string(value).unwrap_or_default()),
    }
}

fn assertion_matches(
    comparator: AssertComparator,
    left: &Value,
    right: Option<&Value>,
) -> Result<bool, FailureCode> {
    check_value(left)?;
    if let Some(right) = right {
        check_value(right)?;
    }
    match comparator {
        AssertComparator::Eq => Ok(left == right.ok_or(FailureCode::InvalidInput)?),
        AssertComparator::NotEq => Ok(left != right.ok_or(FailureCode::InvalidInput)?),
        AssertComparator::True => left.as_bool().ok_or(FailureCode::InvalidInput),
        AssertComparator::False => left
            .as_bool()
            .map(|value| !value)
            .ok_or(FailureCode::InvalidInput),
        AssertComparator::Zero | AssertComparator::NonZero => {
            let zero =
                if let Some(digits) = left.as_str().and_then(|value| value.strip_prefix("0x")) {
                    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                        return Err(FailureCode::InvalidInput);
                    }
                    digits.bytes().all(|byte| byte == b'0')
                } else {
                    quantity(left.clone())
                        .map_err(|_| FailureCode::InvalidInput)?
                        .is_zero()
                };
            Ok(if comparator == AssertComparator::Zero {
                zero
            } else {
                !zero
            })
        }
        AssertComparator::HashEq => {
            let expected = right
                .and_then(Value::as_str)
                .and_then(valid_hash)
                .ok_or(FailureCode::InvalidInput)?;
            let actual = if let Some(text) = left.as_str().filter(|text| text.starts_with("0x")) {
                let hex = hex_bytes(text, true, None)?;
                digest(
                    &ethers::utils::hex::decode(&hex[2..])
                        .map_err(|_| FailureCode::InvalidInput)?,
                )
            } else {
                let mut encoded = String::new();
                canonical_json(left, &mut encoded);
                digest(encoded.as_bytes())
            };
            Ok(actual == expected)
        }
    }
}

fn store_output(
    state: &mut State,
    id: StepId,
    value: Value,
    summary: &mut OutputSummary,
) -> Result<(), FailureCode> {
    let size = check_value(&value)?;
    if size > state.remaining_output {
        return Err(FailureCode::LimitExceeded);
    }
    let encoded = serde_json::to_vec(&value).map_err(|_| FailureCode::InvalidInput)?;
    summary.digest = Some(digest(&encoded));
    summary.encoded_bytes = Some(size);
    state.remaining_output -= size;
    state.outputs.insert(id, value);
    Ok(())
}

fn valid_hash(value: &str) -> Option<String> {
    hex_bytes(value, false, Some(32)).ok()
}

fn predicate_matches(
    predicate: &SweepPredicate,
    result: &ReadOutcome,
) -> Result<bool, FailureCode> {
    match (predicate, result) {
        (_, ReadOutcome::Failed(error)) => Err(*error),
        (SweepPredicate::Revert, ReadOutcome::Revert) => Ok(true),
        (SweepPredicate::Revert, _) | (_, ReadOutcome::Revert) => Ok(false),
        (SweepPredicate::Success, ReadOutcome::Value(_)) => Ok(true),
        (SweepPredicate::Equals(expected), ReadOutcome::Value(actual)) => {
            let expected = expected.as_str().ok_or(FailureCode::InvalidInput)?;
            Ok(expected.eq_ignore_ascii_case(actual))
        }
        (SweepPredicate::Zero | SweepPredicate::NonZero, ReadOutcome::Value(actual)) => {
            let bytes = actual
                .strip_prefix("0x")
                .ok_or(FailureCode::InvalidBackendResult)?;
            if bytes.is_empty() {
                return Err(FailureCode::InvalidBackendResult);
            }
            let zero = bytes.bytes().all(|byte| byte == b'0');
            Ok(if matches!(predicate, SweepPredicate::Zero) {
                zero
            } else {
                !zero
            })
        }
    }
}

fn stop_status(stop: Stop, report: &mut StepReport) -> bool {
    match stop {
        Stop::Failed(error) => {
            report.status = StepStatus::Failed;
            report.failure = Some(error);
            true
        }
        Stop::Cancelled => {
            report.status = StepStatus::Cancelled;
            true
        }
        Stop::DependencySkipped => {
            report.status = StepStatus::Skipped;
            report.skipped = Some(SkipReason::DryRunDependency);
            false
        }
    }
}

fn mutation_result(
    outcome: MutationOutcome,
    id: StepId,
    state: &mut State,
    report: &mut StepReport,
    deployment: bool,
) -> bool {
    match outcome {
        MutationOutcome::Cancelled => {
            report.status = StepStatus::Cancelled;
            true
        }
        MutationOutcome::Failed(error) => {
            report.status = StepStatus::Failed;
            report.failure = Some(error);
            true
        }
        MutationOutcome::Unknown { tx_hash, reason } => {
            report.status = StepStatus::Unknown;
            report.unknown = Some(reason);
            report.output.submission = Some(SubmissionState::Unknown);
            report.output.transaction_hash = tx_hash.as_deref().and_then(valid_hash);
            true
        }
        MutationOutcome::Confirmed(confirmed) => {
            let Some(hash) = valid_hash(&confirmed.tx_hash) else {
                report.status = StepStatus::Unknown;
                report.unknown = Some(UnknownReason::InvalidConfirmation);
                report.output.submission = Some(SubmissionState::Unknown);
                return true;
            };
            report.output.transaction_hash = Some(hash.clone());
            report.output.submission = Some(SubmissionState::Confirmed);
            if confirmed.reverted {
                report.status = StepStatus::Failed;
                report.failure = Some(FailureCode::Reverted);
                return true;
            }
            let address = if deployment {
                match confirmed
                    .contract_address
                    .as_deref()
                    .and_then(|v| hex_bytes(v, false, Some(20)).ok())
                {
                    Some(address) if address[2..].bytes().any(|b| b != b'0') => Some(address),
                    _ => {
                        report.status = StepStatus::Unknown;
                        report.unknown = Some(UnknownReason::InvalidConfirmation);
                        report.output.submission = Some(SubmissionState::Unknown);
                        return true;
                    }
                }
            } else {
                None
            };
            let mut output = json!({"transaction_hash": hash});
            if let Some(address) = address {
                output["address"] = Value::String(address);
            }
            for (field, quantity_text) in [
                ("block_number", confirmed.block_number),
                ("gas_used", confirmed.gas_used),
            ] {
                if let Some(text) = quantity_text {
                    let Ok(number) = quantity(Value::String(text)) else {
                        report.status = StepStatus::Failed;
                        report.failure = Some(FailureCode::InvalidBackendResult);
                        return true;
                    };
                    output[field] = Value::String(format!("{number:#x}"));
                }
            }
            match store_output(state, id, output, &mut report.output) {
                Ok(()) => {
                    report.status = StepStatus::Succeeded;
                    false
                }
                Err(error) => {
                    report.status = StepStatus::Failed;
                    report.failure = Some(error);
                    true
                }
            }
        }
    }
}

async fn execute_step<B: MacroBackend + ?Sized>(
    backend: &B,
    token: &ExecutionToken,
    cancel: &CancellationToken,
    dry_run: bool,
    step: &PlanStep,
    state: &mut State,
    report: &mut StepReport,
) -> bool {
    if dry_run && mutates(&step.operation) {
        report.status = StepStatus::Skipped;
        report.skipped = Some(SkipReason::DryRunMutation);
        return false;
    }
    let result: Result<(), Stop> = async {
        match &step.operation {
            Operation::Deploy {
                creation,
                value,
                gas,
                signer,
            } => {
                let creation = hex_bytes(&string(resolve(creation, state)?)?, false, None)?;
                let value = quantity(resolve(value, state)?)?.to_string();
                let gas = resolve_gas(gas, state)?;
                report.output.selected_gas = Some(gas);
                if !backend.is_current(token) {
                    return Err(Stop::Failed(FailureCode::StaleContext));
                }
                if cancel.is_cancelled() {
                    return Err(Stop::Cancelled);
                }
                let outcome = backend
                    .deploy(token, &creation, &value, gas, *signer, cancel)
                    .await;
                if mutation_result(outcome, step.id, state, report, true) {
                    return Ok(());
                }
            }
            Operation::AbiCall(input) => {
                let request = resolve_abi(input, state)?;
                if let Some(signer) = input.signer {
                    report.output.selected_gas = request.gas;
                    if !backend.is_current(token) {
                        return Err(Stop::Failed(FailureCode::StaleContext));
                    }
                    if cancel.is_cancelled() {
                        return Err(Stop::Cancelled);
                    }
                    let outcome = backend.abi_send(token, &request, signer, cancel).await;
                    if mutation_result(outcome, step.id, state, report, false) {
                        return Ok(());
                    }
                } else {
                    let pinned = source(backend, token, None, Some(cancel)).await?;
                    verify_source(backend, token, &pinned, Some(cancel)).await?;
                    let outcome =
                        read_future(backend.abi_call(token, &request, &pinned), Some(cancel))
                            .await?;
                    verify_source(backend, token, &pinned, Some(cancel)).await?;
                    let outcome = normalize_read(outcome)?;
                    report.output.source = Some(pinned);
                    match outcome {
                        ReadOutcome::Value(value) => store_output(
                            state,
                            step.id,
                            json!({"value": value}),
                            &mut report.output,
                        )?,
                        ReadOutcome::Revert => return Err(Stop::Failed(FailureCode::Reverted)),
                        ReadOutcome::Failed(error) => return Err(Stop::Failed(error)),
                    }
                }
            }
            Operation::RawCall { transaction, gas } => {
                let tx = resolve_transaction(transaction, state)?;
                let gas = gas
                    .as_ref()
                    .map(|gas| resolve_gas(gas, state))
                    .transpose()?;
                let pinned = source(backend, token, None, Some(cancel)).await?;
                let outcome = raw_read(
                    backend,
                    token,
                    &RawCallRequest {
                        transaction: tx,
                        gas,
                        source: pinned.clone(),
                    },
                    cancel,
                )
                .await?;
                report.output.source = Some(pinned);
                match outcome {
                    ReadOutcome::Value(value) => {
                        store_output(state, step.id, json!({"value": value}), &mut report.output)?
                    }
                    ReadOutcome::Revert => return Err(Stop::Failed(FailureCode::Reverted)),
                    ReadOutcome::Failed(error) => return Err(Stop::Failed(error)),
                }
            }
            Operation::RawSend {
                transaction,
                gas,
                signer,
            } => {
                let tx = resolve_transaction(transaction, state)?;
                let (exact_gas, expected_source) = match gas {
                    GasLimit::Exact(gas) => (*gas, None),
                    GasLimit::FromSweep(id) => {
                        if state.skipped.contains(id) {
                            return Err(Stop::DependencySkipped);
                        }
                        let sweep = state
                            .sweeps
                            .get(id)
                            .ok_or(Stop::Failed(FailureCode::InvalidReference))?;
                        if tx.from.is_none() || tx != sweep.transaction {
                            return Err(Stop::Failed(FailureCode::InvalidInput));
                        }
                        if source(backend, token, None, Some(cancel)).await? != sweep.source {
                            return Err(Stop::Failed(FailureCode::SourceChanged));
                        }
                        (sweep.gas, Some(sweep.source.clone()))
                    }
                };
                if !backend.is_current(token) {
                    return Err(Stop::Failed(FailureCode::StaleContext));
                }
                if cancel.is_cancelled() {
                    return Err(Stop::Cancelled);
                }
                report.output.selected_gas = Some(exact_gas);
                report.output.source = expected_source.clone();
                let request = RawSendRequest {
                    transaction: tx,
                    exact_gas,
                    signer: *signer,
                    expected_source,
                };
                let outcome = backend.raw_send(token, &request, cancel).await;
                if mutation_result(outcome, step.id, state, report, false) {
                    return Ok(());
                }
            }
            Operation::Storage(probe) => {
                let (address, slot) = resolve_probe(probe, state)?;
                let pinned = source(backend, token, None, Some(cancel)).await?;
                let value = storage_read(
                    backend,
                    token,
                    &StorageRequest {
                        address,
                        slot,
                        source: pinned.clone(),
                    },
                    Some(cancel),
                )
                .await?;
                report.output.source = Some(pinned);
                store_output(state, step.id, json!({"value": value}), &mut report.output)?;
            }
            Operation::Snapshot { probes } => {
                let probes = probes
                    .iter()
                    .map(|probe| resolve_probe(probe, state))
                    .collect::<Result<Vec<_>, _>>()?;
                let pinned = source(backend, token, None, Some(cancel)).await?;
                if cancel.is_cancelled() {
                    return Err(Stop::Cancelled);
                }
                let snapshot = match backend.snapshot(token, cancel).await {
                    ControlOutcome::Confirmed(id) => id,
                    ControlOutcome::Failed(error) => return Err(Stop::Failed(error)),
                    ControlOutcome::Cancelled => return Err(Stop::Cancelled),
                    ControlOutcome::Unknown(reason) => {
                        report.status = StepStatus::Unknown;
                        report.unknown = Some(reason);
                        return Ok(());
                    }
                };
                if snapshot.len() > 66 || quantity(Value::String(snapshot.clone())).is_err() {
                    report.status = StepStatus::Unknown;
                    report.unknown = Some(UnknownReason::InvalidConfirmation);
                    return Ok(());
                }
                if source(backend, token, None, Some(cancel)).await? != pinned {
                    return Err(Stop::Failed(FailureCode::SourceChanged));
                }
                let mut captured = Vec::with_capacity(probes.len());
                for (address, slot) in probes {
                    let request = StorageRequest {
                        address: address.clone(),
                        slot: slot.clone(),
                        source: pinned.clone(),
                    };
                    let word = storage_read(backend, token, &request, Some(cancel)).await?;
                    captured.push((address, slot, word));
                }
                report.output.source = Some(pinned.clone());
                report.output.verified_probes = Some(captured.len());
                store_output(
                    state,
                    step.id,
                    json!({"probe_count": captured.len()}),
                    &mut report.output,
                )?;
                state.snapshots.insert(
                    step.id,
                    Checkpoint {
                        id: snapshot,
                        source: pinned,
                        probes: captured,
                    },
                );
            }
            Operation::Revert { snapshot } => {
                if state.skipped.contains(snapshot) {
                    return Err(Stop::DependencySkipped);
                }
                let checkpoint = state
                    .snapshots
                    .get(snapshot)
                    .cloned()
                    .ok_or(Stop::Failed(FailureCode::InvalidReference))?;
                if !backend.is_current(token) {
                    return Err(Stop::Failed(FailureCode::StaleContext));
                }
                if cancel.is_cancelled() {
                    return Err(Stop::Cancelled);
                }
                let reverted = backend.revert(token, &checkpoint.id, cancel).await;
                let accepted = match reverted {
                    ControlOutcome::Confirmed(true) => true,
                    ControlOutcome::Cancelled => return Err(Stop::Cancelled),
                    ControlOutcome::Confirmed(false) => {
                        report.failure = Some(FailureCode::RevertRejected);
                        false
                    }
                    ControlOutcome::Failed(error) => {
                        report.failure = Some(error);
                        false
                    }
                    ControlOutcome::Unknown(reason) => {
                        report.unknown = Some(reason);
                        false
                    }
                };
                if !accepted {
                    report.status = StepStatus::RollbackFailed;
                    return Ok(());
                }
                // Once revert was accepted, finish read-back even if cancellation was requested.
                let verification: Result<(), Stop> = async {
                    if source(backend, token, None, None).await? != checkpoint.source {
                        return Err(Stop::Failed(FailureCode::VerificationFailed));
                    }
                    for (address, slot, expected) in &checkpoint.probes {
                        let request = StorageRequest {
                            address: address.clone(),
                            slot: slot.clone(),
                            source: checkpoint.source.clone(),
                        };
                        if storage_read(backend, token, &request, None).await? != *expected {
                            return Err(Stop::Failed(FailureCode::VerificationFailed));
                        }
                    }
                    Ok(())
                }
                .await;
                if verification.is_err() {
                    report.status = StepStatus::RollbackFailed;
                    report.failure = Some(FailureCode::VerificationFailed);
                    return Ok(());
                }
                state.snapshots.remove(snapshot);
                report.output.source = Some(checkpoint.source);
                report.output.verified_probes = Some(checkpoint.probes.len());
                if checkpoint.probes.is_empty() {
                    report.status = StepStatus::RevertAcknowledgedUnverified;
                    return Ok(());
                }
                store_output(
                    state,
                    step.id,
                    json!({"verified_probes": checkpoint.probes.len()}),
                    &mut report.output,
                )?;
            }
            Operation::Assert {
                left,
                comparator,
                right,
            } => {
                let left = resolve(left, state)?;
                let right = right
                    .as_ref()
                    .map(|input| resolve(input, state))
                    .transpose()?;
                let passed = assertion_matches(*comparator, &left, right.as_ref())?;
                store_output(
                    state,
                    step.id,
                    json!({"passed": passed}),
                    &mut report.output,
                )?;
                if !passed {
                    return Err(Stop::Failed(FailureCode::AssertionFailed));
                }
            }
            Operation::Delay { milliseconds } => {
                tokio::select! { biased;
                    _ = cancel.cancelled() => return Err(Stop::Cancelled),
                    _ = tokio::time::sleep(Duration::from_millis(*milliseconds)) => {}
                }
                store_output(state, step.id, Value::Null, &mut report.output)?;
            }
            Operation::ProbeGasSweep {
                probe, router, data, token: asset, treasury, from,
                start, end, increment, max_attempts, outer_gas_margin, final_gas_margin,
            } => {
                let probe = resolve_address(probe, state)?;
                let router = resolve_address(router, state)?;
                let data = hex_bytes(&string(resolve(data, state)?)?, false, None)?;
                let asset = resolve_address(asset, state)?;
                let treasury = resolve_address(treasury, state)?;
                let from = resolve_address(from, state)?;
                let calldata = ethers::utils::hex::decode(&data[2..])
                    .map_err(|_| Stop::Failed(FailureCode::InvalidInput))?;
                let intrinsic = 21_000u64.checked_add(calldata.iter().try_fold(0u64, |sum, byte| {
                    sum.checked_add(if *byte == 0 { 4 } else { 16 })
                }).ok_or(Stop::Failed(FailureCode::InvalidInput))?)
                    .ok_or(Stop::Failed(FailureCode::InvalidInput))?;
                let pinned = source(backend, token, None, Some(cancel)).await?;
                report.output.source = Some(pinned.clone());
                let mut gas = *start;
                let mut attempts = 0u16;
                let mut selected = None;
                while gas <= *end && attempts < *max_attempts {
                    let request = ProbeGasRequest {
                        probe: probe.clone(), router: router.clone(), data: data.clone(),
                        token: asset.clone(), treasury: treasury.clone(), from: from.clone(),
                        gas, outer_gas: gas.checked_add(*outer_gas_margin)
                            .ok_or(Stop::Failed(FailureCode::InvalidInput))?,
                        source: pinned.clone(),
                    };
                    let outcome = probe_read(backend, token, &request, cancel).await?;
                    attempts += 1;
                    report.output.attempts = Some(attempts);
                    match outcome {
                        ReadOutcome::Value(value) if value.len() == 66 && value[2..].bytes().all(|b| b == b'0') => {
                            selected = Some(gas);
                            break;
                        }
                        ReadOutcome::Value(value) if value.len() == 66 => {}
                        ReadOutcome::Revert => return Err(Stop::Failed(FailureCode::Reverted)),
                        ReadOutcome::Failed(error) => return Err(Stop::Failed(error)),
                        _ => return Err(Stop::Failed(FailureCode::InvalidBackendResult)),
                    }
                    gas = match gas.checked_add(*increment) {
                        Some(next) => next,
                        None => break,
                    };
                }
                let selected = selected.ok_or(Stop::Failed(FailureCode::NoMatch))?;
                let exact_gas = selected.checked_add(*final_gas_margin)
                    .and_then(|gas| gas.checked_add(intrinsic))
                    .ok_or(Stop::Failed(FailureCode::InvalidInput))?;
                report.output.selected_gas = Some(exact_gas);
                store_output(state, step.id, json!({"gas": exact_gas, "attempts": attempts}), &mut report.output)?;
                state.sweeps.insert(step.id, SweepSelection {
                    transaction: RawTransaction { to: router, data, value: "0".into(), from: Some(from) },
                    source: pinned,
                    gas: exact_gas,
                });
            }
            Operation::GasSweep {
                transaction,
                start,
                end,
                increment,
                max_attempts,
                predicate,
            } => {
                let tx = resolve_transaction(transaction, state)?;
                let pinned = source(backend, token, None, Some(cancel)).await?;
                report.output.source = Some(pinned.clone());
                let mut gas = *start;
                let mut attempts = 0u16;
                let mut selected = None;
                while gas <= *end && attempts < *max_attempts {
                    let request = RawCallRequest {
                        transaction: tx.clone(),
                        gas: Some(gas),
                        source: pinned.clone(),
                    };
                    let outcome = raw_read(backend, token, &request, cancel).await?;
                    attempts += 1;
                    report.output.attempts = Some(attempts);
                    if predicate_matches(predicate, &outcome)? {
                        selected = Some(gas);
                        break;
                    }
                    let Some(next) = gas.checked_add(*increment) else {
                        break;
                    };
                    gas = next;
                }
                let selected = selected.ok_or(Stop::Failed(FailureCode::NoMatch))?;
                report.output.selected_gas = Some(selected);
                store_output(
                    state,
                    step.id,
                    json!({"gas": selected, "attempts": attempts}),
                    &mut report.output,
                )?;
                state.sweeps.insert(
                    step.id,
                    SweepSelection {
                        transaction: tx,
                        source: pinned,
                        gas: selected,
                    },
                );
            }
        }
        Ok(())
    }
    .await;
    if let Err(stop) = result {
        return stop_status(stop, report);
    }
    match report.status {
        StepStatus::Running => {
            report.status = StepStatus::Succeeded;
            false
        }
        StepStatus::Succeeded | StepStatus::Skipped => false,
        _ => true,
    }
}

/// Executes one plan in order. Observer events contain summaries, never inputs or
/// raw results. A mutation already in flight is awaited rather than abandoned.
pub async fn execute<B, F>(
    plan: ExecutionPlan,
    backend: &B,
    cancel: &CancellationToken,
    mut observer: F,
) -> Result<RunReport, RunIdsExhausted>
where
    B: MacroBackend + ?Sized,
    F: FnMut(&RunEvent) + Send,
{
    let id = NEXT_RUN_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .map(RunId)
        .map_err(|_| RunIdsExhausted)?;
    let validation = validate_plan(&plan);
    let mut report = RunReport {
        id,
        token: plan.token.clone(),
        dry_run: plan.dry_run,
        status: RunStatus::Completed,
        failure: None,
        steps: plan
            .steps
            .iter()
            .take(MAX_STEPS)
            .map(|step| StepReport {
                id: step.id,
                kind: kind(&step.operation),
                status: StepStatus::Pending,
                failure: None,
                unknown: None,
                skipped: None,
                output: OutputSummary::default(),
            })
            .collect(),
    };
    observer(&RunEvent::Started {
        id,
        token: plan.token.clone(),
        dry_run: plan.dry_run,
    });
    let mut state = State {
        outputs: HashMap::new(),
        skipped: HashSet::new(),
        snapshots: HashMap::new(),
        sweeps: HashMap::new(),
        remaining_output: MAX_TOTAL_OUTPUT_BYTES,
    };
    if let Err(error) = validation {
        report.status = RunStatus::InvalidPlan;
        report.failure = Some(error);
        for step in &mut report.steps {
            step.status = StepStatus::Skipped;
            step.skipped = Some(SkipReason::Stopped);
            observer(&RunEvent::Step {
                id,
                step: step.clone(),
            });
        }
        observer(&RunEvent::Finished {
            id,
            status: report.status,
        });
        return Ok(report);
    }
    let needs_lease = !plan.dry_run && plan.steps.iter().any(|step| mutates(&step.operation));
    let _lease = if needs_lease {
        match read_future(
            backend.acquire_mutation_lease(&plan.token, cancel),
            Some(cancel),
        )
        .await
        {
            Ok(Ok(lease)) if backend.is_current(&plan.token) && !cancel.is_cancelled() => {
                Some(lease)
            }
            outcome => {
                let (status, failure) = match outcome {
                    Err(Stop::Cancelled) => (RunStatus::Cancelled, None),
                    _ if cancel.is_cancelled() => (RunStatus::Cancelled, None),
                    Ok(Err(error)) => (RunStatus::Failed, Some(error)),
                    _ => (RunStatus::Failed, Some(FailureCode::StaleContext)),
                };
                report.status = status;
                report.failure = failure;
                for step in &mut report.steps {
                    step.status = StepStatus::Skipped;
                    step.skipped = Some(SkipReason::Stopped);
                    observer(&RunEvent::Step {
                        id,
                        step: step.clone(),
                    });
                }
                observer(&RunEvent::Finished { id, status });
                return Ok(report);
            }
        }
    } else {
        None
    };
    let mut stopped = false;
    for (step, result) in plan.steps.iter().zip(report.steps.iter_mut()) {
        let was_stopped = stopped;
        if stopped {
            result.status = StepStatus::Skipped;
            result.skipped = Some(SkipReason::Stopped);
        } else if cancel.is_cancelled() {
            result.status = StepStatus::Cancelled;
            stopped = true;
        } else if !backend.is_current(&plan.token) {
            result.status = StepStatus::Failed;
            result.failure = Some(FailureCode::StaleContext);
            stopped = true;
        } else if plan.dry_run && mutates(&step.operation) {
            result.status = StepStatus::Skipped;
            result.skipped = Some(SkipReason::DryRunMutation);
            state.skipped.insert(step.id);
        } else {
            result.status = StepStatus::Running;
            observer(&RunEvent::Step {
                id,
                step: result.clone(),
            });
            stopped = execute_step(
                backend,
                &plan.token,
                cancel,
                plan.dry_run,
                step,
                &mut state,
                result,
            )
            .await;
            if result.status == StepStatus::Skipped {
                state.skipped.insert(step.id);
            }
        }
        observer(&RunEvent::Step {
            id,
            step: result.clone(),
        });
        if stopped && !was_stopped {
            report.status = match result.status {
                StepStatus::Failed => RunStatus::Failed,
                StepStatus::Unknown => RunStatus::Unknown,
                StepStatus::Cancelled => RunStatus::Cancelled,
                StepStatus::RevertAcknowledgedUnverified => RunStatus::RevertAcknowledgedUnverified,
                StepStatus::RollbackFailed => RunStatus::RollbackFailed,
                _ => RunStatus::Failed,
            };
        }
    }
    if !stopped && plan.dry_run {
        report.status = RunStatus::PreflightComplete;
    }
    observer(&RunEvent::Finished {
        id,
        status: report.status,
    });
    Ok(report)
}

/// What a run left on the chain, in plain words: how many transactions were
/// confirmed (each is its own transaction; a later failure does not undo
/// them), whether any outcome is unknown, and whether a revert restored state.
pub fn chain_effect(report: &RunReport) -> String {
    use StepKind::*;
    let sends = |step: &&StepReport| matches!(step.kind, Deploy | AbiCall | RawSend | GasSweep | ProbeGasSweep);
    let confirmed = report.steps.iter().filter(sends).filter(|step| step.output.submission == Some(SubmissionState::Confirmed)).count();
    let unknown = report.steps.iter().filter(sends).filter(|step| step.output.submission == Some(SubmissionState::Unknown) || step.status == StepStatus::Unknown).count();
    let reverted = report.steps.iter().any(|step| step.kind == Revert && step.status == StepStatus::Succeeded);
    let unverified = report.steps.iter().any(|step| step.status == StepStatus::RevertAcknowledgedUnverified);
    if report.dry_run {
        return "Dry run: nothing was signed or sent. Reads ran on current state; steps that write (and steps that need their output) were skipped, so this is not a simulation of the whole sequence.".into();
    }
    let mut lines = vec![match confirmed {
        0 => "No transaction was confirmed.".to_string(),
        1 => "1 transaction was confirmed on the workspace chain.".to_string(),
        n => format!("{n} transactions were confirmed on the workspace chain, each separately: the run is not atomic."),
    }];
    if matches!(report.status, RunStatus::Failed | RunStatus::Cancelled | RunStatus::Unknown) && confirmed > 0 && !reverted {
        lines.push("The run stopped early; confirmed transactions stay applied. Revert to a snapshot (a Revert step or Explorer > Timeline) to undo them.".into());
    }
    if unknown > 0 {
        lines.push(format!("{unknown} submission(s) have an unknown outcome; check the chain before running again."));
    }
    if reverted {
        lines.push("A Revert step restored an earlier snapshot, verified by its probes.".into());
    } else if unverified {
        lines.push("A revert was acknowledged by the node but not verified by probes; check state before relying on it.".into());
    }
    lines.join("\n")
}

/// Evidence intentionally omits inputs, outputs, signer aliases, RPC handles,
/// snapshot IDs, and backend messages. Digests establish integrity, not truth.
/// The digest covers compact JSON for `evidence` with object keys sorted.
pub fn export_evidence(report: &RunReport) -> Value {
    let steps: Vec<Value> = report.steps.iter().map(|step| {
        let source = step.output.source.clone().and_then(|value| canonical_source(value).ok());
        json!({"id": step.id, "kind": step.kind, "status": step.status, "failure": step.failure,
            "unknown": step.unknown, "skipped": step.skipped,
            "output": {"digest": step.output.digest.as_deref().and_then(valid_hash),
                "encoded_bytes": step.output.encoded_bytes, "transaction_hash": step.output.transaction_hash.as_deref().and_then(valid_hash),
                "submission": step.output.submission, "selected_gas": step.output.selected_gas,
                "attempts": step.output.attempts, "verified_probes": step.output.verified_probes, "source": source}})
    }).collect();
    let evidence = json!({"schema": "chasm.macro-evidence.v1", "run_id": report.id,
        "workspace_epoch": report.token.workspace_epoch, "network_epoch": report.token.network_epoch,
        "dry_run": report.dry_run, "status": report.status, "failure": report.failure, "steps": steps});
    let mut encoded = String::new();
    canonical_json(&evidence, &mut encoded);
    json!({"evidence": evidence, "digest": {"algorithm": "keccak256", "value": digest(encoded.as_bytes())}})
}

#[cfg(test)]
#[path = "macro_runner_tests.rs"]
mod tests;
