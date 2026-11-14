use super::application::{self, AppError, CommandRequest, Query};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

const APPROVAL_TTL: Duration = Duration::from_secs(120);
const MAX_REQUESTS: usize = 1024;
const MAX_APPROVALS: usize = 16;
const MAX_RESPONSE: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Principal {
    pub id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Scopes {
    pub read: bool,
    pub ui: bool,
    pub analyze: bool,
}

struct Client {
    id: String,
    digest: [u8; 32],
    scopes: Scopes,
    window: Instant,
    calls: u16,
}

#[derive(Clone)]
pub struct Approval {
    pub id: String,
    pub client_id: String,
    pub request: CommandRequest,
    pub target: String,
    expires: Instant,
}

struct RequestRecord {
    fingerprint: [u8; 32],
    result: Value,
}

pub enum UiRequest {
    Query {
        principal: Principal,
        query: Query,
        reply: oneshot::Sender<Result<Value, AppError>>,
    },
    Command {
        principal: Principal,
        request: CommandRequest,
        reply: oneshot::Sender<Result<Value, AppError>>,
    },
    Source {
        principal: Principal,
        context: application::Context,
        reply: oneshot::Sender<Result<String, AppError>>,
    },
}

type Bridge = Arc<dyn Fn(UiRequest) + Send + Sync>;

struct State {
    enabled: bool,
    endpoint: String,
    clients: Vec<Client>,
    approvals: VecDeque<Approval>,
    requests: HashMap<(String, String), RequestRecord>,
    audit: VecDeque<Value>,
    bridge: Option<Bridge>,
}

pub struct Hub {
    state: Mutex<State>,
    execution: Mutex<()>,
    slots: Arc<tokio::sync::Semaphore>,
    investigation: Mutex<Option<Arc<super::investigation::Investigation>>>,
}

impl Hub {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                enabled: false,
                endpoint: String::new(),
                clients: Vec::new(),
                approvals: VecDeque::new(),
                requests: HashMap::new(),
                audit: VecDeque::new(),
                bridge: None,
            }),
            execution: Mutex::new(()),
            slots: Arc::new(tokio::sync::Semaphore::new(4)),
            investigation: Mutex::new(None),
        })
    }

    pub fn bind(&self, bridge: Bridge) {
        self.state.lock().unwrap().bridge = Some(bridge);
    }

    pub fn unbind(&self) {
        let mut state = self.state.lock().unwrap();
        state.bridge = None;
        expire_all(&mut state, "workspace_changed");
    }

    pub fn enable_local(&self, endpoint: String) {
        let mut state = self.state.lock().unwrap();
        state.enabled = true;
        state.endpoint = endpoint;
        audit(&mut state, "local", "access.enabled", "enabled");
    }

    pub fn disable_local(&self) {
        let _execution = self.execution.lock().unwrap();
        let mut state = self.state.lock().unwrap();
        state.enabled = false;
        state.clients.clear();
        expire_all(&mut state, "access_disabled");
        state.requests.clear();
        audit(&mut state, "local", "access.disabled", "disabled");
    }

    pub fn pair_local(&self, scopes: Scopes) -> Result<(String, String), AppError> {
        let mut state = self.state.lock().unwrap();
        if !state.enabled {
            return Err(AppError::new("disabled", "Enable AI access locally first."));
        }
        if state.clients.len() >= 8 {
            return Err(AppError::new(
                "client_limit",
                "Revoke a client before pairing another.",
            ));
        }
        let id = random_id("client");
        let token = random_id("chasm");
        state.clients.push(Client {
            id: id.clone(),
            digest: ethers::utils::keccak256(token.as_bytes()),
            scopes,
            window: Instant::now(),
            calls: 0,
        });
        audit(&mut state, "local", "client.paired", &id);
        Ok((id, token))
    }

    pub fn revoke_local(&self, id: &str) {
        let _execution = self.execution.lock().unwrap();
        let mut state = self.state.lock().unwrap();
        state.clients.retain(|client| client.id != id);
        state.requests.retain(|(client, _), _| client != id);
        let ids: Vec<_> = state
            .approvals
            .iter()
            .filter(|item| item.client_id == id)
            .map(|item| item.id.clone())
            .collect();
        for approval in ids {
            finish_approval(&mut state, &approval, "client_revoked");
        }
        audit(&mut state, "local", "client.revoked", id);
    }

    pub fn authenticate(&self, token: &str) -> Result<Principal, AppError> {
        if token.len() > 128 || token.len() < 32 {
            return Err(unauthorized());
        }
        let digest = ethers::utils::keccak256(token.as_bytes());
        let mut state = self.state.lock().unwrap();
        if !state.enabled {
            return Err(unauthorized());
        }
        let client = state
            .clients
            .iter_mut()
            .find(|client| constant_equal(&client.digest, &digest))
            .ok_or_else(unauthorized)?;
        if client.window.elapsed() >= Duration::from_secs(60) {
            client.window = Instant::now();
            client.calls = 0;
        }
        if client.calls >= 120 {
            return Err(AppError::new(
                "rate_limited",
                "Client request limit reached. Retry later.",
            ));
        }
        client.calls += 1;
        Ok(Principal {
            id: client.id.clone(),
        })
    }

    pub fn authorize(&self, principal: &Principal, scope: &str) -> Result<(), AppError> {
        let state = self.state.lock().unwrap();
        if !state.enabled {
            return Err(unauthorized());
        }
        let client = state
            .clients
            .iter()
            .find(|client| client.id == principal.id)
            .ok_or_else(unauthorized)?;
        let allowed = match scope {
            "read" => client.scopes.read,
            "ui" => client.scopes.ui,
            "analyze" => client.scopes.analyze,
            _ => false,
        };
        if allowed {
            Ok(())
        } else {
            Err(AppError::new(
                "scope_denied",
                format!("This client does not have the {scope} scope."),
            ))
        }
    }

    pub async fn query(&self, principal: Principal, query: Query) -> Result<Value, AppError> {
        self.authorize(&principal, "read")?;
        let (reply, receive) = oneshot::channel();
        let bridge = self.state.lock().unwrap().bridge.clone().ok_or_else(|| {
            AppError::new("unavailable", "The native window is changing workspace.")
        })?;
        bridge(UiRequest::Query {
            principal,
            query,
            reply,
        });
        bounded(
            tokio::time::timeout(Duration::from_secs(5), receive)
                .await
                .map_err(|_| {
                    AppError::new("ui_timeout", "The native window did not respond in time.")
                })?
                .map_err(|_| AppError::new("ui_closed", "The native window is unavailable."))??,
        )
    }

    pub async fn command(
        &self,
        principal: Principal,
        request: CommandRequest,
    ) -> Result<Value, AppError> {
        self.authorize(&principal, "ui")?;
        let (reply, receive) = oneshot::channel();
        let bridge = self.state.lock().unwrap().bridge.clone().ok_or_else(|| {
            AppError::new("unavailable", "The native window is changing workspace.")
        })?;
        bridge(UiRequest::Command {
            principal,
            request,
            reply,
        });
        bounded(
            tokio::time::timeout(Duration::from_secs(5), receive)
                .await
                .map_err(|_| {
                    AppError::new(
                        "ui_timeout",
                        "Read request status before retrying; use the same request ID.",
                    )
                })?
                .map_err(|_| AppError::new("ui_closed", "The native window is unavailable."))??,
        )
    }

    pub fn execute_ui(
        &self,
        principal: &Principal,
        request: &CommandRequest,
        state: &mut super::session::DesktopState,
    ) -> Result<Value, AppError> {
        let _execution = self.execution.lock().unwrap();
        self.authorize(principal, "ui")?;
        if request.request_id.is_empty()
            || request.request_id.len() > 128
            || !request
                .request_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
        {
            return Err(AppError::new(
                "invalid_request_id",
                "Request ID must be 1..128 ASCII letters, digits, dash, dot, colon or underscore.",
            ));
        }
        let fingerprint = ethers::utils::keccak256(
            serde_json::to_vec(request)
                .map_err(|_| AppError::new("invalid_request", "Invalid command."))?,
        );
        let key = (principal.id.clone(), request.request_id.clone());
        {
            let mut inner = self.state.lock().unwrap();
            sweep(&mut inner);
            if let Some(record) = inner.requests.get(&key) {
                return if record.fingerprint == fingerprint {
                    Ok(record.result.clone())
                } else {
                    Err(AppError::new(
                        "request_id_conflict",
                        "That request ID was already used with a different payload.",
                    ))
                };
            }
            if inner.requests.len() >= MAX_REQUESTS {
                return Err(AppError::new("request_capacity","Request history is full. Disable access and pair a new client; requests are never silently evicted and rerun."));
            }
        }
        application::check_context(state, &request.context)?;
        if matches!(request.command, application::Command::OpenWorkspace { .. }) {
            return Err(AppError::new("local_only", "Open Project uses the local native folder picker. Remote filesystem paths are not accepted until protected directory identity is available."));
        }
        application::validate(state, &request.command)?;
        let result = if request.command.sensitive() {
            let mut inner = self.state.lock().unwrap();
            if inner.approvals.len() >= MAX_APPROVALS {
                return Err(AppError::new(
                    "approval_capacity",
                    "Resolve an existing local approval first.",
                ));
            }
            let id = random_id("approval");
            let target = match &request.command {
                application::Command::RemoveDeployment { id } => state
                    .deployments
                    .iter()
                    .enumerate()
                    .find(|(index, _)| {
                        application::deployment_id(state, *index).as_ref() == Some(id)
                    })
                    .map(|(_, item)| {
                        format!(
                            "{} at {} (remove local record only; contract stays on chain)",
                            item.name, item.address
                        )
                    })
                    .unwrap_or_default(),
                _ => String::new(),
            };
            inner.approvals.push_back(Approval {
                id: id.clone(),
                client_id: principal.id.clone(),
                request: request.clone(),
                target,
                expires: Instant::now() + APPROVAL_TTL,
            });
            json!({"status":"pending_approval","approvalId":id,"expiresInSeconds":APPROVAL_TTL.as_secs(),"requestId":request.request_id})
        } else {
            match application::apply(state, &request.command) {
                Ok(result) => result,
                Err(error) => json!({"status":"failed","error":error}),
            }
        };
        let mut inner = self.state.lock().unwrap();
        inner.requests.insert(
            key,
            RequestRecord {
                fingerprint,
                result: result.clone(),
            },
        );
        audit(
            &mut inner,
            &principal.id,
            request.command.action_id(),
            result["status"].as_str().unwrap_or("completed"),
        );
        Ok(result)
    }

    pub fn pending_local(&self) -> Option<Approval> {
        let mut state = self.state.lock().unwrap();
        sweep(&mut state);
        state.approvals.front().cloned()
    }

    pub fn resolve_local(
        &self,
        id: &str,
        accepted: bool,
        state: &mut super::session::DesktopState,
    ) -> Result<Value, AppError> {
        let _execution = self.execution.lock().unwrap();
        let approval = {
            let mut inner = self.state.lock().unwrap();
            sweep(&mut inner);
            let index = inner
                .approvals
                .iter()
                .position(|item| item.id == id)
                .ok_or_else(|| {
                    AppError::new(
                        "approval_expired",
                        "The approval expired or was already used.",
                    )
                })?;
            inner.approvals.remove(index).unwrap()
        };
        let result = if accepted {
            let confirmation = state.security_confirmation;
            // Only this exact local approval may pass its own cross-window confirmation gate.
            state.security_confirmation = false;
            let result = self.authorize(
                &Principal {
                    id: approval.client_id.clone(),
                },
                "ui",
            )
            .and_then(|_| application::check_context(state, &approval.request.context))
            .and_then(|_| application::apply(state, &approval.request.command))
            .unwrap_or_else(|error| json!({"status":"rejected","error":error}));
            state.security_confirmation = confirmation;
            result
        } else {
            json!({"status":"cancelled"})
        };
        let mut inner = self.state.lock().unwrap();
        if let Some(record) = inner.requests.get_mut(&(
            approval.client_id.clone(),
            approval.request.request_id.clone(),
        )) {
            record.result = result.clone();
        }
        audit(
            &mut inner,
            &approval.client_id,
            approval.request.command.action_id(),
            if accepted {
                "local_decision"
            } else {
                "cancelled"
            },
        );
        Ok(result)
    }

    pub fn request_status(
        &self,
        principal: &Principal,
        request_id: &str,
    ) -> Result<Value, AppError> {
        self.authorize(principal, "ui")?;
        let mut state = self.state.lock().unwrap();
        sweep(&mut state);
        state
            .requests
            .get(&(principal.id.clone(), request_id.to_string()))
            .map(|record| record.result.clone())
            .ok_or_else(|| {
                AppError::new(
                    "not_found",
                    "No accepted command with that request ID for this client.",
                )
            })
    }

    pub fn local_status(&self) -> Value {
        let mut state = self.state.lock().unwrap();
        sweep(&mut state);
        json!({"enabled":state.enabled,"endpoint":state.endpoint,"clients":state.clients.iter().map(|client|json!({"id":client.id,"scopes":client.scopes})).collect::<Vec<_>>(),"pendingApprovals":state.approvals.len(),"audit":state.audit})
    }

    pub async fn acquire(&self) -> Result<tokio::sync::OwnedSemaphorePermit, AppError> {
        self.slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| AppError::new("busy", "Too many concurrent control requests."))
    }

    pub fn set_investigation(&self, investigation: Arc<super::investigation::Investigation>) {
        *self.investigation.lock().unwrap() = Some(investigation);
    }

    pub async fn analyze(&self, principal: Principal, value: Value) -> Result<Value, AppError> {
        self.authorize(&principal, "analyze")?;
        let request: super::investigation::AnalysisRequest = serde_json::from_value(value).map_err(|_|AppError::new("invalid_params","Invalid analysis request. Read the schema; arbitrary RPC methods and endpoints are not accepted."))?;
        let investigation =
            self.investigation.lock().unwrap().clone().ok_or_else(|| {
                AppError::new("unavailable", "Investigation service is unavailable.")
            })?;
        let rpc = if let super::investigation::AnalysisRequest::Chain { context, .. } = &request {
            let (reply, receive) = oneshot::channel();
            let bridge =
                self.state.lock().unwrap().bridge.clone().ok_or_else(|| {
                    AppError::new("unavailable", "The native window is unavailable.")
                })?;
            bridge(UiRequest::Source {
                principal: principal.clone(),
                context: context.clone(),
                reply,
            });
            Some(
                tokio::time::timeout(Duration::from_secs(5), receive)
                    .await
                    .map_err(|_| AppError::new("ui_timeout", "Source context did not respond."))?
                    .map_err(|_| AppError::new("ui_closed", "Window closed."))??,
            )
        } else {
            None
        };
        let result = tokio::time::timeout(
            Duration::from_secs(12),
            investigation.execute(rpc.as_deref(), request),
        )
        .await
        .map_err(|_| {
            AppError::new(
                "timeout",
                "Analysis exceeded its time budget. Reduce the history count.",
            )
        })??;
        self.authorize(&principal, "analyze")?;
        bounded(result)
    }
}

pub fn bounded(value: Value) -> Result<Value, AppError> {
    if serde_json::to_vec(&value).map_or(true, |bytes| bytes.len() > MAX_RESPONSE) {
        Err(AppError::new(
            "response_too_large",
            "Result exceeds the response limit. Use a smaller query.",
        ))
    } else {
        Ok(value)
    }
}

fn random_id(prefix: &str) -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    format!("{prefix}_{}", ethers::utils::hex::encode(bytes))
}

fn constant_equal(left: &[u8; 32], right: &[u8; 32]) -> bool {
    left.iter()
        .zip(right)
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}
fn unauthorized() -> AppError {
    AppError::new(
        "unauthorized",
        "Enable access and pair this client in the native Chasm window.",
    )
}
fn audit(state: &mut State, client: &str, action: &str, status: &str) {
    state
        .audit
        .push_back(json!({"clientId":client,"actionId":action,"status":status}));
    while state.audit.len() > 100 {
        state.audit.pop_front();
    }
}
fn finish_approval(state: &mut State, id: &str, reason: &str) {
    if let Some(index) = state.approvals.iter().position(|item| item.id == id) {
        let item = state.approvals.remove(index).unwrap();
        if let Some(record) = state
            .requests
            .get_mut(&(item.client_id.clone(), item.request.request_id.clone()))
        {
            record.result = json!({"status":"cancelled","reason":reason});
        }
    }
}
fn sweep(state: &mut State) {
    let expired: Vec<_> = state
        .approvals
        .iter()
        .filter(|item| Instant::now() >= item.expires)
        .map(|item| item.id.clone())
        .collect();
    for id in expired {
        finish_approval(state, &id, "expired");
    }
}
fn expire_all(state: &mut State, reason: &str) {
    let ids: Vec<_> = state.approvals.iter().map(|item| item.id.clone()).collect();
    for id in ids {
        finish_approval(state, &id, reason);
    }
}

#[cfg(test)]
mod tests {
    use super::super::application::{Activity, Command};
    use super::*;

    fn paired() -> (Arc<Hub>, Principal, String) {
        let hub = Hub::new();
        hub.enable_local("http://127.0.0.1:1".into());
        let (_, token) = hub
            .pair_local(Scopes {
                read: true,
                ui: true,
                analyze: false,
            })
            .unwrap();
        let principal = hub.authenticate(&token).unwrap();
        (hub, principal, token)
    }

    #[test]
    fn disabled_revoked_and_wrong_scope_clients_are_rejected() {
        let hub = Hub::new();
        assert!(hub.authenticate("invalid").is_err());
        let (hub, principal, token) = paired();
        assert_eq!(
            hub.authorize(&principal, "analyze").unwrap_err().code,
            "scope_denied"
        );
        assert!(!hub.local_status().to_string().contains(&token));
        hub.revoke_local(&principal.id);
        assert!(hub.authenticate(&token).is_err());
        assert!(hub.authorize(&principal, "read").is_err());
    }

    #[test]
    fn duplicate_commands_do_not_repeat_and_conflicting_payloads_are_rejected() {
        let (hub, principal, _) = paired();
        let root = tempfile::tempdir().unwrap();
        let mut state = super::super::session::DesktopState::new(root.path().into(), 1.0);
        let mut request = CommandRequest {
            request_id: "fixture-1".into(),
            context: application::context(&state),
            command: Command::Navigate {
                activity: Activity::Wallet,
            },
        };
        let first = hub.execute_ui(&principal, &request, &mut state).unwrap();
        let second = hub.execute_ui(&principal, &request, &mut state).unwrap();
        assert_eq!(first, second);
        assert_eq!(state.documents.len(), 1);
        request.command = Command::Navigate {
            activity: Activity::Explorer,
        };
        assert_eq!(
            hub.execute_ui(&principal, &request, &mut state)
                .unwrap_err()
                .code,
            "request_id_conflict"
        );
    }

    #[test]
    fn failed_partial_commands_keep_dedup_records_and_disable_recovers_capacity() {
        let (hub, principal, _) = paired();
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join(".chasm"),
            "fixture prevents settings directory",
        )
        .unwrap();
        let mut state = super::super::session::DesktopState::new(root.path().into(), 1.0);
        let mut request = CommandRequest {
            request_id: "partial".into(),
            context: application::context(&state),
            command: Command::Layout {
                sidebar_width: Some(300.0),
                collapsed: None,
                zoom_percent: None,
            },
        };
        let first = hub.execute_ui(&principal, &request, &mut state).unwrap();
        assert_eq!(first["status"], "failed");
        assert_eq!(
            hub.execute_ui(&principal, &request, &mut state).unwrap(),
            first
        );
        request.command = Command::Navigate {
            activity: Activity::Wallet,
        };
        assert_eq!(
            hub.execute_ui(&principal, &request, &mut state)
                .unwrap_err()
                .code,
            "request_id_conflict"
        );
        hub.disable_local();
        assert!(hub.state.lock().unwrap().requests.is_empty());
        hub.enable_local("fixture".into());
        let (_, token) = hub
            .pair_local(Scopes {
                read: true,
                ui: true,
                analyze: false,
            })
            .unwrap();
        let next = hub.authenticate(&token).unwrap();
        request.context = application::context(&state);
        hub.execute_ui(&next, &request, &mut state).unwrap();
    }

    #[test]
    fn expired_approval_never_executes_the_next_queued_request() {
        let (hub, principal, _) = paired();
        let root = tempfile::tempdir().unwrap();
        let mut state = super::super::session::DesktopState::new(root.path().into(), 1.0);
        for suffix in [1, 2] {
            state.deployments.push(super::super::session::Deployment {
                name: format!("fixture-{suffix}"),
                address: format!("0x{suffix:040x}"),
            });
        }
        let mut ids = Vec::new();
        for index in 0..2 {
            let request = CommandRequest {
                request_id: format!("remove-{index}"),
                context: application::context(&state),
                command: Command::RemoveDeployment {
                    id: application::deployment_id(&state, index).unwrap(),
                },
            };
            let result = hub.execute_ui(&principal, &request, &mut state).unwrap();
            ids.push(result["approvalId"].as_str().unwrap().to_string());
        }
        hub.state
            .lock()
            .unwrap()
            .approvals
            .front_mut()
            .unwrap()
            .expires = Instant::now() - Duration::from_secs(1);
        assert!(hub.resolve_local(&ids[0], true, &mut state).is_err());
        assert_eq!(state.deployments.len(), 2);
        assert_eq!(hub.pending_local().unwrap().id, ids[1]);
        hub.resolve_local(&ids[1], false, &mut state).unwrap();
        assert_eq!(state.deployments.len(), 2);
    }

    #[test]
    fn remote_project_paths_are_denied_before_any_filesystem_validation() {
        let (hub, principal, _) = paired();
        let mut state = super::super::session::DesktopState::welcome();
        let request = CommandRequest {
            request_id: "unc".into(),
            context: application::context(&state),
            command: Command::OpenWorkspace {
                path: "\\\\unreachable.invalid\\share".into(),
            },
        };
        assert_eq!(
            hub.execute_ui(&principal, &request, &mut state)
                .unwrap_err()
                .code,
            "local_only"
        );
        assert!(hub.pending_local().is_none());
    }

    #[test]
    fn approval_is_local_once_context_bound_and_revocation_cancels_it() {
        let (hub, principal, _) = paired();
        let root = tempfile::tempdir().unwrap();
        let mut state = super::super::session::DesktopState::new(root.path().into(), 1.0);
        state.deployments.push(super::super::session::Deployment {
            name: "fixture".into(),
            address: "0x0000000000000000000000000000000000000001".into(),
        });
        let request = CommandRequest {
            request_id: "fixture-remove".into(),
            context: application::context(&state),
            command: Command::RemoveDeployment {
                id: application::deployment_id(&state, 0).unwrap(),
            },
        };
        let result = hub.execute_ui(&principal, &request, &mut state).unwrap();
        assert_eq!(state.deployments.len(), 1);
        let id = result["approvalId"].as_str().unwrap();
        state.view_revision += 1;
        let result = hub.resolve_local(id, true, &mut state).unwrap();
        assert_eq!(result["error"]["code"], "stale_context");
        assert_eq!(state.deployments.len(), 1);
        assert!(hub.resolve_local(id, true, &mut state).is_err());
        let request = CommandRequest {
            request_id: "fixture-remove-2".into(),
            context: application::context(&state),
            ..request
        };
        hub.execute_ui(&principal, &request, &mut state).unwrap();
        hub.revoke_local(&principal.id);
        assert!(hub.pending_local().is_none());
        assert_eq!(state.deployments.len(), 1);
    }

    #[test]
    fn local_approval_can_pass_only_its_own_cross_window_gate() {
        let (hub, principal, _) = paired();
        let root = tempfile::tempdir().unwrap();
        let mut state = super::super::session::DesktopState::new(root.path().into(), 1.0);
        state.deployments.push(super::super::session::Deployment {
            name: "fixture".into(), address: "0x0000000000000000000000000000000000000001".into(),
        });
        let request = CommandRequest { request_id: "gate-remove".into(), context: application::context(&state), command: Command::RemoveDeployment { id: application::deployment_id(&state, 0).unwrap() } };
        let pending = hub.execute_ui(&principal, &request, &mut state).unwrap();
        state.security_confirmation = true;
        assert!(application::apply(&mut state, &Command::Navigate { activity: application::Activity::Contracts }).is_err());
        let result = hub.resolve_local(pending["approvalId"].as_str().unwrap(), true, &mut state).unwrap();
        assert_eq!(result["status"], "completed");
        assert!(state.security_confirmation);
        assert!(state.deployments.is_empty());
    }
}
