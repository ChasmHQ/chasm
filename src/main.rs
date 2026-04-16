#![allow(non_snake_case)]
mod compiler;
mod config;
mod watcher;
mod anvil;

use axum::{
    extract::{ws::{Message, WebSocket, WebSocketUpgrade}, Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
    Json,
};
use clap::Parser;
use include_dir::{include_dir, Dir};
use std::{net::SocketAddr, path::PathBuf, process::Command, sync::{Arc, Mutex}};
use tokio::sync::broadcast;
use tower_http::cors::CorsLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use crate::compiler::Compiler;
use walkdir::WalkDir;
use serde::{Deserialize, Serialize};
use ethers::types::U256;

struct AppState {
    tx: broadcast::Sender<String>,
    last_msg: Arc<Mutex<Option<String>>>,
    fork_node: Arc<Mutex<anvil::AnvilNode>>,
    root_dir: PathBuf,
    remappings: Arc<Mutex<Vec<String>>>,
}

// ─── Helper: trigger recompile & broadcast via WebSocket ─────────────────────

async fn trigger_recompile(state: &Arc<AppState>) {
    let remappings = state.remappings.lock().unwrap().clone();
    let root = state.root_dir.clone();
    let tx = state.tx.clone();
    let last_msg = state.last_msg.clone();
    tokio::task::spawn_blocking(move || {
        match Compiler::new(root, remappings) {
            Ok(compiler) => match compiler.compile_to_json() {
                Ok(json) => {
                    if let Ok(mut lock) = last_msg.lock() {
                        *lock = Some(json.clone());
                    }
                    let _ = tx.send(json);
                }
                Err(e) => {
                    let msg = serde_json::json!({
                        "type": "compile_error",
                        "error": e.to_string()
                    }).to_string();
                    if let Ok(mut lock) = last_msg.lock() {
                        *lock = Some(msg.clone());
                    }
                    let _ = tx.send(msg);
                }
            },
            Err(e) => {
                let msg = serde_json::json!({
                    "type": "compile_error",
                    "error": e.to_string()
                }).to_string();
                let _ = tx.send(msg);
            }
        }
    });
}

// ─── Manual recompile ────────────────────────────────────────────────────────

async fn manual_recompile(State(state): State<Arc<AppState>>) -> Response {
    trigger_recompile(&state).await;
    Json(serde_json::json!({ "status": "recompiling" })).into_response()
}

// ─── Request/Response types ───────────────────────────────────────────────────

#[derive(Deserialize)]
struct TraceParams {
    rpc_url: Option<String>,
}

#[derive(Deserialize)]
struct TraceCalltreeRequest {
    rpcUrl: String,
    call: serde_json::Value,
    blockTag: Option<String>,
}

#[derive(Deserialize)]
struct TraceCallRequest {
    rpcUrl: String,
    call: serde_json::Value,
    blockTag: Option<String>,
}

#[derive(Deserialize)]
struct ForkStartRequest {
    rpcUrl: String,
    blockNumber: Option<u64>,
}

#[derive(Serialize)]
struct ForkStatusResponse {
    running: bool,
    rpcUrl: Option<String>,
    blockNumber: Option<u64>,
    port: u16,
}

#[derive(Serialize)]
struct KeystoreListResponse {
    accounts: Vec<String>,
}

#[derive(Deserialize)]
struct KeystoreUnlockRequest {
    account: String,
    password: String,
}

#[derive(Deserialize)]
struct KeystoreCreateRequest {
    account: String,
    password: String,
    privateKey: Option<String>,
}

#[derive(Serialize)]
struct KeystoreUnlockResponse {
    privateKey: String,
}

#[derive(Deserialize)]
struct ProxyRequest {
    url: String,
    method: String,
    params: Option<serde_json::Value>,
    id: Option<u64>,
    jsonrpc: Option<String>,
}

// OZ-specific request/response types
#[derive(Deserialize)]
struct OzInstallLocalRequest {
    version: String,
}

#[derive(Deserialize)]
struct OzInstallGlobalRequest {
    version: String,
}

#[derive(Deserialize)]
struct OzUseGlobalRequest {
    version: String,
}

// ─── Proxy ────────────────────────────────────────────────────────────────────

async fn handle_proxy_request(
    Json(payload): Json<ProxyRequest>,
) -> Response {
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "jsonrpc": payload.jsonrpc.unwrap_or("2.0".to_string()),
        "method": payload.method,
        "params": payload.params.unwrap_or(serde_json::json!([])),
        "id": payload.id.unwrap_or(1)
    });

    match client.post(&payload.url).json(&body).send().await {
        Ok(res) => {
            let status = StatusCode::from_u16(res.status().as_u16()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            match res.json::<serde_json::Value>().await {
                Ok(data) => Json::<serde_json::Value>(data).into_response(),
                Err(_) => status.into_response()
            }
        },
        Err(e) => Json(serde_json::json!({"error": format!("Proxy failed: {}", e)})).into_response()
    }
}

static UI_DIR: Dir = include_dir!("$CARGO_MANIFEST_DIR/ui/dist");

#[derive(Parser, Debug)]
#[command(name = "chasm", about = "ChainSmith CLI")]
struct Cli {
    #[arg(value_name = "path", default_value = ".")]
    path: PathBuf,
}

// ─── Main ─────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    let args = Cli::parse();
    let root_dir = args.path.canonicalize().unwrap_or(args.path);

    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "chainsmith=debug,tower_http=debug".into()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    tracing::info!("Starting ChainSmith...");

    // ── OZ scan & remapping setup ────────────────────────────────────────────
    let mut chasm_config = config::ChasmConfig::load(&root_dir).unwrap_or_default();

    if let Some(oz_path) = config::scan_openzeppelin(&root_dir) {
        tracing::info!("Found openzeppelin-contracts at: {:?}", oz_path);
        let remapping = config::generate_remapping(&oz_path, &root_dir);
        let had_oz = chasm_config.remappings.iter().any(|r| r.contains("@openzeppelin"));
        if !had_oz {
            chasm_config.add_remapping(remapping.clone());
            if let Err(e) = chasm_config.save(&root_dir) {
                tracing::warn!("Could not save chasm.toml: {}", e);
            } else {
                tracing::info!("Created .chasm/chasm.toml with remapping: {}", remapping);
            }
        }
    } else {
        tracing::info!("No openzeppelin-contracts found in project. Skipping remapping setup.");
    }

    let remappings = Arc::new(Mutex::new(chasm_config.remappings.clone()));

    // ── Channel for broadcasting updates ─────────────────────────────────────
    let (tx, _rx) = broadcast::channel(100);
    let last_msg = Arc::new(Mutex::new(None));

    // ── Start Anvil (Primary) ─────────────────────────────────────────────────
    let mut anvil = anvil::AnvilNode::new(8545);
    if let Err(e) = anvil.start() {
        tracing::error!("Failed to start anvil: {}", e);
    } else {
        tracing::info!("Anvil started on port 8545");
    }

    let fork_node = Arc::new(Mutex::new(anvil::AnvilNode::new(8546)));

    // ── Initial Compile ───────────────────────────────────────────────────────
    tracing::info!("Performing initial compilation...");
    let initial_remappings = remappings.lock().unwrap().clone();
    match Compiler::new(root_dir.clone(), initial_remappings) {
        Ok(compiler) => match compiler.compile_to_json() {
            Ok(json) => {
                tracing::info!("Initial compilation successful. Payload size: {}", json.len());
                if let Ok(mut lock) = last_msg.lock() {
                    *lock = Some(json);
                }
            },
            Err(e) => {
                tracing::error!("Initial compilation failed: {}", e);
                let err_msg = serde_json::json!({
                    "type": "compile_error",
                    "error": e.to_string()
                }).to_string();
                if let Ok(mut lock) = last_msg.lock() {
                    *lock = Some(err_msg);
                }
            }
        },
        Err(e) => tracing::error!("Failed to create compiler: {}", e),
    }

    // ── File Watcher ──────────────────────────────────────────────────────────
    let tx_for_watcher = tx.clone();
    let last_msg_for_watcher = last_msg.clone();
    let remappings_for_watcher = remappings.clone();
    if let Err(e) = watcher::setup_watcher(
        root_dir.clone(),
        tx_for_watcher,
        last_msg_for_watcher,
        remappings_for_watcher,
    ).await {
        tracing::error!("Failed to setup watcher: {}", e);
    }

    let app_state = Arc::new(AppState { tx, last_msg, fork_node, root_dir, remappings });

    // ── Router ────────────────────────────────────────────────────────────────
    let app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/compile", post(manual_recompile))
        .route("/inspect/:contract", get(inspect_storage))
        .route("/trace/:tx_hash", get(get_trace))
        .route("/trace/calltree", post(get_trace_calltree))
        .route("/trace/call", post(get_trace_call))
        .route("/fork/start", post(start_fork))
        .route("/fork/stop", post(stop_fork))
        .route("/fork/status", get(fork_status))
        .route("/keystores", get(list_keystores))
        .route("/keystores/unlock", post(unlock_keystore))
        .route("/keystores/create", post(create_keystore))
        .route("/keystores/remove", post(remove_keystore))
        .route("/proxy", post(handle_proxy_request))
        // ── OZ Remapping endpoints ──
        .route("/oz/status", get(oz_status))
        .route("/oz/global-versions", get(oz_global_versions))
        .route("/oz/install/local", post(oz_install_local))
        .route("/oz/install/global", post(oz_install_global))
        .route("/oz/use-global", post(oz_use_global))
        .route("/oz/use-local", post(oz_use_local))
        .route("/", get(serve_ui_root))
        .route("/*path", get(serve_ui))
        .layer(CorsLayer::permissive())
        .with_state(app_state);

    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    tracing::info!("ChainSmith UI listening on http://{}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

// ─── OZ Endpoints ─────────────────────────────────────────────────────────────

/// GET /oz/status — status instalasi OZ (lokal & global)
async fn oz_status(State(state): State<Arc<AppState>>) -> Response {
    let local_oz = config::scan_openzeppelin(&state.root_dir);
    let local_path = local_oz.as_ref().map(|p| {
        p.strip_prefix(&state.root_dir)
            .map(|rel| rel.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| p.to_string_lossy().replace('\\', "/"))
    });

    let global_versions = config::list_global_oz_versions();
    let active_remappings = state.remappings.lock().unwrap().clone();

    // Parse active OZ version from remappings:
    // - Global path contains "openzeppelin-contracts@X.Y.Z" → extract version string
    // - Local path contains "lib/openzeppelin-contracts" (no @) → "local"
    // - No OZ remapping → null
    let active_version: Option<String> = active_remappings.iter()
        .find(|r| r.contains("@openzeppelin"))
        .and_then(|r| {
            let path = r.splitn(2, '=').nth(1).unwrap_or("");
            if let Some(start) = path.find("openzeppelin-contracts@") {
                let after = &path[start + "openzeppelin-contracts@".len()..];
                let version = after.split('/').next().unwrap_or("").trim_end_matches('/');
                if !version.is_empty() {
                    return Some(version.to_string());
                }
            }
            if path.contains("lib/openzeppelin-contracts") || path.contains("lib\\openzeppelin-contracts") {
                return Some("local".to_string());
            }
            None
        });

    Json(serde_json::json!({
        "local": local_oz.is_some(),
        "localPath": local_path,
        "globalVersions": global_versions,
        "activeRemappings": active_remappings,
        "activeVersion": active_version,
    })).into_response()
}

/// POST /oz/use-local — switch ke local OZ yang ada di lib/openzeppelin-contracts
async fn oz_use_local(State(state): State<Arc<AppState>>) -> Response {
    let root = &state.root_dir;
    let dest = root.join("lib").join("openzeppelin-contracts");
    if !dest.exists() {
        return Json(serde_json::json!({
            "error": "No local OZ found at lib/openzeppelin-contracts. Install it first via Local Install."
        })).into_response();
    }

    let remapping = config::generate_remapping(&dest, root);
    let mut cfg = config::ChasmConfig::load(root).unwrap_or_default();
    cfg.replace_oz_remapping(remapping.clone());
    if let Err(e) = cfg.save(root) {
        tracing::warn!("Failed to save chasm.toml: {}", e);
    }
    {
        let mut rem = state.remappings.lock().unwrap();
        rem.retain(|r| !r.contains("@openzeppelin"));
        rem.push(remapping);
    }
    trigger_recompile(&state).await;
    Json(serde_json::json!({ "ok": true })).into_response()
}

/// GET /oz/global-versions — list versi OZ global yang tersedia
async fn oz_global_versions() -> Response {
    Json(serde_json::json!({
        "versions": config::list_global_oz_versions()
    })).into_response()
}

/// POST /oz/install/local — clone OZ into project's lib/ folder via git clone
async fn oz_install_local(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<OzInstallLocalRequest>,
) -> Response {
    let root = &state.root_dir;
    let version = payload.version.trim().to_string();

    if version.is_empty() {
        return Json(serde_json::json!({
            "error": "Version cannot be empty"
        })).into_response();
    }

    let dest = root.join("lib").join("openzeppelin-contracts");

    if dest.exists() {
        return Json(serde_json::json!({
            "error": "lib/openzeppelin-contracts already exists. Remove it first before installing a different version."
        })).into_response();
    }

    // Create lib/ if it doesn't exist
    if let Err(e) = std::fs::create_dir_all(root.join("lib")) {
        return Json(serde_json::json!({
            "error": format!("Failed to create lib/ directory: {}", e)
        })).into_response();
    }

    let branch = format!("v{}", version);
    // Strip Windows UNC extended-length prefix (\\?\) — produced by canonicalize() on Windows,
    // not accepted by git as a destination path. No-op on macOS/Linux.
    let dest_str = dest.to_string_lossy().to_string();
    let dest_str = dest_str.strip_prefix(r"\\?\").unwrap_or(&dest_str).to_string();

    tracing::info!("git clone openzeppelin-contracts@{} into {:?}", version, dest);

    let clone_result = tokio::task::spawn_blocking(move || {
        Command::new("git")
            .arg("clone")
            .arg("https://github.com/OpenZeppelin/openzeppelin-contracts.git")
            .arg("--branch").arg(&branch)
            .arg("--depth").arg("1")
            .arg(&dest_str)
            .output()
    }).await;

    let out = match clone_result {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => return Json(serde_json::json!({
            "error": format!("Failed to run git clone: {}", e)
        })).into_response(),
        Err(e) => return Json(serde_json::json!({
            "error": format!("Task spawn error: {}", e)
        })).into_response(),
    };

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let _ = std::fs::remove_dir_all(&dest);
        return Json(serde_json::json!({
            "error": format!("git clone failed: {}", stderr)
        })).into_response();
    }

    // Update chasm.toml with new remapping
    let remapping = config::generate_remapping(&dest, root);
    let mut cfg = config::ChasmConfig::load(root).unwrap_or_default();
    cfg.replace_oz_remapping(remapping.clone());
    if let Err(e) = cfg.save(root) {
        tracing::warn!("Failed to save chasm.toml: {}", e);
    }

    // Update AppState remappings
    {
        let mut rem = state.remappings.lock().unwrap();
        rem.retain(|r| !r.contains("@openzeppelin"));
        rem.push(remapping);
    }

    // Trigger recompile
    trigger_recompile(&state).await;

    Json(serde_json::json!({ "success": true })).into_response()
}

/// POST /oz/install/global — download OZ ke ~/.chasm/lib/ via git clone
async fn oz_install_global(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<OzInstallGlobalRequest>,
) -> Response {
    let version = payload.version.trim().to_string();

    if version.is_empty() {
        return Json(serde_json::json!({
            "error": "version tidak boleh kosong"
        })).into_response();
    }

    let dest = config::global_oz_path(&version);

    if dest.exists() {
        // Versi sudah ada, langsung gunakan
        return oz_apply_global_version(&state, &version).await;
    }

    // Buat direktori lib global jika belum ada
    let lib_dir = config::global_lib_dir();
    if let Err(e) = std::fs::create_dir_all(&lib_dir) {
        return Json(serde_json::json!({
            "error": format!("Gagal membuat direktori global lib: {}", e)
        })).into_response();
    }

    let branch = format!("v{}", version);
    // Strip Windows UNC extended-length prefix (\\?\) — produced by canonicalize() on Windows,
    // not accepted by git as a destination path. No-op on macOS/Linux.
    let dest_str = dest.to_string_lossy().to_string();
    let dest_str = dest_str.strip_prefix(r"\\?\").unwrap_or(&dest_str).to_string();

    tracing::info!("git clone openzeppelin-contracts@{} ke {}", version, dest_str);

    // Jalankan di spawn_blocking karena git clone bisa makan waktu beberapa menit
    let branch_clone = branch.clone();
    let dest_clone = dest_str.clone();
    let clone_result = tokio::task::spawn_blocking(move || {
        Command::new("git")
            .arg("clone")
            .arg("https://github.com/OpenZeppelin/openzeppelin-contracts.git")
            .arg("--branch")
            .arg(&branch_clone)
            .arg("--depth")
            .arg("1")
            .arg(&dest_clone)
            .output()
    }).await;

    match clone_result {
        Ok(Ok(out)) => {
            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr);
                // Hapus direktori partial jika clone gagal
                let _ = std::fs::remove_dir_all(&dest_str);
                return Json(serde_json::json!({
                    "error": format!("git clone gagal: {}", stderr)
                })).into_response();
            }
            oz_apply_global_version(&state, &version).await
        }
        Ok(Err(e)) => Json(serde_json::json!({
            "error": format!("Gagal menjalankan git clone: {}", e)
        })).into_response(),
        Err(e) => Json(serde_json::json!({
            "error": format!("Task spawn error: {}", e)
        })).into_response(),
    }
}

/// POST /oz/use-global — gunakan versi OZ global yang sudah ada
async fn oz_use_global(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<OzUseGlobalRequest>,
) -> Response {
    let version = payload.version.trim().to_string();

    if version.is_empty() {
        return Json(serde_json::json!({
            "error": "version tidak boleh kosong"
        })).into_response();
    }

    let dest = config::global_oz_path(&version);
    if !dest.exists() {
        return Json(serde_json::json!({
            "error": format!("Versi {} belum didownload. Gunakan /oz/install/global terlebih dahulu.", version)
        })).into_response();
    }

    oz_apply_global_version(&state, &version).await
}

/// Helper: terapkan remapping global OZ, simpan ke chasm.toml, trigger recompile
async fn oz_apply_global_version(state: &Arc<AppState>, version: &str) -> Response {
    let oz_path = config::global_oz_path(version);
    let root = &state.root_dir;

    // Generate remapping dengan absolute path ke global dir
    let remapping = config::generate_remapping(&oz_path, root);

    let mut cfg = config::ChasmConfig::load(root).unwrap_or_default();
    cfg.replace_oz_remapping(remapping.clone());
    if let Err(e) = cfg.save(root) {
        tracing::warn!("Gagal menyimpan chasm.toml: {}", e);
    }

    // Update AppState remappings
    {
        let mut rem = state.remappings.lock().unwrap();
        rem.retain(|r| !r.contains("@openzeppelin"));
        rem.push(remapping);
    }

    // Trigger recompile
    trigger_recompile(state).await;

    Json(serde_json::json!({ "success": true, "version": version })).into_response()
}

// ─── WebSocket ────────────────────────────────────────────────────────────────

async fn serve_ui_root() -> Response {
    serve_ui(Path("".to_string())).await
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
) -> Response {
    ws.on_upgrade(|socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: Arc<AppState>) {
    let cached_msg = {
        let lock = state.last_msg.lock().unwrap();
        lock.clone()
    };

    if let Some(msg) = cached_msg {
        let _ = socket.send(Message::Text(msg)).await;
    }

    let mut rx = state.tx.subscribe();

    while let Ok(msg) = rx.recv().await {
        if socket.send(Message::Text(msg)).await.is_err() {
            break;
        }
    }
}

// ─── Storage / Trace ──────────────────────────────────────────────────────────

async fn inspect_storage(
    Path(contract): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Response {
    tracing::info!("Inspecting storage for {}", contract);

    let remappings = state.remappings.lock().unwrap().clone();
    let root = state.root_dir.clone();
    let contract_name = contract.clone();

    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<serde_json::Value> {
        let compiler = Compiler::new_for_storage_layout(root, remappings)?;
        let artifacts = compiler.compile()?;
        for (name, _, artifact) in artifacts {
            if name == contract_name {
                if let Some(layout) = artifact.storage_layout {
                    return Ok(serde_json::to_value(&layout)?);
                }
                return Err(anyhow::anyhow!(
                    "No storage layout for {} (interface or abstract contract)",
                    contract_name
                ));
            }
        }
        Err(anyhow::anyhow!("Contract {} not found in compilation", contract_name))
    }).await;

    match result {
        Ok(Ok(layout)) => Json(layout).into_response(),
        Ok(Err(e)) => Json(serde_json::json!({ "error": e.to_string() })).into_response(),
        Err(e) => Json(serde_json::json!({ "error": format!("Task error: {}", e) })).into_response(),
    }
}

async fn get_trace(
    Path(tx_hash): Path<String>,
    Query(params): Query<TraceParams>,
    State(state): State<Arc<AppState>>,
) -> Response {
    let rpc_url = params.rpc_url.unwrap_or("http://127.0.0.1:8545".to_string());
    tracing::info!("Tracing tx {} on {}", tx_hash, rpc_url);

    let output = Command::new("cast")
        .current_dir(&state.root_dir)
        .arg("run")
        .arg(&tx_hash)
        .arg("--rpc-url")
        .arg(&rpc_url)
        .output();

    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            Json(serde_json::json!({
                "stdout": stdout,
                "stderr": stderr
            })).into_response()
        },
        Err(e) => Json(serde_json::json!({
            "error": format!("Failed to execute cast: {}", e)
        })).into_response()
    }
}

async fn get_trace_call(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<TraceCallRequest>,
) -> Response {
    let url = payload.rpcUrl;
    let block_tag = payload.blockTag.unwrap_or("latest".to_string());

    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "debug_traceCall",
        "params": [payload.call, block_tag]
    });

    let output = Command::new("curl")
        .current_dir(&state.root_dir)
        .arg("-sS")
        .arg("-X")
        .arg("POST")
        .arg(&url)
        .arg("-H")
        .arg("Content-Type: application/json")
        .arg("-d")
        .arg(body.to_string())
        .output();

    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            if !out.status.success() {
                return Json(serde_json::json!({
                    "error": format!("Trace call failed: {}", stderr)
                })).into_response();
            }
            if stdout.trim().is_empty() {
                return Json(serde_json::json!({
                    "error": format!("Empty trace response: {}", stderr)
                })).into_response();
            }
            Json(serde_json::json!({
                "stdout": stdout,
                "stderr": stderr
            })).into_response()
        }
        Err(e) => Json(serde_json::json!({
            "error": format!("Failed to execute trace call: {}", e)
        })).into_response()
    }
}

async fn get_trace_calltree(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<TraceCalltreeRequest>,
) -> Response {
    let rpc_url = payload.rpcUrl;
    let block_tag = payload.blockTag.unwrap_or("latest".to_string());

    let to = payload.call.get("to").and_then(|v| v.as_str()).unwrap_or("");
    let data = payload.call.get("data").and_then(|v| v.as_str()).unwrap_or("0x");
    let value = payload.call.get("value").and_then(|v| v.as_str());
    let from = payload.call.get("from").and_then(|v| v.as_str());
    let gas = payload.call.get("gas").and_then(|v| v.as_str());

    let mut cmd = Command::new("cast");
    cmd.current_dir(&state.root_dir);
    cmd.arg("call");
    cmd.arg("--rpc-url").arg(&rpc_url);
    cmd.arg("--trace");
    cmd.arg("--gas-price").arg("0");
    if let Some(f) = from {
        cmd.arg("--from").arg(f);
    }
    if let Some(g) = gas {
        let cleaned = g.strip_prefix("0x").unwrap_or(g);
        if !cleaned.is_empty() && cleaned != "0" {
            if let Ok(val) = U256::from_str_radix(cleaned, 16) {
                cmd.arg("--gas").arg(val.to_string());
            }
        }
    }
    cmd.arg("--block").arg(&block_tag);

    if to.is_empty() {
        cmd.arg("--create");
        cmd.arg(data);
    } else {
        cmd.arg(to);
        cmd.arg(data);
    }
    if let Some(v) = value {
        let cleaned = v.strip_prefix("0x").unwrap_or(v);
        if !cleaned.is_empty() && cleaned != "0" {
            let decimal_value = if v.starts_with("0x") {
                match U256::from_str_radix(cleaned, 16) {
                    Ok(val) => val.to_string(),
                    Err(_) => cleaned.to_string(),
                }
            } else {
                cleaned.to_string()
            };
            cmd.arg("--value").arg(decimal_value);
        }
    }

    let output = cmd.output();

    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            if !out.status.success() {
                return Json(serde_json::json!({
                    "error": format!("Cast trace failed: {}", stderr)
                })).into_response();
            }
            if stdout.trim().is_empty() {
                return Json(serde_json::json!({
                    "error": format!("Empty trace response: {}", stderr)
                })).into_response();
            }
            Json(serde_json::json!({
                "stdout": stdout,
                "stderr": stderr
            })).into_response()
        }
        Err(e) => Json(serde_json::json!({
            "error": format!("Failed to execute cast trace: {}", e)
        })).into_response()
    }
}

// ─── UI ───────────────────────────────────────────────────────────────────────

async fn serve_ui(Path(path): Path<String>) -> Response {
    let trimmed = path.trim_start_matches('/');
    let file_path = if trimmed.is_empty() { "index.html" } else { trimmed };
    let file = UI_DIR.get_file(file_path).or_else(|| UI_DIR.get_file("index.html"));

    if let Some(file) = file {
        let mime = mime_guess::from_path(file.path()).first_or_octet_stream();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_str(mime.as_ref()).unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
        );
        return (StatusCode::OK, headers, file.contents()).into_response();
    }

    StatusCode::NOT_FOUND.into_response()
}

// ─── Fork ─────────────────────────────────────────────────────────────────────

async fn start_fork(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<ForkStartRequest>,
) -> Response {
    let mut node = state.fork_node.lock().unwrap();
    if node.is_running() {
        node.stop();
    }

    match node.start_fork(payload.rpcUrl.clone(), payload.blockNumber) {
        Ok(_) => {
            Json(serde_json::json!({
                "status": "running",
                "rpcUrl": payload.rpcUrl,
                "blockNumber": payload.blockNumber,
                "port": node.port(),
            })).into_response()
        }
        Err(e) => Json(serde_json::json!({
            "error": format!("Failed to start forked anvil: {}", e)
        })).into_response()
    }
}

async fn stop_fork(State(state): State<Arc<AppState>>) -> Response {
    let mut node = state.fork_node.lock().unwrap();
    node.stop();
    Json(serde_json::json!({ "status": "stopped" })).into_response()
}

async fn fork_status(State(state): State<Arc<AppState>>) -> Response {
    let node = state.fork_node.lock().unwrap();
    let (rpc_url, block_number) = node.fork_info();
    let payload = ForkStatusResponse {
        running: node.is_running(),
        rpcUrl: rpc_url,
        blockNumber: block_number,
        port: node.port(),
    };
    Json(payload).into_response()
}

// ─── Keystores ────────────────────────────────────────────────────────────────

async fn list_keystores() -> Response {
    let mut accounts = Vec::new();
    if let Ok(home) = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")) {
        let keystore_dir = PathBuf::from(home).join(".foundry").join("keystores");
        if keystore_dir.exists() {
            for entry in WalkDir::new(keystore_dir).max_depth(1).into_iter().filter_map(|e| e.ok()) {
                if entry.file_type().is_file() {
                    if let Some(name) = entry.file_name().to_str() {
                        if !name.starts_with('.') {
                            accounts.push(name.to_string());
                        }
                    }
                }
            }
        }
    }
    Json(KeystoreListResponse { accounts }).into_response()
}

async fn unlock_keystore(
    Json(payload): Json<KeystoreUnlockRequest>,
) -> Response {
    let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).unwrap_or(".".to_string());
    let keystore_path = PathBuf::from(home).join(".foundry").join("keystores").join(&payload.account);

    let output = Command::new("cast")
        .arg("wallet")
        .arg("decrypt-keystore")
        .arg(keystore_path)
        .arg("--unsafe-password")
        .arg(&payload.password)
        .output();

    match output {
        Ok(out) => {
            if out.status.success() {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let private_key = stdout.trim().split_whitespace().last().unwrap_or("").to_string();

                if private_key.starts_with("0x") {
                    Json(KeystoreUnlockResponse { privateKey: private_key }).into_response()
                } else {
                    if let Some(start) = stdout.find("0x") {
                        let pk = &stdout[start..];
                        let pk = pk.split_whitespace().next().unwrap_or("").to_string();
                        Json(KeystoreUnlockResponse { privateKey: pk }).into_response()
                    } else {
                        Json(serde_json::json!({"error": format!("Could not parse private key from output: {}", stdout)})).into_response()
                    }
                }
            } else {
                let stderr = String::from_utf8_lossy(&out.stderr);
                Json(serde_json::json!({"error": format!("Decryption failed: {}", stderr)})).into_response()
            }
        },
        Err(e) => Json(serde_json::json!({"error": format!("Failed to execute cast: {}", e)})).into_response()
    }
}

#[derive(Deserialize)]
struct KeystoreRemoveRequest {
    account: String,
    password: String,
}

async fn remove_keystore(
    Json(payload): Json<KeystoreRemoveRequest>,
) -> Response {
    let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).unwrap_or(".".to_string());
    let keystore_root = PathBuf::from(home).join(".foundry").join("keystores");

    let output = Command::new("cast")
        .arg("wallet")
        .arg("remove")
        .arg("--name")
        .arg(&payload.account)
        .arg("--dir")
        .arg(keystore_root)
        .arg("--unsafe-password")
        .arg(&payload.password)
        .output();

    match output {
        Ok(out) => {
            if out.status.success() {
                Json(serde_json::json!({"status": "success"})).into_response()
            } else {
                let stderr = String::from_utf8_lossy(&out.stderr);
                Json(serde_json::json!({"error": format!("Remove failed: {}", stderr)})).into_response()
            }
        },
        Err(e) => Json(serde_json::json!({"error": format!("Failed to execute cast: {}", e)})).into_response()
    }
}

async fn create_keystore(
    Json(payload): Json<KeystoreCreateRequest>,
) -> Response {
    let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).unwrap_or(".".to_string());
    let keystore_root = PathBuf::from(home).join(".foundry").join("keystores");

    if !keystore_root.exists() {
        let _ = std::fs::create_dir_all(&keystore_root);
    }

    let mut cmd = Command::new("cast");
    cmd.arg("wallet");

    if let Some(ref pk) = payload.privateKey {
        cmd.arg("import")
           .arg(&payload.account)
           .arg("--private-key")
           .arg(pk)
           .arg("--unsafe-password")
           .arg(&payload.password)
           .arg("--keystore-dir")
           .arg(&keystore_root);
    } else {
        let full_path = keystore_root.join(&payload.account);
        cmd.arg("new")
           .arg(full_path)
           .arg("--unsafe-password")
           .arg(&payload.password);
    }

    let output = cmd.output();

    match output {
        Ok(out) => {
            if out.status.success() {
                Json(serde_json::json!({"status": "success", "account": payload.account})).into_response()
            } else {
                let stderr = String::from_utf8_lossy(&out.stderr);
                Json(serde_json::json!({"error": format!("Operation failed: {}", stderr)})).into_response()
            }
        },
        Err(e) => Json(serde_json::json!({"error": format!("Failed to execute cast: {}", e)})).into_response()
    }
}
