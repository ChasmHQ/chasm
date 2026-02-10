#![allow(non_snake_case)]
mod compiler;
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
}

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

#[tokio::main]
async fn main() {
    let args = Cli::parse();
    let root_dir = args.path.canonicalize().unwrap_or(args.path);
    // Initialize logging
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "chainsmith=debug,tower_http=debug".into()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    tracing::info!("Starting ChainSmith...");

    // Channel for broadcasting updates to frontend
    let (tx, _rx) = broadcast::channel(100);
    let last_msg = Arc::new(Mutex::new(None));

    // Start Anvil (Primary)
    let mut anvil = anvil::AnvilNode::new(8545);
    if let Err(e) = anvil.start() {
        tracing::error!("Failed to start anvil: {}", e);
    } else {
        tracing::info!("Anvil started on port 8545");
    }

    // Forked Anvil (Optional)
    let fork_node = Arc::new(Mutex::new(anvil::AnvilNode::new(8546)));
    
    // Initial Compile
    tracing::info!("Performing initial compilation...");
    let compiler = Compiler::new(root_dir.clone()).unwrap();
    match compiler.compile_to_json() {
        Ok(json) => {
            tracing::info!("Initial compilation successful. Payload size: {}", json.len());
            if let Ok(mut lock) = last_msg.lock() {
                *lock = Some(json);
            }
        },
        Err(e) => {
            tracing::error!("Initial compilation failed: {}", e);
            let err_msg = format!("{{\"type\": \"compile_error\", \"error\": \"{}\"}}", e);
            if let Ok(mut lock) = last_msg.lock() {
                *lock = Some(err_msg);
            }
        }
    }

    // Start File Watcher
    let tx_for_watcher = tx.clone();
    let last_msg_for_watcher = last_msg.clone();
    if let Err(e) = watcher::setup_watcher(root_dir.clone(), tx_for_watcher, last_msg_for_watcher).await {
        tracing::error!("Failed to setup watcher: {}", e);
    }

    let app_state = Arc::new(AppState { tx, last_msg, fork_node, root_dir });

    // Build our application with a route
    let app = Router::new()
        .route("/ws", get(ws_handler))
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
        .route("/", get(serve_ui_root))
        .route("/*path", get(serve_ui))
        .layer(CorsLayer::permissive())
        .with_state(app_state);

    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    tracing::info!("ChainSmith UI listening on http://{}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

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

async fn inspect_storage(
    Path(contract): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Response {
    tracing::info!("Inspecting storage for {}", contract);

    let current_dir = state.root_dir.clone();
    let mut file_path = None;

    // Determine source directory
    let contracts_dir = current_dir.join("contracts");
    let src_path = if contracts_dir.exists() {
        contracts_dir.clone()
    } else {
        current_dir.clone()
    };

    for entry in WalkDir::new(&current_dir).into_iter().filter_map(|e| e.ok()) {
        if entry.file_name().to_string_lossy() == format!("{}.sol", contract) {
            file_path = Some(entry.path().to_path_buf());
            break;
        }
    }

    let target = if let Some(path) = file_path {
        format!("{}:{}", path.display(), contract)
    } else {
        contract
    };

    let output = Command::new("forge")
        .arg("inspect")
        .arg(&target)
        .arg("storage")
        .arg("--json")
        .arg("--root")
        .arg(&current_dir)
        .arg("--contracts")
        .arg(&src_path)
        .output();

    match output {
        Ok(out) => {
            if out.status.success() {
                let stdout = String::from_utf8_lossy(&out.stdout);
                Json(serde_json::from_str::<serde_json::Value>(&stdout).unwrap_or(serde_json::json!({
                    "error": "Failed to parse forge output"
                }))).into_response()
            } else {
                let stderr = String::from_utf8_lossy(&out.stderr);
                Json(serde_json::json!({
                    "error": format!("Forge failed: {}", stderr)
                })).into_response()
            }
        },
        Err(e) => {
             Json(serde_json::json!({
                "error": format!("Failed to execute forge: {}", e)
            })).into_response()
        }
    }
}

async fn get_trace(
    Path(tx_hash): Path<String>,
    Query(params): Query<TraceParams>,
    State(state): State<Arc<AppState>>,
) -> Response {
    let rpc_url = params.rpc_url.unwrap_or("http://127.0.0.1:8545".to_string());
    tracing::info!("Tracing tx {} on {}", tx_hash, rpc_url);

    // cast run <tx> --rpc-url <url>
    // cast run outputs colored ansi. We want that to display in frontend.
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
            
            // cast run output is often in stdout, but errors in stderr.
            // We return both.
            Json(serde_json::json!({
                "stdout": stdout,
                "stderr": stderr
            })).into_response()
        },
        Err(e) => {
             Json(serde_json::json!({
                "error": format!("Failed to execute cast: {}", e)
            })).into_response()
        }
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
        Err(e) => {
            Json(serde_json::json!({
                "error": format!("Failed to execute trace call: {}", e)
            })).into_response()
        }
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
        // Contract creation trace
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
        Err(e) => {
            Json(serde_json::json!({
                "error": format!("Failed to execute cast trace: {}", e)
            })).into_response()
        }
    }
}

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
        Err(e) => {
            Json(serde_json::json!({
                "error": format!("Failed to start forked anvil: {}", e)
            })).into_response()
        }
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

async fn list_keystores() -> Response {
    let mut accounts = Vec::new();
    // foundry keystores are in ~/.foundry/keystores
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

    // cast wallet decrypt-keystore <PATH> --unsafe-password <PASS>
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
                // Output format: "... private key is: 0x..."
                // We take the last word.
                let private_key = stdout.trim().split_whitespace().last().unwrap_or("").to_string();
                
                if private_key.starts_with("0x") {
                     Json(KeystoreUnlockResponse { privateKey: private_key }).into_response()
                } else {
                     // Fallback: try to find it in the string if formatting is different
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

    // cast wallet remove --name <NAME> --dir <DIR> --unsafe-password <PASS>
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
        // IMPORT MODE
        // cast wallet import <NAME> --private-key <KEY> --unsafe-password <PASS> --keystore-dir <DIR>
        cmd.arg("import")
           .arg(&payload.account)
           .arg("--private-key")
           .arg(pk)
           .arg("--unsafe-password")
           .arg(&payload.password)
           .arg("--keystore-dir")
           .arg(&keystore_root);
    } else {
        // NEW RANDOM MODE
        // cast wallet new <FULL_PATH> --unsafe-password <PASS>
        let full_path = keystore_root.join(&payload.account);
        cmd.arg("new")
           .arg(full_path)
           .arg("--unsafe-password")
           .arg(&payload.password);
    }

    // No stdin needed anymore
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