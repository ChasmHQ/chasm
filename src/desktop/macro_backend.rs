use std::{sync::Arc, time::Duration};

use ethers::{
    abi::{Abi, FunctionExt},
    types::U256,
};
use futures::future::BoxFuture;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::{call_result::CallResult, chain_read, contract_service, macro_runner as runner};
use super::{mutation_gate::MutationGate, owned_node::OwnedNode};

const LOCAL_RPC: &str = "http://127.0.0.1:8545";
const CONTROL_TIMEOUT: Duration = Duration::from_secs(10);
const CONTROL_LIMIT: usize = 16 * 1024;
const MAX_CREATION_HEX: usize = 8 * 1024 * 1024;
const ACCOUNT_HANDLES: [runner::SignerAlias; 3] = [
    runner::SignerAlias::Handle(0),
    runner::SignerAlias::Handle(1),
    runner::SignerAlias::Handle(2),
];

#[derive(Clone)]
pub struct SignerAddresses {
    accounts: [String; 3],
    active: Option<String>,
}

impl SignerAddresses {
    pub fn new(accounts: [String; 3], active: Option<String>) -> Result<Self, runner::FailureCode> {
        if accounts
            .iter()
            .chain(active.iter())
            .any(|address| !valid_address(address))
        {
            return Err(runner::FailureCode::InvalidInput);
        }
        Ok(Self {
            accounts: accounts.map(|address| address.to_ascii_lowercase()),
            active: active.map(|address| address.to_ascii_lowercase()),
        })
    }

    pub fn address(&self, alias: runner::SignerAlias) -> Option<&str> {
        match alias {
            runner::SignerAlias::Active => self.active.as_deref(),
            runner::SignerAlias::Handle(index) => usize::try_from(index)
                .ok()
                .and_then(|index| self.accounts.get(index))
                .map(String::as_str),
        }
    }
}

/// Supplies a fresh key only to an in-process signing call. Implementations must
/// bind aliases for the entire run and never log or persist the returned key.
pub trait SignerResolver: Send + Sync {
    fn address(&self, alias: runner::SignerAlias) -> Option<String>;
    fn key(&self, alias: runner::SignerAlias) -> Option<String>;
}

/// Read the live desktop context, not a snapshot captured at run creation.
/// A token must be replaced whenever the owned process or endpoint changes.
pub trait ContextAuthority: Send + Sync {
    fn is_current(&self, token: &runner::ExecutionToken) -> bool;
}

/// Implement this boundary inside contract_service, where transaction signing
/// and the final pre-submission checks are available. An error is safe only if
/// submission definitely did not begin; all later ambiguity is a CallResult
/// marked unknown. The callback must be checked immediately before signing and
/// again immediately before submission, after any awaited RPC.
pub trait GuardedSubmission: Send + Sync {
    fn deploy<'a>(
        &'a self,
        guard: &'a (dyn Fn() -> bool + Send + Sync),
        owned: Arc<OwnedNode>,
        key: String,
        creation: &'a str,
        value: &'a str,
        exact_gas: u64,
    ) -> BoxFuture<'a, Result<CallResult, ()>>;
    fn abi_send<'a>(
        &'a self,
        guard: &'a (dyn Fn() -> bool + Send + Sync),
        owned: Arc<OwnedNode>,
        key: String,
        request: contract_service::Request,
        exact_gas: u64,
    ) -> BoxFuture<'a, Result<CallResult, ()>>;
    /// `expected_source`, when present, must be rechecked against the current
    /// head after all pre-submission awaits and immediately before broadcast.
    fn raw_send<'a>(
        &'a self,
        guard: &'a (dyn Fn() -> bool + Send + Sync),
        owned: Arc<OwnedNode>,
        key: String,
        request: &'a runner::RawSendRequest,
    ) -> BoxFuture<'a, Result<CallResult, ()>>;
}

/// Typed in-process control boundary. The implementation must use only the
/// fixed owned loopback endpoint, bounded requests/responses, no redirects, and
/// the same final guard immediately before dispatch. It must return an envelope
/// with the request id 1, without exposing provider text. Any transport error
/// after dispatch is unknown, not a definite failure or cancellation.
pub trait OwnedControls: Send + Sync {
    fn snapshot<'a>(
        &'a self,
        guard: &'a (dyn Fn() -> bool + Send + Sync),
    ) -> BoxFuture<'a, Result<Value, ControlTransport>>;
    fn revert<'a>(
        &'a self,
        guard: &'a (dyn Fn() -> bool + Send + Sync),
        private_id: &'a str,
    ) -> BoxFuture<'a, Result<Value, ControlTransport>>;
    /// `evm_setAutomine` / `evm_mine` for bundles. Default: unavailable.
    fn automine<'a>(&'a self, _guard: &'a (dyn Fn() -> bool + Send + Sync), _enabled: bool) -> BoxFuture<'a, Result<Value, ControlTransport>> {
        Box::pin(async { Err(ControlTransport::NotDispatched) })
    }
    fn mine<'a>(&'a self, _guard: &'a (dyn Fn() -> bool + Send + Sync)) -> BoxFuture<'a, Result<Value, ControlTransport>> {
        Box::pin(async { Err(ControlTransport::NotDispatched) })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlTransport {
    NotDispatched,
    Unknown,
}

/// The integration traits are deliberately mandatory: the current service
/// cannot yet prove epochs at submission or expose its private raw-send path.
pub struct DesktopMacroBackend<A, S, T, C> {
    authority: A,
    signers: S,
    submissions: T,
    controls: C,
    owned: Arc<OwnedNode>,
    gate: MutationGate,
    token: runner::ExecutionToken,
    addresses: SignerAddresses,
    /// Bundle submissions waiting for their block: hash -> (result, creation).
    pending: std::sync::Mutex<std::collections::HashMap<String, (CallResult, bool)>>,
}

impl<A: ContextAuthority, S: SignerResolver, T: GuardedSubmission, C: OwnedControls>
    DesktopMacroBackend<A, S, T, C>
{
    pub fn new(
        authority: A,
        signers: S,
        submissions: T,
        controls: C,
        owned: Arc<OwnedNode>,
        gate: MutationGate,
        token: runner::ExecutionToken,
        addresses: SignerAddresses,
    ) -> Result<Self, runner::FailureCode> {
        if token.rpc_token == 0 || !authority.is_current(&token) || !owned.authorizes_loopback(8545)
        {
            return Err(runner::FailureCode::Unauthorized);
        }
        for alias in ACCOUNT_HANDLES
            .into_iter()
            .chain([runner::SignerAlias::Active])
        {
            let expected = addresses.address(alias);
            let actual = signers.address(alias);
            if expected != actual.as_deref().map(str::to_ascii_lowercase).as_deref() {
                return Err(runner::FailureCode::InvalidInput);
            }
        }
        Ok(Self {
            authority,
            signers,
            submissions,
            controls,
            owned,
            gate,
            token,
            addresses,
            pending: Default::default(),
        })
    }

    /// A submission result: kept for later when the node only queued it.
    fn submitted(&self, result: Result<CallResult, ()>, deployment: bool) -> runner::MutationOutcome {
        if let Ok(result) = &result {
            if result.context.request["submissionState"] == "pending" {
                if let Some(hash) = result.context.request["transactionHash"].as_str().filter(|hash| valid_hex(hash, 32)).map(str::to_ascii_lowercase) {
                    self.pending.lock().unwrap().insert(hash.clone(), (result.clone(), deployment));
                    return runner::MutationOutcome::Pending { tx_hash: hash };
                }
            }
        }
        map_submission(result, deployment)
    }

    fn current(&self, token: &runner::ExecutionToken) -> bool {
        token == &self.token
            && self.authority.is_current(token)
            && self.owned.authorizes_loopback(8545)
    }

    fn key(
        &self,
        alias: runner::SignerAlias,
        from: Option<&str>,
    ) -> Result<String, runner::FailureCode> {
        use ethers::signers::Signer;
        let expected = self
            .addresses
            .address(alias)
            .ok_or(runner::FailureCode::Unavailable)?;
        if from.is_some_and(|from| !expected.eq_ignore_ascii_case(from)) {
            return Err(runner::FailureCode::InvalidInput);
        }
        let key = self
            .signers
            .key(alias)
            .ok_or(runner::FailureCode::Unavailable)?;
        let address = match super::contract_service::impersonated(&key) {
            Some(address) => address,
            None => key.parse::<ethers::signers::LocalWallet>().map_err(|_| runner::FailureCode::Unavailable)?.address(),
        };
        if !format!("{address:#x}").eq_ignore_ascii_case(expected) {
            return Err(runner::FailureCode::Unauthorized);
        }
        Ok(key)
    }

    async fn chain_query(
        &self,
        token: &runner::ExecutionToken,
        request: chain_read::ReadRequest,
    ) -> Result<Value, runner::FailureCode> {
        if !self.current(token) {
            return Err(runner::FailureCode::StaleContext);
        }
        let result = chain_read::query(LOCAL_RPC, request)
            .await
            .map_err(read_error)?;
        if !self.current(token) {
            return Err(runner::FailureCode::StaleContext);
        }
        Ok(result)
    }

    async fn get_source(
        &self,
        token: &runner::ExecutionToken,
        block: Option<u64>,
    ) -> Result<runner::SourceIdentity, runner::FailureCode> {
        let selector = block
            .map(chain_read::BlockRef::Number)
            .unwrap_or(chain_read::BlockRef::Latest);
        let value = self
            .chain_query(
                token,
                chain_read::ReadRequest::Block {
                    selector,
                    full_tx: false,
                },
            )
            .await?;
        source_from_envelope(&value)
    }

    async fn read_transaction(
        &self,
        token: &runner::ExecutionToken,
        transaction: &runner::RawTransaction,
        gas: Option<u64>,
        source: &runner::SourceIdentity,
    ) -> runner::ReadOutcome {
        let call = chain_read::CallRequest {
            from: transaction.from.clone(),
            to: Some(transaction.to.clone()),
            data: Some(transaction.data.clone()),
            value: Some(transaction.value.clone()),
            gas: gas.map(|gas| gas.to_string()),
            gas_price: None,
        };
        match self
            .chain_query(
                token,
                chain_read::ReadRequest::Simulate {
                    call,
                    block: chain_read::BlockRef::Number(source.block_number),
                },
            )
            .await
        {
            Ok(value) => {
                if source_from_envelope(&value).ok().as_ref() != Some(source) {
                    return runner::ReadOutcome::Failed(runner::FailureCode::SourceChanged);
                }
                match value.pointer("/data/return_data").and_then(Value::as_str) {
                    Some(data) if valid_data(data) => {
                        runner::ReadOutcome::Value(data.to_ascii_lowercase())
                    }
                    _ => runner::ReadOutcome::Failed(runner::FailureCode::InvalidBackendResult),
                }
            }
            Err(runner::FailureCode::Reverted) => runner::ReadOutcome::Revert,
            Err(error) => runner::ReadOutcome::Failed(error),
        }
    }

    fn prepare_abi(
        &self,
        token: &runner::ExecutionToken,
        request: &runner::AbiRequest,
    ) -> Result<contract_service::Request, runner::FailureCode> {
        let abi: Abi = serde_json::from_value(request.abi.clone())
            .map_err(|_| runner::FailureCode::InvalidInput)?;
        let mut functions = abi
            .functions()
            .filter(|function| function.abi_signature() == request.signature);
        functions.next().ok_or(runner::FailureCode::InvalidInput)?;
        if functions.next().is_some() {
            return Err(runner::FailureCode::InvalidInput);
        }
        let arguments = serde_json::to_string(&request.arguments)
            .map_err(|_| runner::FailureCode::InvalidInput)?;
        let mut prepared = contract_service::prepare(
            "",
            &request.address,
            &request.signature,
            &abi,
            &arguments,
            &request.value,
            request.from.as_deref().unwrap_or(""),
            token.workspace_epoch,
            token.network_epoch,
            LOCAL_RPC.into(),
        )
        .map_err(|_| runner::FailureCode::InvalidInput)?;
        if let Some(gas) = request.gas {
            if gas == 0 {
                return Err(runner::FailureCode::InvalidInput);
            }
            prepared.call["gas"] = json!(format!("{gas:#x}"));
        } else {
            prepared
                .call
                .as_object_mut()
                .ok_or(runner::FailureCode::InvalidInput)?
                .remove("gas");
        }
        Ok(prepared)
    }
}

impl<A: ContextAuthority, S: SignerResolver, T: GuardedSubmission, C: OwnedControls>
    runner::MacroBackend for DesktopMacroBackend<A, S, T, C>
{
    fn is_current(&self, token: &runner::ExecutionToken) -> bool {
        self.current(token)
    }

    fn acquire_mutation_lease<'a>(
        &'a self,
        token: &'a runner::ExecutionToken,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<Box<dyn runner::MutationLease + 'a>, runner::FailureCode>> {
        Box::pin(async move {
            if !self.current(token) {
                return Err(runner::FailureCode::StaleContext);
            }
            let lease = self
                .gate
                .acquire(cancel)
                .await
                .ok_or(runner::FailureCode::Unavailable)?;
            if !self.current(token) {
                return Err(runner::FailureCode::StaleContext);
            }
            Ok(Box::new(lease) as Box<dyn runner::MutationLease>)
        })
    }

    fn source<'a>(
        &'a self,
        token: &'a runner::ExecutionToken,
        block: Option<u64>,
    ) -> BoxFuture<'a, Result<runner::SourceIdentity, runner::FailureCode>> {
        Box::pin(async move { self.get_source(token, block).await })
    }

    fn deploy<'a>(
        &'a self,
        token: &'a runner::ExecutionToken,
        creation: &'a str,
        value: &'a str,
        exact_gas: u64,
        signer: runner::SignerAlias,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, runner::MutationOutcome> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return runner::MutationOutcome::Cancelled;
            }
            if !self.current(token) {
                return runner::MutationOutcome::Failed(runner::FailureCode::StaleContext);
            }
            if exact_gas == 0 || !valid_creation(creation) {
                return runner::MutationOutcome::Failed(runner::FailureCode::InvalidInput);
            }
            let key = match self.key(signer, None) {
                Ok(key) => key,
                Err(error) => return runner::MutationOutcome::Failed(error),
            };
            let guard = || self.current(token) && !cancel.is_cancelled();
            self.submitted(
                self.submissions
                    .deploy(&guard, self.owned.clone(), key, creation, value, exact_gas)
                    .await,
                true,
            )
        })
    }

    fn abi_call<'a>(
        &'a self,
        token: &'a runner::ExecutionToken,
        request: &'a runner::AbiRequest,
        source: &'a runner::SourceIdentity,
    ) -> BoxFuture<'a, runner::ReadOutcome> {
        Box::pin(async move {
            let prepared = match self.prepare_abi(token, request) {
                Ok(value) => value,
                Err(error) => return runner::ReadOutcome::Failed(error),
            };
            let Some(data) = prepared.call.get("data").and_then(Value::as_str) else {
                return runner::ReadOutcome::Failed(runner::FailureCode::InvalidInput);
            };
            let transaction = runner::RawTransaction {
                to: request.address.clone(),
                data: data.to_owned(),
                value: request.value.clone(),
                from: request.from.clone(),
            };
            self.read_transaction(token, &transaction, request.gas, source)
                .await
        })
    }

    fn abi_send<'a>(
        &'a self,
        token: &'a runner::ExecutionToken,
        request: &'a runner::AbiRequest,
        signer: runner::SignerAlias,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, runner::MutationOutcome> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return runner::MutationOutcome::Cancelled;
            }
            if !self.current(token) {
                return runner::MutationOutcome::Failed(runner::FailureCode::StaleContext);
            }
            let Some(exact_gas) = request.gas.filter(|gas| *gas > 0) else {
                return runner::MutationOutcome::Failed(runner::FailureCode::InvalidInput);
            };
            let key = match self.key(signer, request.from.as_deref()) {
                Ok(key) => key,
                Err(error) => return runner::MutationOutcome::Failed(error),
            };
            let prepared = match self.prepare_abi(token, request) {
                Ok(value) => value,
                Err(error) => return runner::MutationOutcome::Failed(error),
            };
            let guard = || self.current(token) && !cancel.is_cancelled();
            self.submitted(
                self.submissions
                    .abi_send(&guard, self.owned.clone(), key, prepared, exact_gas)
                    .await,
                false,
            )
        })
    }

    fn raw_call<'a>(
        &'a self,
        token: &'a runner::ExecutionToken,
        request: &'a runner::RawCallRequest,
    ) -> BoxFuture<'a, runner::ReadOutcome> {
        Box::pin(async move {
            self.read_transaction(token, &request.transaction, request.gas, &request.source)
                .await
        })
    }

    fn probe_gas<'a>(
        &'a self,
        token: &'a runner::ExecutionToken,
        request: &'a runner::ProbeGasRequest,
    ) -> BoxFuture<'a, runner::ReadOutcome> {
        Box::pin(async move {
            if !self.current(token) || !valid_address(&request.probe) || !valid_address(&request.router)
                || !valid_address(&request.token) || !valid_address(&request.treasury)
                || !valid_address(&request.from) || !valid_data(&request.data)
                || request.data == "0x" || request.gas == 0 || request.outer_gas <= request.gas
                || request.outer_gas > request.gas.saturating_add(4_000_000)
            {
                return runner::ReadOutcome::Failed(runner::FailureCode::InvalidInput);
            }
            let function = match ethers::abi::parse_abi(&["function probe(address,uint256,bytes,address,address) returns (uint256)"]) {
                Ok(abi) => abi,
                Err(_) => return runner::ReadOutcome::Failed(runner::FailureCode::InvalidInput),
            };
            let Some(function) = function.function("probe").ok() else {
                return runner::ReadOutcome::Failed(runner::FailureCode::InvalidInput);
            };
            let Some(bytes) = request.data.strip_prefix("0x").and_then(|digits| ethers::utils::hex::decode(digits).ok()) else {
                return runner::ReadOutcome::Failed(runner::FailureCode::InvalidInput);
            };
            let address = |text: &str| text.parse::<ethers::types::Address>()
                .map(ethers::abi::Token::Address);
            let encoded = [
                address(&request.router),
                Ok(ethers::abi::Token::Uint(U256::from(request.gas))),
                Ok(ethers::abi::Token::Bytes(bytes)),
                address(&request.token),
                address(&request.treasury),
            ];
            let Ok(tokens) = encoded.into_iter().collect::<Result<Vec<_>, _>>() else {
                return runner::ReadOutcome::Failed(runner::FailureCode::InvalidInput);
            };
            let Ok(data) = function.encode_input(&tokens) else {
                return runner::ReadOutcome::Failed(runner::FailureCode::InvalidInput);
            };
            let tx = runner::RawTransaction {
                to: request.probe.clone(),
                data: format!("0x{}", ethers::utils::hex::encode(data)),
                value: "0".into(),
                from: Some(request.from.clone()),
            };
            self.read_transaction(token, &tx, Some(request.outer_gas), &request.source).await
        })
    }

    fn raw_send<'a>(
        &'a self,
        token: &'a runner::ExecutionToken,
        request: &'a runner::RawSendRequest,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, runner::MutationOutcome> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return runner::MutationOutcome::Cancelled;
            }
            if !self.current(token) {
                return runner::MutationOutcome::Failed(runner::FailureCode::StaleContext);
            }
            if request.exact_gas == 0 {
                return runner::MutationOutcome::Failed(runner::FailureCode::InvalidInput);
            }
            let key = match self.key(request.signer, request.transaction.from.as_deref()) {
                Ok(key) => key,
                Err(error) => return runner::MutationOutcome::Failed(error),
            };
            if let Some(source) = &request.expected_source {
                match self.get_source(token, None).await {
                    Ok(actual) if actual == *source => {}
                    Ok(_) => {
                        return runner::MutationOutcome::Failed(runner::FailureCode::SourceChanged)
                    }
                    Err(error) => return runner::MutationOutcome::Failed(error),
                }
            }
            let guard = || self.current(token) && !cancel.is_cancelled();
            self.submitted(
                self.submissions
                    .raw_send(&guard, self.owned.clone(), key, request)
                    .await,
                false,
            )
        })
    }

    fn storage<'a>(
        &'a self,
        token: &'a runner::ExecutionToken,
        request: &'a runner::StorageRequest,
    ) -> BoxFuture<'a, Result<String, runner::FailureCode>> {
        Box::pin(async move {
            let value = self
                .chain_query(
                    token,
                    chain_read::ReadRequest::Storage {
                        address: request.address.clone(),
                        slot: request.slot.clone(),
                        block: chain_read::BlockRef::Number(request.source.block_number),
                    },
                )
                .await?;
            if source_from_envelope(&value)? != request.source {
                return Err(runner::FailureCode::SourceChanged);
            }
            value
                .pointer("/data/value")
                .and_then(Value::as_str)
                .filter(|word| valid_hex(word, 32))
                .map(str::to_ascii_lowercase)
                .ok_or(runner::FailureCode::InvalidBackendResult)
        })
    }

    fn snapshot<'a>(
        &'a self,
        token: &'a runner::ExecutionToken,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, runner::ControlOutcome<String>> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return runner::ControlOutcome::Cancelled;
            }
            if !self.current(token) {
                return runner::ControlOutcome::Failed(runner::FailureCode::StaleContext);
            }
            let guard = || self.current(token) && !cancel.is_cancelled();
            match tokio::time::timeout(CONTROL_TIMEOUT, self.controls.snapshot(&guard)).await {
                Ok(Ok(value)) => parse_snapshot(value),
                Ok(Err(ControlTransport::NotDispatched)) => {
                    runner::ControlOutcome::Failed(runner::FailureCode::Unavailable)
                }
                _ => runner::ControlOutcome::Unknown(runner::UnknownReason::Control),
            }
        })
    }

    fn revert<'a>(
        &'a self,
        token: &'a runner::ExecutionToken,
        snapshot: &'a str,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, runner::ControlOutcome<bool>> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return runner::ControlOutcome::Cancelled;
            }
            if !self.current(token) {
                return runner::ControlOutcome::Failed(runner::FailureCode::StaleContext);
            }
            if !valid_quantity(snapshot) {
                return runner::ControlOutcome::Failed(runner::FailureCode::InvalidInput);
            }
            let guard = || self.current(token) && !cancel.is_cancelled();
            match tokio::time::timeout(CONTROL_TIMEOUT, self.controls.revert(&guard, snapshot))
                .await
            {
                Ok(Ok(value)) => parse_revert(value),
                Ok(Err(ControlTransport::NotDispatched)) => {
                    runner::ControlOutcome::Failed(runner::FailureCode::Unavailable)
                }
                _ => runner::ControlOutcome::Unknown(runner::UnknownReason::Control),
            }
        })
    }

    fn set_automine<'a>(&'a self, token: &'a runner::ExecutionToken, enabled: bool) -> BoxFuture<'a, runner::ControlOutcome<bool>> {
        Box::pin(async move {
            if !self.current(token) {
                return runner::ControlOutcome::Failed(runner::FailureCode::StaleContext);
            }
            let guard = || self.current(token);
            match tokio::time::timeout(CONTROL_TIMEOUT, self.controls.automine(&guard, enabled)).await {
                Ok(Ok(value)) if envelope(&value).is_some() => runner::ControlOutcome::Confirmed(true),
                Ok(Err(ControlTransport::NotDispatched)) => runner::ControlOutcome::Failed(runner::FailureCode::Unavailable),
                _ => runner::ControlOutcome::Unknown(runner::UnknownReason::Control),
            }
        })
    }

    fn mine<'a>(&'a self, token: &'a runner::ExecutionToken) -> BoxFuture<'a, runner::ControlOutcome<bool>> {
        Box::pin(async move {
            if !self.current(token) {
                return runner::ControlOutcome::Failed(runner::FailureCode::StaleContext);
            }
            let guard = || self.current(token);
            match tokio::time::timeout(CONTROL_TIMEOUT, self.controls.mine(&guard)).await {
                Ok(Ok(value)) if envelope(&value).is_some() => runner::ControlOutcome::Confirmed(true),
                Ok(Err(ControlTransport::NotDispatched)) => runner::ControlOutcome::Failed(runner::FailureCode::Unavailable),
                _ => runner::ControlOutcome::Unknown(runner::UnknownReason::Control),
            }
        })
    }

    fn confirm_pending<'a>(&'a self, token: &'a runner::ExecutionToken, tx_hash: &'a str) -> BoxFuture<'a, runner::MutationOutcome> {
        Box::pin(async move {
            let Some((pending, deployment)) = self.pending.lock().unwrap().remove(&tx_hash.to_ascii_lowercase()) else {
                return runner::MutationOutcome::Unknown { tx_hash: Some(tx_hash.to_string()), reason: runner::UnknownReason::InvalidConfirmation };
            };
            if !self.current(token) {
                return runner::MutationOutcome::Unknown { tx_hash: Some(tx_hash.to_string()), reason: runner::UnknownReason::Receipt };
            }
            map_submission(Ok(contract_service::finish_bundled(pending, deployment).await), deployment)
        })
    }
}

fn valid_hex(text: &str, bytes: usize) -> bool {
    text.strip_prefix("0x").is_some_and(|digits| {
        digits.len() == bytes * 2 && digits.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}
fn valid_address(text: &str) -> bool {
    valid_hex(text, 20)
}
fn valid_data(text: &str) -> bool {
    text.len() <= 64 * 1024
        && text.strip_prefix("0x").is_some_and(|digits| {
            digits.len() % 2 == 0 && digits.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}
fn valid_creation(text: &str) -> bool {
    text.len() <= MAX_CREATION_HEX
        && text.strip_prefix("0x").is_some_and(|digits| {
            !digits.is_empty()
                && digits.len() % 2 == 0
                && digits.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}
fn valid_quantity(text: &str) -> bool {
    text.len() <= 66
        && text.strip_prefix("0x").is_some_and(|digits| {
            !digits.is_empty()
                && digits.bytes().all(|byte| byte.is_ascii_hexdigit())
                && U256::from_str_radix(digits, 16).is_ok()
        })
}

fn source_from_envelope(value: &Value) -> Result<runner::SourceIdentity, runner::FailureCode> {
    let chain = value
        .get("chain_id")
        .and_then(Value::as_str)
        .and_then(|value| U256::from_dec_str(value).ok())
        .ok_or(runner::FailureCode::InvalidBackendResult)?;
    if chain != U256::from(31337) {
        return Err(runner::FailureCode::Unauthorized);
    }
    let genesis = value
        .get("genesis_block_hash")
        .and_then(Value::as_str)
        .filter(|value| valid_hex(value, 32))
        .ok_or(runner::FailureCode::InvalidBackendResult)?;
    let number = value
        .pointer("/block/number")
        .and_then(Value::as_str)
        .filter(|value| valid_quantity(value))
        .and_then(|value| U256::from_str_radix(&value[2..], 16).ok())
        .filter(|value| *value <= U256::from(u64::MAX))
        .ok_or(runner::FailureCode::InvalidBackendResult)?
        .as_u64();
    let hash = value
        .pointer("/block/hash")
        .and_then(Value::as_str)
        .filter(|value| valid_hex(value, 32))
        .ok_or(runner::FailureCode::InvalidBackendResult)?;
    Ok(runner::SourceIdentity {
        chain_id: "0x7a69".into(),
        genesis_hash: genesis.to_ascii_lowercase(),
        block_number: number,
        block_hash: hash.to_ascii_lowercase(),
    })
}

fn read_error(error: chain_read::ReadError) -> runner::FailureCode {
    use chain_read::ReadErrorCode as R;
    match error.code {
        R::InvalidInput | R::InvalidUrl | R::MethodNotAllowed => runner::FailureCode::InvalidInput,
        R::Network | R::Timeout | R::HttpError => runner::FailureCode::Transport,
        R::Reorg => runner::FailureCode::SourceChanged,
        R::NotFound | R::Incomplete | R::TxPending => runner::FailureCode::Unavailable,
        R::RpcError => runner::FailureCode::Rpc,
        R::CallReverted => runner::FailureCode::Reverted,
        R::TooLarge | R::BadResponse => runner::FailureCode::InvalidBackendResult,
    }
}

fn envelope(value: &Value) -> Option<&Value> {
    if serde_json::to_vec(value).ok()?.len() > CONTROL_LIMIT
        || value.get("jsonrpc")?.as_str()? != "2.0"
        || value.get("id")?.as_u64()? != 1
        || value.get("error").is_some()
    {
        return None;
    }
    value.get("result")
}

fn parse_snapshot(value: Value) -> runner::ControlOutcome<String> {
    match envelope(&value)
        .and_then(Value::as_str)
        .filter(|value| valid_quantity(value))
    {
        Some(id) => runner::ControlOutcome::Confirmed(id.to_ascii_lowercase()),
        None => runner::ControlOutcome::Unknown(runner::UnknownReason::InvalidConfirmation),
    }
}

fn parse_revert(value: Value) -> runner::ControlOutcome<bool> {
    match envelope(&value).and_then(Value::as_bool) {
        Some(value) => runner::ControlOutcome::Confirmed(value),
        None => runner::ControlOutcome::Unknown(runner::UnknownReason::InvalidConfirmation),
    }
}

fn map_submission(result: Result<CallResult, ()>, deployment: bool) -> runner::MutationOutcome {
    let result = match result {
        Ok(result) => result,
        Err(()) => return runner::MutationOutcome::Failed(runner::FailureCode::Unavailable),
    };
    let hash = result
        .context
        .request
        .get("transactionHash")
        .and_then(Value::as_str)
        .filter(|hash| valid_hex(hash, 32))
        .map(str::to_ascii_lowercase);
    let unknown = |reason| runner::MutationOutcome::Unknown {
        tx_hash: hash.clone(),
        reason,
    };
    match result
        .context
        .request
        .get("submissionState")
        .and_then(Value::as_str)
    {
        Some("unknown") => return unknown(runner::UnknownReason::Receipt),
        Some("confirmed") => {}
        _ => return unknown(runner::UnknownReason::InvalidConfirmation),
    }
    let Some(hash) = hash.clone() else {
        return unknown(runner::UnknownReason::InvalidConfirmation);
    };
    let receipt = &result.raw["result"];
    if result.raw.get("error").is_some()
        || !receipt
            .get("transactionHash")
            .and_then(Value::as_str)
            .is_some_and(|actual| actual.eq_ignore_ascii_case(&hash))
    {
        return unknown(runner::UnknownReason::InvalidConfirmation);
    }
    let source = &result.context.source;
    let source_chain = source.get("chainId").and_then(Value::as_str);
    let source_genesis = source.get("genesisHash").and_then(Value::as_str);
    let source_block = source.pointer("/block/number").and_then(Value::as_str);
    let receipt_block = receipt.get("blockNumber").and_then(Value::as_str);
    let source_hash = source.pointer("/block/hash").and_then(Value::as_str);
    let receipt_hash = receipt.get("blockHash").and_then(Value::as_str);
    let matching_block = source_block
        .zip(receipt_block)
        .is_some_and(|(source, receipt)| {
            valid_quantity(source)
                && valid_quantity(receipt)
                && U256::from_str_radix(&source[2..], 16).ok()
                    == U256::from_str_radix(&receipt[2..], 16).ok()
        });
    if !source_chain.is_some_and(|chain| {
        valid_quantity(chain)
            && U256::from_str_radix(&chain[2..], 16).ok() == Some(U256::from(31337))
    }) || !source_genesis.is_some_and(|hash| valid_hex(hash, 32))
        || !matching_block
        || !source_hash
            .zip(receipt_hash)
            .is_some_and(|(source, receipt)| {
                valid_hex(source, 32)
                    && valid_hex(receipt, 32)
                    && source.eq_ignore_ascii_case(receipt)
            })
    {
        return unknown(runner::UnknownReason::InvalidConfirmation);
    }
    let Some(status) = receipt
        .get("status")
        .and_then(Value::as_str)
        .filter(|value| valid_quantity(value))
        .and_then(|value| U256::from_str_radix(&value[2..], 16).ok())
    else {
        return unknown(runner::UnknownReason::InvalidConfirmation);
    };
    if status != U256::zero() && status != U256::one() {
        return unknown(runner::UnknownReason::InvalidConfirmation);
    }
    let quantity = |name: &str| -> Option<Option<String>> {
        match receipt.get(name) {
            None | Some(Value::Null) => Some(None),
            Some(Value::String(value)) if valid_quantity(value) => {
                Some(Some(value.to_ascii_lowercase()))
            }
            _ => None,
        }
    };
    let Some(block_number) = quantity("blockNumber") else {
        return unknown(runner::UnknownReason::InvalidConfirmation);
    };
    let Some(gas_used) = quantity("gasUsed") else {
        return unknown(runner::UnknownReason::InvalidConfirmation);
    };
    let address = if deployment && status == U256::one() {
        let Some(address) = receipt
            .get("contractAddress")
            .and_then(Value::as_str)
            .filter(|address| {
                valid_address(address) && address[2..].bytes().any(|byte| byte != b'0')
            })
        else {
            return unknown(runner::UnknownReason::InvalidConfirmation);
        };
        Some(address.to_ascii_lowercase())
    } else {
        None
    };
    runner::MutationOutcome::Confirmed(runner::ConfirmedMutation {
        tx_hash: hash,
        contract_address: address,
        reverted: status.is_zero(),
        block_number,
        gas_used,
    })
}

#[cfg(test)]
#[path = "macro_backend_tests.rs"]
mod tests;
