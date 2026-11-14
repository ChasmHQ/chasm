mod config;
mod security;
mod worker;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        DefaultBodyLimit, Extension, Path, Query, State,
    },
    http::StatusCode,
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
pub use config::Config;
use config::{MAX_BODY, MAX_REQUEST, PROTOCOL};
use rand::{rngs::OsRng, RngCore};
pub use security::Owner;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    hash::{Hash, Hasher},
    io::Write,
    path::{Component, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::broadcast;
use worker::Worker;

#[derive(Debug, Clone, Serialize)]
pub struct GatewayError {
    pub code: String,
    pub message: String,
    #[serde(skip)]
    status: StatusCode,
    #[serde(skip)]
    fatal: bool,
}

impl GatewayError {
    fn new(status: StatusCode, code: &str, message: &str) -> Self {
        Self {
            status,
            code: code.into(),
            message: message.chars().take(2048).collect(),
            fatal: false,
        }
    }
    fn bad(message: &str) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_params", message)
    }
    fn forbidden(message: &str) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", message)
    }
    fn conflict(message: &str) -> Self {
        Self::new(StatusCode::CONFLICT, "conflict", message)
    }
    fn missing() -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "Debugger session or resource was not found for this browser.",
        )
    }
    fn unavailable() -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, "unavailable", "Local debugger is unavailable. Set --debugger-python or CHASM_DEBUGGER_PYTHON to an installed chasm-debugger interpreter; no download is attempted.")
    }
    fn worker(message: &str) -> Self {
        let mut error = Self::new(StatusCode::BAD_GATEWAY, "worker_failed", message);
        error.fatal = true;
        error
    }
    fn service(code: &str, message: &str) -> Self {
        let code = if [
            "invalid_request",
            "invalid_params",
            "unknown_method",
            "no_session",
            "unsupported",
            "start_failed",
            "operation_failed",
        ]
        .contains(&code)
        {
            code
        } else {
            "operation_failed"
        };
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }
}
impl std::fmt::Display for GatewayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for GatewayError {}
impl IntoResponse for GatewayError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({"error": self}))).into_response()
    }
}

pub(super) fn random_id() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceInfo {
    pub id: String,
    pub name: String,
    pub kind: String,
}
struct Resource {
    info: ResourceInfo,
    owner: String,
    path: PathBuf,
    size: usize,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Envelope {
    id: String,
    generation: u64,
    revision: u64,
    sequence: u64,
    state: String,
    busy: bool,
    snapshot: Option<Value>,
    error: Option<GatewayError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
}
struct Session {
    owner: String,
    envelope: Envelope,
    active: bool,
    worker: Option<Arc<Worker>>,
    requests: HashMap<String, u64>,
    events: VecDeque<Envelope>,
    event_bytes: usize,
    last_activity: Instant,
}
struct Registry {
    owners: HashMap<String, String>,
    owner_activity: HashMap<String, Instant>,
    resources: HashMap<String, Resource>,
    session: Option<Session>,
    generation: u64,
}

pub struct Manager {
    config: Config,
    registry: Mutex<Registry>,
    resources_dir: Option<tempfile::TempDir>,
    availability: Option<String>,
    events: broadcast::Sender<()>,
    build_admission: Arc<tokio::sync::Semaphore>,
    stopping: std::sync::atomic::AtomicBool,
}

impl Manager {
    pub async fn new(config: Config) -> Arc<Self> {
        let resources_dir = tempfile::Builder::new()
            .prefix("chasm-debugger-")
            .tempdir()
            .ok();
        let mut availability = config.reason.clone();
        if resources_dir.is_none() {
            availability = Some("Could not create a private debugger resource directory.".into());
        }
        if availability.is_none() {
            // Probe before serving, then terminate. No worker is retained when idle.
            availability = match Worker::spawn(&config, None) {
                Ok(worker) => {
                    let probe = tokio::time::timeout(
                        config.timeout.min(std::time::Duration::from_secs(10)),
                        worker.hello(),
                    )
                    .await;
                    worker.killer.kill().await;
                    match probe {
                        Ok(Ok(_)) => None,
                        Ok(Err(error)) => Some(error.message),
                        Err(_) => Some(
                            "Debugger hello timed out; check the local debugger installation."
                                .into(),
                        ),
                    }
                }
                Err(error) => Some(error.message),
            };
        }
        let (events, _) = broadcast::channel(64);
        let manager = Arc::new(Self {
            config,
            availability,
            resources_dir,
            events,
            build_admission: Arc::new(tokio::sync::Semaphore::new(1)),
            stopping: std::sync::atomic::AtomicBool::new(false),
            registry: Mutex::new(Registry {
                owners: HashMap::new(),
                owner_activity: HashMap::new(),
                resources: HashMap::new(),
                session: None,
                generation: 0,
            }),
        });
        let weak = Arc::downgrade(&manager);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                let Some(manager) = weak.upgrade() else {
                    return;
                };
                manager.cleanup_idle().await;
            }
        });
        manager
    }

    /// Remove one owner-scoped resource once its source build is stale. An active
    /// debugger session keeps its already-open worker; no new launch may resolve it.
    pub fn revoke_resource(&self, owner: &Owner, id: &str) -> bool {
        let removed = {
            let mut registry = self.registry.lock().unwrap();
            if registry
                .resources
                .get(id)
                .is_none_or(|resource| resource.owner != owner.0)
            {
                return false;
            }
            registry.resources.remove(id)
        };
        if let Some(resource) = removed {
            let _ = std::fs::remove_file(resource.path);
            true
        } else {
            false
        }
    }

    /// Compiler integration uses the same owner-scoped, server-generated resources as uploads.
    pub fn register_build_info(
        &self,
        owner: &Owner,
        name: &str,
        content: Value,
    ) -> Result<ResourceInfo, GatewayError> {
        self.register_resource(owner, "build_info", name, content)
    }

    fn register_resource(
        &self,
        owner: &Owner,
        kind: &str,
        name: &str,
        content: Value,
    ) -> Result<ResourceInfo, GatewayError> {
        if !["capture", "build_info"].contains(&kind)
            || !content.is_object()
            || name.is_empty()
            || name.len() > 128
            || name.chars().any(char::is_control)
        {
            return Err(GatewayError::bad("Resource requires capture/build_info kind, a name (1–128 characters), and object content."));
        }
        let bytes = serde_json::to_vec(&content)
            .map_err(|_| GatewayError::bad("Invalid resource JSON."))?;
        if bytes.len() > MAX_BODY {
            return Err(GatewayError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "too_large",
                "Debugger resources are limited to 8 MiB.",
            ));
        }
        let mut registry = self.registry.lock().unwrap();
        if !registry.owners.contains_key(&owner.0) {
            return Err(GatewayError::forbidden("Unknown debugger browser owner."));
        }
        if registry.resources.len() >= 64
            || registry.resources.values().map(|r| r.size).sum::<usize>() + bytes.len()
                > 64 * 1024 * 1024
        {
            return Err(GatewayError::conflict(
                "Debugger resource capacity reached; restart the local server to clear resources.",
            ));
        }
        let directory = self
            .resources_dir
            .as_ref()
            .ok_or_else(GatewayError::unavailable)?;
        let info = ResourceInfo {
            id: random_id(),
            name: name.into(),
            kind: kind.into(),
        };
        let path = directory.path().join(format!("{}.json", info.id));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|_| GatewayError::worker("Could not store debugger resource."))?;
        if file.write_all(&bytes).is_err() {
            let _ = std::fs::remove_file(&path);
            return Err(GatewayError::worker("Could not store debugger resource."));
        }
        registry.resources.insert(
            info.id.clone(),
            Resource {
                info: info.clone(),
                owner: owner.0.clone(),
                path,
                size: bytes.len(),
            },
        );
        Ok(info)
    }

    pub async fn shutdown(&self) {
        self.stopping
            .store(true, std::sync::atomic::Ordering::Release);
        let worker = {
            let mut registry = self.registry.lock().unwrap();
            registry.session.as_mut().and_then(|session| {
                session.active = false;
                session.envelope.state = "cancelled".into();
                session.envelope.busy = false;
                session.envelope.revision += 1;
                self.publish(session);
                session.worker.take()
            })
        };
        if let Some(worker) = worker {
            worker.killer.kill().await;
        }
    }

    async fn cleanup_idle(&self) {
        let expired = {
            let mut registry = self.registry.lock().unwrap();
            let active_owner = registry
                .session
                .as_ref()
                .filter(|s| s.active)
                .map(|s| s.owner.clone());
            let owners: Vec<_> = registry
                .owner_activity
                .iter()
                .filter(|(owner, seen)| {
                    seen.elapsed() > Duration::from_secs(30 * 60)
                        && active_owner.as_ref() != Some(owner)
                })
                .map(|(owner, _)| owner.clone())
                .collect();
            for owner in &owners {
                registry.owners.remove(owner);
                registry.owner_activity.remove(owner);
            }
            registry.resources.retain(|_, resource| {
                if owners.contains(&resource.owner) {
                    let _ = std::fs::remove_file(&resource.path);
                    false
                } else {
                    true
                }
            });
            registry
                .session
                .as_mut()
                .filter(|s| {
                    s.active
                        && !s.envelope.busy
                        && s.last_activity.elapsed() > Duration::from_secs(10 * 60)
                })
                .map(|s| {
                    s.envelope.state = "cancelling".into();
                    s.envelope.busy = true;
                    self.publish(s);
                    (s.envelope.id.clone(), s.worker.clone())
                })
        };
        if let Some((id, worker)) = expired {
            if let Some(worker) = worker {
                worker.killer.kill().await;
            }
            let mut registry = self.registry.lock().unwrap();
            if let Some(s) = registry.session.as_mut().filter(|s| s.envelope.id == id) {
                s.active = false;
                s.worker = None;
                s.envelope.state = "cancelled".into();
                s.envelope.busy = false;
                s.envelope.revision += 1;
                self.publish(s);
            }
        }
    }

    fn publish(&self, session: &mut Session) {
        session.envelope.sequence += 1;
        session.event_bytes += envelope_bytes(&session.envelope);
        session.events.push_back(session.envelope.clone());
        while session.events.len() > 1
            && (session.events.len() > 64 || session.event_bytes > 16 * 1024 * 1024)
        {
            if let Some(event) = session.events.pop_front() {
                session.event_bytes = session.event_bytes.saturating_sub(envelope_bytes(&event));
            }
        }
        let _ = self.events.send(());
    }

    fn resolve_resource(
        &self,
        owner: &Owner,
        id: &str,
        kind: &str,
    ) -> Result<PathBuf, GatewayError> {
        let builtin = match (id, kind) {
            ("synthetic", "capture") => Some(self.config.example.clone()),
            ("source-demo", "capture") => Some(
                self.config
                    .service_dir
                    .join("examples/web-debugger/source-replay.json"),
            ),
            ("source-demo-build", "build_info") => Some(
                self.config
                    .service_dir
                    .join("examples/web-debugger/build-info.json"),
            ),
            _ => None,
        };
        if let Some(path) = builtin {
            return path
                .canonicalize()
                .ok()
                .filter(|p| p.is_file())
                .ok_or_else(GatewayError::missing);
        }
        let registry = self.registry.lock().unwrap();
        let resource = registry
            .resources
            .get(id)
            .filter(|r| r.owner == owner.0 && r.info.kind == kind)
            .ok_or_else(GatewayError::missing)?;
        let path = resource
            .path
            .canonicalize()
            .map_err(|_| GatewayError::missing())?;
        let root = self
            .resources_dir
            .as_ref()
            .unwrap()
            .path()
            .canonicalize()
            .map_err(|_| GatewayError::missing())?;
        if !path.starts_with(root) || !path.is_file() {
            return Err(GatewayError::missing());
        }
        Ok(path)
    }

    fn target(&self, target: &str) -> Result<PathBuf, GatewayError> {
        let relative = std::path::Path::new(target);
        if target.contains('\\')
            || target.contains(':')
            || relative.is_absolute()
            || relative
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
            || relative.extension().and_then(|s| s.to_str()) != Some("sol")
        {
            return Err(GatewayError::bad(
                "Foundry target must be a workspace-relative .sol file without traversal.",
            ));
        }
        let root = self
            .config
            .workspace
            .canonicalize()
            .map_err(|_| GatewayError::bad("Debugger workspace does not exist."))?;
        let path = root
            .join(relative)
            .canonicalize()
            .map_err(|_| GatewayError::bad("Foundry target does not exist."))?;
        if !path.starts_with(&root) || !path.is_file() {
            return Err(GatewayError::bad(
                "Foundry target must remain inside the trusted workspace.",
            ));
        }
        // The driver's nearest foundry.toml lookup must not climb into an ancestor project.
        let foundry_root = path
            .parent()
            .unwrap()
            .ancestors()
            .find(|dir| dir.join("foundry.toml").is_file());
        if !foundry_root.is_some_and(|dir| dir.starts_with(&root)) {
            return Err(GatewayError::bad("Foundry mode requires foundry.toml inside the trusted workspace; ancestor projects are not allowed."));
        }
        Ok(path)
    }
}

pub fn router(manager: Arc<Manager>) -> Router {
    Router::new()
        .route("/capabilities", get(capabilities))
        .route("/resources", post(upload))
        .route("/build-info", post(build_info))
        .route("/sessions", post(start))
        .route("/sessions/:id", get(session).delete(dispose))
        .route("/sessions/:id/commands", post(command))
        .route("/sessions/:id/events", get(events))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .layer(middleware::from_fn_with_state(
            manager.clone(),
            security::protect,
        ))
        .with_state(manager)
}

async fn capabilities(
    State(manager): State<Arc<Manager>>,
    Extension(owner): Extension<Owner>,
) -> Json<Value> {
    let csrf = manager.registry.lock().unwrap().owners[&owner.0].clone();
    let providers: Vec<_> = manager
        .config
        .providers
        .keys()
        .map(|id| json!({"id": id, "label": id}))
        .collect();
    let mut examples = vec![];
    if manager.config.example.is_file() {
        examples.push(json!({"id":"synthetic", "label":"Synthetic offline replay"}));
    }
    if manager
        .config
        .service_dir
        .join("examples/web-debugger/source-replay.json")
        .is_file()
        && manager
            .config
            .service_dir
            .join("examples/web-debugger/build-info.json")
            .is_file()
    {
        examples.push(json!({"id":"source-demo", "label":"Withdrawal lab (synthetic)", "buildInfoId":"source-demo-build"}));
    }
    Json(
        json!({"available":manager.availability.is_none(), "protocol":PROTOCOL, "providers":providers,
        "modes":["offline","replay","fork","foundry"], "reason":manager.availability, "csrfToken":csrf, "examples":examples}),
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Upload {
    kind: String,
    name: String,
    content: Value,
}
async fn upload(
    State(manager): State<Arc<Manager>>,
    Extension(owner): Extension<Owner>,
    Json(body): Json<Upload>,
) -> Result<Json<ResourceInfo>, GatewayError> {
    tokio::task::spawn_blocking(move || {
        manager.register_resource(&owner, &body.kind, &body.name, body.content)
    })
    .await
    .map_err(|_| GatewayError::worker("Resource registration failed."))?
    .map(Json)
}

async fn build_info(
    State(manager): State<Arc<Manager>>,
    Extension(owner): Extension<Owner>,
) -> Result<Json<ResourceInfo>, GatewayError> {
    let build = manager.config.build_info.clone().ok_or_else(|| {
        GatewayError::bad(
            "Workspace build-info generation is unavailable; upload a build_info resource instead.",
        )
    })?;
    let permit = manager
        .build_admission
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            GatewayError::conflict("Workspace build-info generation is already in progress.")
        })?;
    {
        let registry = manager.registry.lock().unwrap();
        // Reserve enough capacity for the largest build before invoking a compiler.
        if registry.resources.len() >= 63
            || registry.resources.values().map(|r| r.size).sum::<usize>() > 56 * 1024 * 1024
        {
            return Err(GatewayError::conflict(
                "Debugger resource capacity is insufficient for a workspace build.",
            ));
        }
    }
    tokio::task::spawn_blocking(move || {
        // Keep admission even if the HTTP client disconnects: blocking compilation is not cancellable.
        let _permit = permit;
        let content = build().map_err(|_| GatewayError::bad("Workspace build-info generation failed. Compile with a locally installed Solidity compiler or upload build-info JSON."))?;
        manager.register_build_info(&owner, "Workspace build info", content)
    }).await.map_err(|_| GatewayError::worker("Workspace build-info task failed."))?.map(Json)
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Start {
    request_id: String,
    mode: String,
    capture_id: Option<String>,
    build_info_id: Option<String>,
    tx_hash: Option<String>,
    provider_id: Option<String>,
    block_number: Option<u64>,
    to: Option<String>,
    caller: Option<String>,
    data: Option<String>,
    value: Option<String>,
    gas: Option<u64>,
    nonce: Option<u64>,
    gas_price: Option<String>,
    target: Option<String>,
    r#match: Option<String>,
}

fn fingerprint<T: Serialize>(request: &T) -> Result<u64, GatewayError> {
    let bytes = serde_json::to_vec(request).map_err(|_| GatewayError::bad("Invalid request."))?;
    if bytes.len() > MAX_REQUEST - 1024 {
        return Err(GatewayError::bad("Debugger command is too large."));
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    Ok(hasher.finish())
}
fn request_id(id: &str) -> Result<(), GatewayError> {
    if id.is_empty() || id.len() > 128 || id.chars().any(char::is_control) {
        return Err(GatewayError::bad(
            "requestId must contain 1–128 non-control characters.",
        ));
    }
    Ok(())
}

async fn start(
    State(manager): State<Arc<Manager>>,
    Extension(owner): Extension<Owner>,
    Json(body): Json<Start>,
) -> Result<(StatusCode, Json<Envelope>), GatewayError> {
    request_id(&body.request_id)?;
    let hash = fingerprint(&body)?;
    {
        let registry = manager.registry.lock().unwrap();
        if let Some(session) = &registry.session {
            if session.owner == owner.0 {
                if let Some(previous) = session.requests.get(&body.request_id) {
                    if *previous == hash {
                        return Ok((StatusCode::ACCEPTED, Json(session.envelope.clone())));
                    }
                    return Err(GatewayError::conflict(
                        "requestId was already used for a different request.",
                    ));
                }
            }
            if session.active {
                return Err(GatewayError::conflict(
                    "Dispose the active debugger session before starting another.",
                ));
            }
        }
    }
    if manager.availability.is_some() {
        return Err(GatewayError::unavailable());
    }
    if !["offline", "replay", "fork", "foundry"].contains(&body.mode.as_str()) {
        return Err(GatewayError::bad("Unsupported debugger mode."));
    }
    let mut params = json!({"mode":body.mode});
    if let Some(id) = &body.capture_id {
        params["capture_path"] = json!(manager.resolve_resource(&owner, id, "capture")?);
    }
    if let Some(id) = &body.build_info_id {
        params["build_info_path"] = json!(manager.resolve_resource(&owner, id, "build_info")?);
    }
    let provider_env = body
        .provider_id
        .as_ref()
        .map(|id| {
            manager
                .config
                .providers
                .get(id)
                .cloned()
                .ok_or_else(|| GatewayError::bad("Unknown configured provider alias."))
        })
        .transpose()?;
    if let Some(env) = &provider_env {
        params["rpc_env"] = json!(env);
    }
    match body.mode.as_str() {
        "offline" if body.capture_id.is_none() => {
            return Err(GatewayError::bad("Offline mode requires captureId."))
        }
        "replay" if body.tx_hash.is_none() || provider_env.is_none() => {
            return Err(GatewayError::bad(
                "Replay requires txHash and a configured providerId.",
            ))
        }
        "fork" if provider_env.is_none() => {
            return Err(GatewayError::bad("Fork requires a configured providerId."))
        }
        "foundry" if body.target.is_none() => {
            return Err(GatewayError::bad(
                "Foundry mode requires a trusted workspace target.",
            ))
        }
        _ => {}
    }
    if let Some(target) = &body.target {
        if body.mode != "foundry" {
            return Err(GatewayError::bad(
                "target is supported only in foundry mode.",
            ));
        }
        params["target_path"] = json!(manager.target(target)?);
    }
    for (key, value) in [
        ("tx_hash", json!(body.tx_hash)),
        ("block_number", json!(body.block_number)),
        ("to", json!(body.to)),
        ("caller", json!(body.caller)),
        ("data", json!(body.data)),
        ("value", json!(body.value)),
        ("gas", json!(body.gas)),
        ("nonce", json!(body.nonce)),
        ("gas_price", json!(body.gas_price)),
        ("match", json!(body.r#match)),
    ] {
        if !value.is_null() {
            params[key] = value;
        }
    }
    let allowed: &[&str] = match body.mode.as_str() {
        "offline" => &["mode", "capture_path", "build_info_path"],
        "replay" => &[
            "mode",
            "capture_path",
            "build_info_path",
            "tx_hash",
            "rpc_env",
        ],
        "fork" => &[
            "mode",
            "build_info_path",
            "block_number",
            "rpc_env",
            "to",
            "caller",
            "data",
            "value",
            "gas",
            "nonce",
            "gas_price",
        ],
        "foundry" => &["mode", "target_path", "match"],
        _ => unreachable!(),
    };
    if params
        .as_object()
        .unwrap()
        .keys()
        .any(|key| !allowed.contains(&key.as_str()))
    {
        return Err(GatewayError::bad(
            "Request contains fields that are not supported in the selected debugger mode.",
        ));
    }
    let envelope = {
        let mut registry = manager.registry.lock().unwrap();
        if manager.stopping.load(std::sync::atomic::Ordering::Acquire) {
            return Err(GatewayError::unavailable());
        }
        if registry.session.as_ref().is_some_and(|s| s.active) {
            return Err(GatewayError::conflict(
                "A debugger session is already active.",
            ));
        }
        registry.generation += 1;
        let mut session = Session {
            owner: owner.0,
            active: true,
            worker: None,
            events: VecDeque::new(),
            event_bytes: 0,
            last_activity: Instant::now(),
            requests: HashMap::from([(body.request_id, hash)]),
            envelope: Envelope {
                id: random_id(),
                generation: registry.generation,
                revision: 0,
                sequence: 0,
                state: "preparing".into(),
                busy: true,
                snapshot: None,
                error: None,
                result: None,
            },
        };
        manager.publish(&mut session);
        let envelope = session.envelope.clone();
        registry.session = Some(session);
        envelope
    };
    let task_manager = manager.clone();
    let id = envelope.id.clone();
    tokio::spawn(async move {
        run_start(task_manager, id, params, provider_env).await;
    });
    Ok((StatusCode::ACCEPTED, Json(envelope)))
}

async fn run_start(manager: Arc<Manager>, id: String, params: Value, provider_env: Option<String>) {
    // Install the kill handle atomically with the active check before any await.
    let worker = {
        let mut registry = manager.registry.lock().unwrap();
        let Some(session) = registry
            .session
            .as_mut()
            .filter(|s| s.envelope.id == id && s.active && s.envelope.state != "cancelling")
        else {
            return;
        };
        match Worker::spawn(&manager.config, provider_env.as_deref()) {
            Ok(worker) => {
                session.worker = Some(worker.clone());
                Ok(worker)
            }
            Err(error) => Err(error),
        }
    };
    match worker {
        Ok(worker) => {
            let operation = async {
                worker.hello().await?;
                worker.request("start", params).await
            };
            let result = deadline(&manager, operation).await;
            finish(&manager, &id, "start", result, worker).await;
        }
        Err(error) => {
            let mut registry = manager.registry.lock().unwrap();
            if let Some(session) = registry
                .session
                .as_mut()
                .filter(|s| s.envelope.id == id && s.active && s.envelope.state != "cancelling")
            {
                session.envelope.busy = false;
                session.envelope.state = "failed".into();
                session.envelope.revision += 1;
                session.envelope.error = Some(error);
                manager.publish(session);
            }
        }
    }
}

async fn deadline<F>(manager: &Manager, operation: F) -> Result<Value, GatewayError>
where
    F: std::future::Future<Output = Result<Value, GatewayError>>,
{
    tokio::time::timeout(manager.config.timeout, operation).await.unwrap_or_else(|_| {
        let mut error = GatewayError::worker("Debugger operation timed out. Its outcome is unknown; the worker was terminated. Dispose this session before restarting; do not retry mutations.");
        error.code = "timeout_unknown".into(); Err(error)
    })
}

async fn finish(
    manager: &Manager,
    id: &str,
    method: &str,
    result: Result<Value, GatewayError>,
    worker: Arc<Worker>,
) {
    let fatal =
        result.as_ref().err().is_some_and(|e| e.fatal) || (method == "start" && result.is_err());
    if fatal {
        worker.killer.kill().await;
    }
    let mut registry = manager.registry.lock().unwrap();
    let Some(session) = registry
        .session
        .as_mut()
        .filter(|s| s.envelope.id == id && s.active && s.envelope.state != "cancelling")
    else {
        return;
    };
    session.envelope.busy = false;
    session.envelope.revision += 1;
    match result {
        Ok(result) => {
            let snapshot = if ["start", "snapshot", "navigate"].contains(&method) {
                Some(result.clone())
            } else {
                result.get("snapshot").filter(|v| v.is_object()).cloned()
            };
            if let Some(snapshot) = snapshot {
                session.envelope.state = snapshot_state(&snapshot).into();
                session.envelope.snapshot = Some(snapshot);
            } else if session.envelope.state == "running" {
                session.envelope.state = "paused".into();
            }
            session.envelope.result = if ["start", "snapshot", "navigate"].contains(&method) {
                None
            } else {
                let mut result = result;
                if let Some(object) = result.as_object_mut() {
                    object.remove("snapshot");
                }
                Some(result)
            };
            session.envelope.error = None;
        }
        Err(error) => {
            session.envelope.state = if fatal {
                "failed"
            } else {
                session
                    .envelope
                    .snapshot
                    .as_ref()
                    .map(snapshot_state)
                    .unwrap_or("paused")
            }
            .into();
            session.envelope.error = Some(error);
            session.envelope.result = None;
        }
    }
    if fatal {
        session.worker = None;
    }
    manager.publish(session);
}

fn envelope_bytes(envelope: &Envelope) -> usize {
    // Counting serialization avoids retaining an extra large encoded copy in the event ring.
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    let _ = serde_json::to_writer(&mut count, envelope);
    count.0
}

fn snapshot_state(snapshot: &Value) -> &'static str {
    let status = snapshot.get("status");
    let state = status
        .and_then(Value::as_str)
        .or_else(|| status.and_then(|s| s.get("state")).and_then(Value::as_str))
        .or_else(|| status.and_then(|s| s.get("status")).and_then(Value::as_str));
    if status.is_some_and(|s| {
        s.get("finished").and_then(Value::as_bool) == Some(true)
            || s.get("halted").and_then(Value::as_bool) == Some(true)
    }) || snapshot.get("finished").and_then(Value::as_bool) == Some(true)
    {
        return "finished";
    }
    match state {
        Some("finished" | "halted" | "completed" | "reverted" | "stopped") => "finished",
        Some("running") => "running",
        _ => "paused",
    }
}

async fn session(
    State(manager): State<Arc<Manager>>,
    Extension(owner): Extension<Owner>,
    Path(id): Path<String>,
) -> Result<Json<Envelope>, GatewayError> {
    let registry = manager.registry.lock().unwrap();
    let session = owned_session(&registry, &owner, &id)?;
    Ok(Json(session.envelope.clone()))
}
fn owned_session<'a>(
    registry: &'a Registry,
    owner: &Owner,
    id: &str,
) -> Result<&'a Session, GatewayError> {
    registry
        .session
        .as_ref()
        .filter(|s| s.envelope.id == id && s.owner == owner.0)
        .ok_or_else(GatewayError::missing)
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CommandRequest {
    request_id: String,
    generation: u64,
    expected_revision: u64,
    method: String,
    params: Value,
}

fn safe_params(params: &Value, method: &str) -> bool {
    match params {
        Value::Object(map) => map.iter().all(|(key, value)| {
            let key = key.to_ascii_lowercase();
            if key == "target" {
                return method == "mutate"
                    && value.as_str().is_some_and(|target| {
                        ["storage", "memory", "stack", "gas", "pc", "variable"].contains(&target)
                    });
            }
            ![
                "path",
                "capture_path",
                "build_info_path",
                "target_path",
                "captureid",
                "buildinfoid",
                "rpc_url",
                "rpcurl",
                "rpc_env",
                "providerid",
                "url",
                "workspace",
                "cwd",
                "output_path",
                "filename",
            ]
            .contains(&key.as_str())
                && safe_params(value, method)
        }),
        Value::Array(items) => items.iter().all(|item| safe_params(item, method)),
        _ => true,
    }
}

async fn command(
    State(manager): State<Arc<Manager>>,
    Extension(owner): Extension<Owner>,
    Path(id): Path<String>,
    Json(body): Json<CommandRequest>,
) -> Result<(StatusCode, Json<Envelope>), GatewayError> {
    request_id(&body.request_id)?;
    if ![
        "snapshot",
        "navigate",
        "breakpoint",
        "evaluate",
        "mutate",
        "command",
        "export_capture",
    ]
    .contains(&body.method.as_str())
        || !body.params.is_object()
        || !safe_params(&body.params, &body.method)
    {
        return Err(GatewayError::bad(
            "Unsupported debugger method or unsafe command parameters.",
        ));
    }
    let hash = fingerprint(&body)?;
    let (worker, envelope) = {
        let mut registry = manager.registry.lock().unwrap();
        let session = registry
            .session
            .as_mut()
            .filter(|s| s.envelope.id == id && s.owner == owner.0)
            .ok_or_else(GatewayError::missing)?;
        if let Some(previous) = session.requests.get(&body.request_id) {
            if *previous == hash {
                return Ok((StatusCode::ACCEPTED, Json(session.envelope.clone())));
            }
            return Err(GatewayError::conflict(
                "requestId was already used for a different request.",
            ));
        }
        if !session.active || session.envelope.state == "failed" {
            return Err(GatewayError::conflict(
                "Debugger session must be disposed and restarted.",
            ));
        }
        if session.envelope.generation != body.generation
            || session.envelope.revision != body.expected_revision
        {
            return Err(GatewayError::conflict(
                "Stale debugger generation or revision; fetch the latest session.",
            ));
        }
        if session.envelope.busy {
            return Err(GatewayError::conflict(
                "A debugger operation is already in flight.",
            ));
        }
        if session.requests.len() >= 256 {
            return Err(GatewayError::conflict("Session request history is full; dispose and start another session. Request ids are never evicted or replayed."));
        }
        let worker = session
            .worker
            .clone()
            .ok_or_else(|| GatewayError::conflict("Debugger worker is unavailable."))?;
        session.requests.insert(body.request_id, hash);
        session.last_activity = Instant::now();
        session.envelope.busy = true;
        session.envelope.state = "running".into();
        session.envelope.result = None;
        session.envelope.error = None;
        manager.publish(session);
        (worker, session.envelope.clone())
    };
    let task_manager = manager.clone();
    tokio::spawn(async move {
        let result = deadline(&task_manager, worker.request(&body.method, body.params)).await;
        finish(&task_manager, &id, &body.method, result, worker).await;
    });
    Ok((StatusCode::ACCEPTED, Json(envelope)))
}

async fn dispose(
    State(manager): State<Arc<Manager>>,
    Extension(owner): Extension<Owner>,
    Path(id): Path<String>,
) -> Result<Json<Envelope>, GatewayError> {
    let worker = {
        let mut registry = manager.registry.lock().unwrap();
        let session = registry
            .session
            .as_mut()
            .filter(|s| s.envelope.id == id && s.owner == owner.0)
            .ok_or_else(GatewayError::missing)?;
        if !session.active {
            return Ok(Json(session.envelope.clone()));
        }
        session.envelope.state = "cancelling".into();
        session.envelope.busy = true;
        manager.publish(session);
        session.worker.clone()
    };
    // Run cancellation in its own task so disconnecting the DELETE cannot leave a half-disposed session.
    let cleanup = manager.clone();
    tokio::spawn(async move {
        if let Some(worker) = worker {
            worker.killer.kill().await;
        }
        let mut registry = cleanup.registry.lock().unwrap();
        let session = registry
            .session
            .as_mut()
            .filter(|s| s.envelope.id == id)
            .ok_or_else(GatewayError::missing)?;
        session.active = false;
        session.worker = None;
        session.envelope.busy = false;
        session.envelope.state = "cancelled".into();
        session.envelope.error = None;
        session.envelope.result = None;
        session.envelope.revision += 1;
        cleanup.publish(session);
        Ok::<_, GatewayError>(Json(session.envelope.clone()))
    })
    .await
    .map_err(|_| GatewayError::worker("Debugger cancellation failed."))?
}

#[derive(Deserialize)]
struct EventQuery {
    after: Option<u64>,
    sequence: Option<u64>,
}
async fn events(
    State(manager): State<Arc<Manager>>,
    Extension(owner): Extension<Owner>,
    Path(id): Path<String>,
    Query(query): Query<EventQuery>,
    websocket: WebSocketUpgrade,
) -> Result<Response, GatewayError> {
    {
        let registry = manager.registry.lock().unwrap();
        owned_session(&registry, &owner, &id)?;
    }
    Ok(websocket.on_upgrade(move |socket| {
        event_socket(socket, manager, owner, id, query.after.or(query.sequence))
    }))
}

async fn event_socket(
    mut socket: WebSocket,
    manager: Arc<Manager>,
    owner: Owner,
    id: String,
    after: Option<u64>,
) {
    let mut receiver = manager.events.subscribe();
    let initial = {
        let registry = manager.registry.lock().unwrap();
        match owned_session(&registry, &owner, &id) {
            Ok(session) => {
                let missed: Vec<_> = after
                    .filter(|n| {
                        *n < session.envelope.sequence
                            && session
                                .events
                                .front()
                                .is_some_and(|e| n.saturating_add(1) >= e.sequence)
                    })
                    .map(|n| {
                        session
                            .events
                            .iter()
                            .filter(|e| e.sequence > n)
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                if missed.is_empty() {
                    vec![session.envelope.clone()]
                } else {
                    missed
                }
            }
            Err(_) => return,
        }
    };
    let mut last = 0;
    for envelope in initial {
        last = envelope.sequence;
        if !send_event(&mut socket, &envelope).await {
            return;
        }
    }
    let mut heartbeat = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                if manager.stopping.load(std::sync::atomic::Ordering::Acquire) {
                    let _ = tokio::time::timeout(Duration::from_secs(1), socket.send(Message::Close(None))).await;
                    return;
                }
            },
            incoming = socket.recv() => match incoming { Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return, _ => {} },
            event = receiver.recv() => {
                let envelope = match event {
                    Err(broadcast::error::RecvError::Closed) => return,
                    Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => {
                        let registry = manager.registry.lock().unwrap();
                        match owned_session(&registry, &owner, &id) { Ok(s) => s.envelope.clone(), Err(_) => return }
                    }
                };
                if envelope.sequence <= last { continue; }
                last = envelope.sequence;
                if !send_event(&mut socket, &envelope).await { return; }
            }
        }
    }
}

async fn send_event(socket: &mut WebSocket, envelope: &Envelope) -> bool {
    matches!(
        tokio::time::timeout(
            Duration::from_secs(5),
            socket.send(Message::Text(serde_json::to_string(envelope).unwrap()))
        )
        .await,
        Ok(Ok(()))
    )
}

#[cfg(test)]
mod tests;
