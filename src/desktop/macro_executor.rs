use futures::future::BoxFuture;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::macro_backend::{
    ContextAuthority, ControlTransport, DesktopMacroBackend, GuardedSubmission, OwnedControls,
    SignerAddresses, SignerResolver,
};
use super::macro_runner as runner;
use super::native_macro::{ExecutionFailure, ExecutionOutput, ExecutionRequest, MacroExecutor};
use super::native_foundry::FoundryController;
use super::session::{anvil_dev_account, DesktopState};

const LOCAL_RPC: &str = "http://127.0.0.1:8545";

#[derive(Clone)]
struct Authority {
    state: Arc<Mutex<DesktopState>>,
    token: runner::ExecutionToken,
}

impl ContextAuthority for Authority {
    fn is_current(&self, token: &runner::ExecutionToken) -> bool {
        if token != &self.token {
            return false;
        }
        let state = self.state.lock().unwrap();
        state.active
            && state.workspace_ready
            && !state.transition_pending()
            && state.workspace_epoch == token.workspace_epoch
            && state.network_epoch == token.network_epoch
            && state.rpc == LOCAL_RPC
            && state.owned_node.as_ref().is_some_and(|node| node.authorizes_loopback(8545))
    }
}

#[derive(Clone)]
struct Signers {
    active_key: Option<String>,
    active_address: Option<String>,
}

impl SignerResolver for Signers {
    fn address(&self, alias: runner::SignerAlias) -> Option<String> {
        match alias {
            runner::SignerAlias::Active => self.active_address.clone(),
            runner::SignerAlias::Handle(index) => anvil_dev_account(index as i32).map(|(address, _)| address.to_ascii_lowercase()),
        }
    }

    fn key(&self, alias: runner::SignerAlias) -> Option<String> {
        match alias {
            runner::SignerAlias::Active => self.active_key.clone(),
            runner::SignerAlias::Handle(index) => anvil_dev_account(index as i32).map(|(_, key)| key.to_string()),
        }
    }
}

#[derive(Clone)]
struct Submissions {
    chain_id: u64,
}

impl GuardedSubmission for Submissions {
    fn deploy<'a>(
        &'a self,
        guard: &'a (dyn Fn() -> bool + Send + Sync),
        owned: Arc<super::owned_node::OwnedNode>,
        key: String,
        creation: &'a str,
        value: &'a str,
        exact_gas: u64,
    ) -> BoxFuture<'a, Result<super::call_result::CallResult, ()>> {
        Box::pin(super::contract_service::guarded_deploy(guard, owned, key, creation, value, exact_gas, self.chain_id))
    }

    fn abi_send<'a>(
        &'a self,
        guard: &'a (dyn Fn() -> bool + Send + Sync),
        owned: Arc<super::owned_node::OwnedNode>,
        key: String,
        request: super::contract_service::Request,
        exact_gas: u64,
    ) -> BoxFuture<'a, Result<super::call_result::CallResult, ()>> {
        Box::pin(super::contract_service::guarded_abi_send(guard, owned, key, request, exact_gas))
    }

    fn raw_send<'a>(
        &'a self,
        guard: &'a (dyn Fn() -> bool + Send + Sync),
        owned: Arc<super::owned_node::OwnedNode>,
        key: String,
        request: &'a runner::RawSendRequest,
    ) -> BoxFuture<'a, Result<super::call_result::CallResult, ()>> {
        Box::pin(super::contract_service::guarded_raw_send(guard, owned, key, request, self.chain_id))
    }
}

#[derive(Clone)]
struct Controls;

impl Controls {
    fn rpc<'a>(&'a self, guard: &'a (dyn Fn() -> bool + Send + Sync), method: &'static str, params: Value) -> BoxFuture<'a, Result<Value, ControlTransport>> {
        Box::pin(async move {
            if !guard() {
                return Err(ControlTransport::NotDispatched);
            }
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .build()
                .map_err(|_| ControlTransport::NotDispatched)?;
            if !guard() {
                return Err(ControlTransport::NotDispatched);
            }
            let mut response = client
                .post(LOCAL_RPC)
                .json(&json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}))
                .send()
                .await
                .map_err(|_| ControlTransport::Unknown)?;
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| ControlTransport::Unknown)? {
                if chunk.len() > 16 * 1024 - bytes.len() {
                    return Err(ControlTransport::Unknown);
                }
                bytes.extend_from_slice(&chunk);
            }
            serde_json::from_slice(&bytes).map_err(|_| ControlTransport::Unknown)
        })
    }
}

impl OwnedControls for Controls {
    fn snapshot<'a>(&'a self, guard: &'a (dyn Fn() -> bool + Send + Sync)) -> BoxFuture<'a, Result<Value, ControlTransport>> {
        self.rpc(guard, "evm_snapshot", json!([]))
    }

    fn revert<'a>(&'a self, guard: &'a (dyn Fn() -> bool + Send + Sync), private_id: &'a str) -> BoxFuture<'a, Result<Value, ControlTransport>> {
        self.rpc(guard, "evm_revert", json!([private_id]))
    }

    fn automine<'a>(&'a self, guard: &'a (dyn Fn() -> bool + Send + Sync), enabled: bool) -> BoxFuture<'a, Result<Value, ControlTransport>> {
        self.rpc(guard, "evm_setAutomine", json!([enabled]))
    }

    fn mine<'a>(&'a self, guard: &'a (dyn Fn() -> bool + Send + Sync)) -> BoxFuture<'a, Result<Value, ControlTransport>> {
        self.rpc(guard, "evm_mine", json!([]))
    }
}

pub struct TrustedMacroExecutor {
    state: Arc<Mutex<DesktopState>>,
    foundry: Arc<FoundryController>,
}

impl TrustedMacroExecutor {
    pub fn new(state: Arc<Mutex<DesktopState>>, foundry: Arc<FoundryController>) -> Self {
        Self { state, foundry }
    }
}

impl MacroExecutor for TrustedMacroExecutor {
    fn execute(
        &self,
        request: ExecutionRequest,
        cancel: tokio_util::sync::CancellationToken,
    ) -> BoxFuture<'static, Result<ExecutionOutput, ExecutionFailure>> {
        let state = self.state.clone();
        let foundry = self.foundry.clone();
        Box::pin(async move {
            let addresses = {
                let state = state.lock().unwrap();
                let accounts = [0, 1, 2].map(|index| anvil_dev_account(index).unwrap().0.to_ascii_lowercase());
                SignerAddresses::new(accounts, (!state.wallet_address.is_empty()).then(|| state.wallet_address.clone()))
                    .map_err(|_| ExecutionFailure::Unavailable)?
            };
            let adapted = if super::macro_adapter::needs_artifacts(&request.definition) {
                let view = foundry.view().map_err(|_| ExecutionFailure::Unavailable)?;
                let generation = view.report.as_ref().map(|report| report.generation).ok_or(ExecutionFailure::Unavailable)?;
                let (provenance, adapted) = foundry
                    .with_current_report(generation, |report| {
                        super::macro_adapter::adapt(
                            &request.definition,
                            report,
                            &addresses,
                            request.token.clone(),
                            request.dry_run,
                        )
                    })
                    .await
                    .map_err(|_| ExecutionFailure::Unavailable)?;
                let adapted = adapted.map_err(|_| ExecutionFailure::InvalidPlan)?;
                foundry.validate_provenance(&provenance).await.map_err(|_| ExecutionFailure::StaleContext)?;
                adapted
            } else {
                super::macro_adapter::adapt_without_artifacts(&request.definition, &addresses, request.token.clone(), request.dry_run)
                    .map_err(|_| ExecutionFailure::InvalidPlan)?
            };
            let (authority, signers, gate, token, active, chain_id, keystores_blocked) = {
                let state_guard = state.lock().unwrap();
                if state_guard.workspace_epoch != request.token.workspace_epoch
                    || state_guard.network_epoch != request.token.network_epoch
                    || !request.owned_node.authorizes_loopback(8545)
                {
                    return Err(ExecutionFailure::StaleContext);
                }
                (
                    Authority { state: state.clone(), token: request.token.clone() },
                    Signers { active_key: state_guard.wallet_key.clone(), active_address: (!state_guard.wallet_address.is_empty()).then(|| state_guard.wallet_address.clone()) },
                    state_guard.mutation_gate.clone(),
                    request.token.clone(),
                    state_guard.active,
                    state_guard.execution_chain_id(),
                    state_guard.keystores_blocked(),
                )
            };
            if !active {
                return Err(ExecutionFailure::StaleContext);
            }
            let backend = DesktopMacroBackend::new(
                authority,
                signers,
                Submissions { chain_id },
                Controls,
                request.owned_node,
                gate,
                token,
                addresses,
                chain_id,
                keystores_blocked,
            ).map_err(|_| ExecutionFailure::Unavailable)?;
            let step_ids = adapted.plan_ids();
            // In a bundle, sends return once the node accepts them; the runner
            // mines them together and then checks each receipt.
            let bundle = adapted.plan.bundle && !adapted.plan.dry_run;
            let report = super::contract_service::BUNDLE_SUBMIT
                .scope(bundle, runner::execute(adapted.plan, &backend, &cancel, |_| {}))
                .await
                .map_err(|_| ExecutionFailure::Failed)?;
            Ok(ExecutionOutput { report, step_ids })
        })
    }
}
