//! Native controller for the transaction/call step debugger.
use super::session::DesktopState;
use super::trace_debug::{self, Program, Session};
use super::{MainWindow, TraceData, TraceLine, TraceLogic, TraceOp, TraceTarget, TraceValue};
use serde_json::{json, Value};
use slint::{ComponentHandle, Weak};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

type State = Arc<Mutex<DesktopState>>;

/// Opcodes shown around the cursor in the listing.
const OPS_WINDOW: usize = 400;

pub fn wire(ui: &MainWindow, state: &State, runtime: tokio::runtime::Handle) {
    let weak = ui.as_weak();
    let held = state.clone();
    let rt = runtime.clone();
    ui.global::<TraceLogic>().on_load(move |input| load(weak.clone(), held.clone(), rt.clone(), input.trim().to_string()));
    let weak = ui.as_weak();
    let held = state.clone();
    let rt = runtime.clone();
    ui.global::<TraceLogic>().on_load_recent(move |index| {
        let query = weak.upgrade().and_then(|ui| {
            let targets = ui.global::<TraceData>().get_targets();
            slint::Model::row_data(&targets, index as usize).map(|target| target.query.to_string())
        });
        if let Some(query) = query {
            load(weak.clone(), held.clone(), rt.clone(), query);
        }
    });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<TraceLogic>().on_navigate(move |action| {
        with_session(&weak, &held, |session| {
            match action.as_str() {
                "continue" => session.resume(true),
                "back" => session.resume(false),
                "step" => session.step_line(true),
                "step-back" => session.step_line(false),
                "next" => session.next_line(true),
                "prev-next" => session.next_line(false),
                "finish" => session.finish(true),
                "finish-back" => session.finish(false),
                "op" => session.step_opcode(true),
                "op-back" => session.step_opcode(false),
                _ => false,
            };
        })
    });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<TraceLogic>().on_seek(move |index| with_session(&weak, &held, |session| { session.go(index.max(0) as usize); }));
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<TraceLogic>().on_toggle_breakpoint(move |line| with_session(&weak, &held, |session| session.toggle_breakpoint(line.max(0) as usize)));
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<TraceLogic>().on_next_opcode(move |opcode, forward| {
        let opcode = opcode.trim().to_string();
        let mut missing = false;
        with_session(&weak, &held, |session| missing = !session.next_opcode(&opcode, forward));
        if missing {
            held.lock().unwrap().trace_status = format!("No {} {} this step.", opcode.to_ascii_uppercase(), if forward { "after" } else { "before" });
            super::paint(&weak, &held);
        }
    });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<TraceLogic>().on_console_run(move |line| {
        let line = line.trim().to_string();
        if line.is_empty() || line.len() > 200 {
            return;
        }
        {
            let mut state = held.lock().unwrap();
            let output = match state.trace.as_mut() {
                Some(session) => super::trace_console::run(session, &line),
                None => "No trace loaded. Debug a transaction or call first.".into(),
            };
            let entry = format!("(trace) {line}\n{output}\n");
            state.trace_console.push_str(&entry);
            // Keep the transcript bounded; drop whole lines from the start.
            if state.trace_console.len() > 64 * 1024 {
                let cut = state.trace_console.len() - 48 * 1024;
                let cut = state.trace_console[cut..].find('\n').map(|at| cut + at + 1).unwrap_or(cut);
                state.trace_console.drain(..cut);
            }
            if state.trace_history.last() != Some(&line) {
                state.trace_history.push(line);
                if state.trace_history.len() > 100 {
                    state.trace_history.remove(0);
                }
            }
            state.trace_history_at = state.trace_history.len();
        }
        if let Some(ui) = weak.upgrade() {
            ui.global::<TraceData>().set_console_input("".into());
        }
        super::paint(&weak, &held);
    });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<TraceLogic>().on_console_history(move |direction| {
        let text = {
            let mut state = held.lock().unwrap();
            let len = state.trace_history.len();
            let at = if direction < 0 { state.trace_history_at.saturating_sub(1) } else { (state.trace_history_at + 1).min(len) };
            state.trace_history_at = at;
            state.trace_history.get(at).cloned().unwrap_or_default()
        };
        if let Some(ui) = weak.upgrade() {
            ui.global::<TraceData>().set_console_input(text.into());
        }
    });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<TraceLogic>().on_open_saved(move |name| {
        {
            let mut state = held.lock().unwrap();
            if state.transition_pending() || state.trace_busy {
                return;
            }
            let result = saved_path(&state.workspace, name.as_str()).and_then(|path| {
                let bytes = std::fs::read(&path).map_err(|error| format!("Could not read {}: {error}", path.display()))?;
                let saved: SavedTrace = serde_json::from_slice(&bytes).map_err(|_| format!("{} is not a Chasm trace.", path.display()))?;
                if saved.format != TRACE_FORMAT {
                    return Err(format!("{} uses an unknown trace format.", path.display()));
                }
                Ok((saved.session(programs(&state))?, path))
            });
            match result {
                Ok((session, path)) => {
                    state.trace_status = format!("Opened {}", shown_path(&path));
                    state.trace = Some(session);
                }
                Err(error) => state.trace_status = error,
            }
        }
        if let Some(ui) = weak.upgrade() {
            ui.global::<super::AppData>().set_debugger_mode(0);
        }
        super::paint(&weak, &held);
    });
    for reveal in [true, false] {
        let weak = ui.as_weak();
        let held = state.clone();
        let rt = runtime.clone();
        let action = move |name: slint::SharedString| {
            let path = {
                let state = held.lock().unwrap();
                saved_path(&state.workspace, name.as_str())
            };
            let weak = weak.clone();
            let held = held.clone();
            rt.spawn(async move {
                let result = match path {
                    Ok(path) if reveal => tokio::task::spawn_blocking(move || super::platform::reveal_file(&path)).await.unwrap_or_else(|_| Err("Could not open File Explorer.".into())).map(|_| String::new()),
                    Ok(path) => std::fs::remove_file(&path).map(|_| format!("Deleted {}", shown_path(&path))).map_err(|error| format!("Could not delete {}: {error}", path.display())),
                    Err(error) => Err(error),
                };
                let _ = slint::invoke_from_event_loop(move || {
                    {
                        let mut state = held.lock().unwrap();
                        match result {
                            Ok(message) if message.is_empty() => {}
                            Ok(message) | Err(message) => state.trace_status = message,
                        }
                    }
                    super::paint(&weak, &held);
                });
            });
        };
        if reveal {
            ui.global::<TraceLogic>().on_reveal_saved(action);
        } else {
            ui.global::<TraceLogic>().on_delete_saved(action);
        }
    }
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<TraceLogic>().on_clear(move || {
        {
            let mut state = held.lock().unwrap();
            state.trace = None;
            state.trace_status.clear();
        }
        super::paint(&weak, &held);
    });
}

fn with_session(weak: &Weak<MainWindow>, held: &State, edit: impl FnOnce(&mut Session)) {
    {
        let mut state = held.lock().unwrap();
        let Some(session) = state.trace.as_mut() else { return };
        edit(session);
    }
    super::paint(weak, held);
}

/// What to trace: a mined transaction hash, or (empty input) the newest
/// transaction or simulated call captured by the active contract tab.
enum Target {
    Transaction(String),
    Call { title: String, call: Value, block: String, target: String },
}

fn pick_target(state: &DesktopState, input: &str) -> Result<Target, String> {
    if !input.is_empty() {
        let hash = input.to_ascii_lowercase();
        if hash.len() != 66 || !hash.starts_with("0x") || !hash[2..].bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("Enter a 32-byte 0x transaction hash.".into());
        }
        return Ok(Target::Transaction(hash));
    }
    if let Some(view) = state.current_contract_view() {
        if let (Some(call), Some(target)) = (&view.reverted_call, &view.target) {
            return Ok(Target::Call {
                title: format!("Reverted call {}", target.contract),
                call: call.clone(),
                block: "latest".into(),
                target: target.address.clone(),
            });
        }
    }
    let result = state.current_contract_view().and_then(|view| view.result.as_ref());
    if let Some(result) = result {
        if let Some(hash) = result.context.request["transactionHash"].as_str() {
            return Ok(Target::Transaction(hash.to_string()));
        }
        if result.context.request["method"] == "eth_call" {
            let params = &result.context.request["params"];
            return Ok(Target::Call {
                title: format!("Call {}.{}", result.context.contract, result.context.signature),
                call: params[0].clone(),
                block: params[1].as_str().unwrap_or("latest").to_string(),
                target: result.context.address.clone(),
            });
        }
    }
    if !state.last_tx.is_empty() {
        return Ok(Target::Transaction(state.last_tx.clone()));
    }
    Err("Nothing to debug yet. Send or simulate a call, or paste a transaction hash.".into())
}

fn programs(state: &DesktopState) -> Vec<Program> {
    state
        .runtimes
        .iter()
        .filter(|(_, runtime)| !runtime.code.is_empty() && !runtime.source_path.is_empty())
        .filter_map(|(name, runtime)| {
            let path = std::path::Path::new(&runtime.source_path);
            let path = if path.is_absolute() { path.to_path_buf() } else { state.workspace.join(path) };
            // Foundry hands solc the source with LF line endings, so source map
            // offsets drift by one byte per line on a CRLF checkout.
            let source = std::fs::read_to_string(&path).ok()?.replace("\r\n", "\n");
            let abi = state.contracts.iter().find(|contract| &contract.name == name).map(|contract| contract.abi.clone());
            Some(Program { name: name.clone(), runtime: runtime.code.clone(), source_map: runtime.source_map.clone(), source_path: runtime.source_path.clone(), source, source_id: runtime.source_id, layout: state.storage_layouts.get(name).cloned(), abi })
        })
        .collect()
}

pub fn load(weak: Weak<MainWindow>, held: State, runtime: tokio::runtime::Handle, input: String) {
    let prepared = {
        let mut state = held.lock().unwrap();
        if state.transition_pending() || !state.workspace_ready || state.trace_busy {
            return;
        }
        match pick_target(&state, &input) {
            Ok(target) => {
                // Remembered so a re-fork can debug the same transaction again.
                state.trace_input = match &target {
                    Target::Transaction(hash) => hash.clone(),
                    Target::Call { .. } => String::new(),
                };
                state.trace_busy = true;
                state.trace_status = "Recording the execution…".into();
                Some((target, state.rpc.clone(), programs(&state), state.workspace_epoch, state.network.fork_url.clone()))
            }
            Err(error) => {
                state.trace_status = error;
                None
            }
        }
    };
    super::paint(&weak, &held);
    let Some((target, rpc, programs, epoch, fork_url)) = prepared else { return };
    let workspace = held.lock().unwrap().workspace.clone();
    runtime.spawn(async move {
        let result = record(&rpc, target, programs, &fork_url).await.map(|(session, saved, hash, note)| {
            let path = save_trace(&workspace, &saved, hash.as_deref());
            (session, path, note)
        });
        let _ = slint::invoke_from_event_loop(move || {
            let mut state = held.lock().unwrap();
            if !state.active || state.workspace_epoch != epoch {
                return;
            }
            state.trace_busy = false;
            match result {
                Ok((session, path, note)) => {
                    state.trace_status = match path {
                        Ok(path) => format!("Saved to {}", shown_path(&path)),
                        Err(error) => format!("Not saved: {error}"),
                    };
                    if !note.is_empty() {
                        state.trace_status = format!("{note} {}", state.trace_status);
                    }
                    state.trace = Some(session);
                    state.open_activity(4);
                }
                Err(error) => state.trace_status = error,
            }
            drop(state);
            super::panels::focus_current();
            super::paint(&weak, &held);
        });
    });
}

/// A recorded execution as saved under `.chasm/traces/`: the node's raw
/// struct-logger trace and the code at each address it entered, so the
/// session rebuilds without the node. Source is read again from the workspace.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct SavedTrace {
    pub format: String,
    pub title: String,
    pub target: String,
    pub block: String,
    pub trace: Value,
    /// address -> 0x runtime code
    pub code: HashMap<String, String>,
    /// 0x calldata of the outermost call.
    #[serde(default)]
    pub input: String,
}

pub const TRACE_FORMAT: &str = "chasm-trace/1";

impl SavedTrace {
    fn session(&self, programs: Vec<Program>) -> Result<Session, String> {
        let code = self
            .code
            .iter()
            .filter_map(|(address, code)| Some((address.clone(), ethers::utils::hex::decode(code.trim_start_matches("0x")).ok()?)))
            .collect();
        let mut session = trace_debug::from_trace(self.title.clone(), &self.target, &self.trace, programs, &code)?;
        session.calldata = ethers::utils::hex::decode(self.input.trim_start_matches("0x")).unwrap_or_default();
        Ok(session)
    }
}

/// Path as the user knows it, without the Windows `\\?\` verbatim prefix.
fn shown_path(path: &std::path::Path) -> String {
    path.display().to_string().trim_start_matches(r"\\?\").to_string()
}

/// Folder for saved traces in the project.
pub fn traces_dir(workspace: &std::path::Path) -> std::path::PathBuf {
    workspace.join(".chasm").join("traces")
}

/// File name for a recording: the transaction hash, or a timestamped call.
fn trace_file_name(saved: &SavedTrace, hash: Option<&str>) -> String {
    match hash {
        Some(hash) => format!("{hash}.json"),
        None => {
            let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|time| time.as_secs()).unwrap_or(0);
            let label: String = saved.title.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
            let label = label.split('-').filter(|part| !part.is_empty()).collect::<Vec<_>>().join("-");
            format!("{stamp}-{}.json", &label[..label.len().min(48)])
        }
    }
}

fn save_trace(workspace: &std::path::Path, saved: &SavedTrace, hash: Option<&str>) -> Result<std::path::PathBuf, String> {
    let dir = traces_dir(workspace);
    std::fs::create_dir_all(&dir).map_err(|error| format!("Could not create {}: {error}", dir.display()))?;
    let path = dir.join(trace_file_name(saved, hash));
    let bytes = serde_json::to_vec(saved).map_err(|error| error.to_string())?;
    std::fs::write(&path, bytes).map_err(|error| format!("Could not write {}: {error}", path.display()))?;
    Ok(path)
}

/// Saved traces, newest first: (file name, title line, detail line).
pub fn saved_traces(workspace: &std::path::Path) -> Vec<(String, String, String)> {
    let Ok(entries) = std::fs::read_dir(traces_dir(workspace)) else { return Vec::new() };
    let mut items: Vec<(std::time::SystemTime, String, u64)> = entries
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            Some((meta.modified().ok()?, entry.file_name().to_string_lossy().to_string(), meta.len()))
        })
        .collect();
    items.sort_by(|a, b| b.0.cmp(&a.0));
    items
        .into_iter()
        .take(100)
        .map(|(modified, name, size)| {
            let stem = name.trim_end_matches(".json");
            // Transaction files are named by hash; calls by time and title.
            let label = if stem.len() == 66 && stem.starts_with("0x") {
                format!("Transaction {}…{}", &stem[..10], &stem[stem.len() - 6..])
            } else {
                let title = stem.split_once('-').map(|(_, rest)| rest).unwrap_or(stem).replace('-', " ");
                let mut chars = title.chars();
                chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
            };
            (name, label, format!("{}  ·  {} KiB", age(modified), size.div_ceil(1024)))
        })
        .collect()
}

fn age(time: std::time::SystemTime) -> String {
    let seconds = time.elapsed().map(|elapsed| elapsed.as_secs()).unwrap_or(0);
    match seconds {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", seconds / 60),
        3600..=86_399 => format!("{} h ago", seconds / 3600),
        _ => format!("{} days ago", seconds / 86_400),
    }
}

/// A saved trace's file name must be a plain name inside `.chasm/traces`.
fn saved_path(workspace: &std::path::Path, name: &str) -> Result<std::path::PathBuf, String> {
    if name.is_empty() || name.contains(['/', '\\', ':']) || name.starts_with('.') || !name.ends_with(".json") {
        return Err("Choose a saved trace from the list.".into());
    }
    Ok(traces_dir(workspace).join(name))
}

/// Records a trace. Returns the session, its saved form, the transaction
/// hash (for the file name) and a note to show when the replay is approximate.
async fn record(rpc: &str, target: Target, programs: Vec<Program>, fork_url: &str) -> Result<(Session, SavedTrace, Option<String>, String), String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .no_proxy()
        .build()
        .map_err(|error| error.to_string())?;
    let call = |method: &'static str, params: Value| {
        let client = client.clone();
        async move {
            let value: Value = client
                .post(rpc)
                .json(&json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}))
                .send()
                .await
                .map_err(|error| format!("Could not reach the node: {error}"))?
                .json()
                .await
                .map_err(|error| format!("The node returned invalid JSON: {error}"))?;
            if let Some(error) = value.get("error") {
                return Err(format!("The node refused {method}: {}", error["message"].as_str().unwrap_or("unknown error")));
            }
            Ok::<Value, String>(value["result"].clone())
        }
    };
    let hash = match &target {
        Target::Transaction(hash) => Some(hash.clone()),
        Target::Call { .. } => None,
    };
    let input;
    let mut replay_note = String::new();
    // A forked-chain transaction runs on a replay node; code is read there too.
    let mut replay_node: Option<crate::anvil::AnvilNode> = None;
    let mut code_rpc = rpc.to_string();
    let mut code_block = String::new();
    // The mined block of a forked-chain transaction, read from the remote chain.
    let fork_block = |tx: &Value| {
        let number = tx["blockNumber"].clone();
        let url = fork_url.to_string();
        async move {
            let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(30)).build().map_err(|error| error.to_string())?;
            let value: Value = client.post(&url).json(&json!({"jsonrpc":"2.0","id":1,"method":"eth_getBlockByNumber","params":[number, false]})).send().await.map_err(|error| error.to_string())?.json().await.map_err(|error| error.to_string())?;
            Ok::<Value, String>(value["result"].clone())
        }
    };
    let (title, trace, target_address, block) = match target {
        Target::Transaction(hash) => {
            let tx = call("eth_getTransactionByHash", json!([hash])).await?;
            if tx.is_null() {
                return Err("This node has no transaction with that hash. A chain revert or a node restart removes later transactions; send it again to debug it.".into());
            }
            let receipt = call("eth_getTransactionReceipt", json!([hash])).await?;
            let to = tx["to"].as_str().map(str::to_string).or_else(|| receipt["contractAddress"].as_str().map(str::to_string)).unwrap_or_default();
            let block = tx["blockNumber"].as_str().unwrap_or("latest").to_string();
            let trace = match call("debug_traceTransaction", json!([hash, trace_debug::trace_options()])).await {
                Ok(trace) => trace,
                // A transaction from the forked chain, not mined on this node:
                // replay it as a call on a fork of its parent block.
                Err(error) if !fork_url.is_empty() && (error.contains("does not exist") || error.contains("not available") || error.contains("not found")) => {
                    let number = u64::from_str_radix(block.trim_start_matches("0x"), 16).unwrap_or(0);
                    if number == 0 {
                        return Err("This transaction has no block number yet; debug it once it is mined.".into());
                    }
                    let (node, url) = start_replay_node(fork_url, number - 1).await?;
                    replay_node = Some(node);
                    code_rpc = url;
                    code_block = "latest".into();
                    // Run it in its own block's context: contracts read block.timestamp,
                    // block.number, basefee and coinbase, and the parent's values send them
                    // down another path.
                    let mined = fork_block(&tx).await.unwrap_or(Value::Null);
                    let mut request = json!({"from": tx["from"], "to": tx["to"], "data": tx["input"], "value": tx["value"], "gas": tx["gas"]});
                    for key in ["maxFeePerGas", "maxPriorityFeePerGas", "gasPrice", "accessList"] {
                        if !tx[key].is_null() {
                            request[key] = tx[key].clone();
                        }
                    }
                    let mut options = trace_debug::trace_options();
                    if mined.is_object() {
                        options["blockOverrides"] = json!({"number": mined["number"], "time": mined["timestamp"], "coinbase": mined["miner"], "baseFee": mined["baseFeePerGas"], "gasLimit": mined["gasLimit"], "random": mined["mixHash"]});
                    }
                    let position = tx["transactionIndex"].as_str().and_then(|text| u64::from_str_radix(text.trim_start_matches("0x"), 16).ok()).unwrap_or(0);
                    let trace = rpc_call(&client, &code_rpc, "debug_traceCall", json!([request, "latest", options])).await?;
                    if position > 0 {
                        replay_note = format!("Replayed on the state before block {number}. It was transaction {} of that block, so the {position} before it did not run first; storage they changed may differ.", position + 1);
                    } else {
                        replay_note = format!("Replayed on a fork of block {} with the chain's own id.", number - 1);
                    }
                    trace
                }
                Err(error) => return Err(error),
            };
            input = tx["input"].as_str().unwrap_or("").to_string();
            let short = format!("{}…{}", &hash[..10], &hash[hash.len() - 6..]);
            (format!("Transaction {short}"), trace, to, block)
        }
        Target::Call { title, call: request, block, target } => {
            input = request["data"].as_str().unwrap_or("").to_string();
            let trace = call("debug_traceCall", json!([request, block, trace_debug::trace_options()])).await?;
            (title, trace, target, block)
        }
    };
    if trace["structLogs"].as_array().is_some_and(Vec::is_empty) && trace["gas"].as_u64().unwrap_or(0) > 21_000 {
        return Err("The node did not record opcode steps for this transaction. It was mined before step recording was enabled; restart the project and send it again.".into());
    }
    // Code at every address the trace may enter, for source mapping.
    let mut addresses = vec![target_address.to_ascii_lowercase()];
    for log in trace["structLogs"].as_array().into_iter().flatten() {
        if matches!(log["op"].as_str(), Some("CALL" | "STATICCALL" | "DELEGATECALL" | "CALLCODE")) {
            if let Some(word) = log["stack"].as_array().and_then(|stack| stack.get(stack.len().wrapping_sub(2))).and_then(Value::as_str) {
                let digits = format!("{:0>40}", word.trim_start_matches("0x"));
                let address = format!("0x{}", &digits[digits.len() - 40..]).to_ascii_lowercase();
                if !addresses.contains(&address) && addresses.len() < 64 {
                    addresses.push(address);
                }
            }
        }
    }
    let mut code_at = HashMap::new();
    let code_block = if code_block.is_empty() { block.clone() } else { code_block };
    for address in addresses.into_iter().filter(|address| address.len() == 42) {
        if let Ok(code) = rpc_call(&client, &code_rpc, "eth_getCode", json!([address, code_block])).await {
            if let Some(bytes) = code.as_str().and_then(|text| ethers::utils::hex::decode(text.trim_start_matches("0x")).ok()) {
                code_at.insert(address, bytes);
            }
        }
    }
    let saved = SavedTrace {
        format: TRACE_FORMAT.into(),
        title,
        target: target_address,
        block,
        trace,
        code: code_at.iter().map(|(address, code)| (address.clone(), format!("0x{}", ethers::utils::hex::encode(code)))).collect(),
        input,
    };
    drop(replay_node);
    let session = saved.session(programs)?;
    Ok((session, saved, hash, replay_note))
}

async fn rpc_call(client: &reqwest::Client, url: &str, method: &str, params: Value) -> Result<Value, String> {
    let value: Value = client
        .post(url)
        .json(&json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}))
        .send()
        .await
        .map_err(|error| format!("Could not reach the node: {error}"))?
        .json()
        .await
        .map_err(|error| format!("The node returned invalid JSON: {error}"))?;
    if let Some(error) = value.get("error") {
        return Err(format!("The node refused {method}: {}", error["message"].as_str().unwrap_or("unknown error")));
    }
    Ok(value["result"].clone())
}

/// Starts a replay fork at `block` on a free local port and waits until it
/// answers. The node stops when the returned handle is dropped.
async fn start_replay_node(fork_url: &str, block: u64) -> Result<(crate::anvil::AnvilNode, String), String> {
    let port = std::net::TcpListener::bind("127.0.0.1:0").and_then(|listener| listener.local_addr()).map_err(|error| error.to_string())?.port();
    let mut node = crate::anvil::AnvilNode::new_silent(port);
    node.start_replay(fork_url, block).map_err(|error| format!("Could not start a replay fork: {error}"))?;
    let url = format!("http://127.0.0.1:{port}");
    let probe = reqwest::Client::builder().timeout(std::time::Duration::from_millis(800)).no_proxy().build().map_err(|error| error.to_string())?;
    for _ in 0..600 {
        node.check_process().map_err(|error| format!("The replay fork stopped: {error}"))?;
        if rpc_call(&probe, &url, "eth_chainId", json!([])).await.is_ok() {
            return Ok((node, url));
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    Err(format!("The replay fork of block {block} did not start within a minute. Check the fork RPC."))
}

fn recent_targets(state: &DesktopState) -> Vec<TraceTarget> {
    state
        .recent_transactions
        .iter()
        .take(30)
        .map(|row| TraceTarget { label: row.title.clone().into(), detail: row.detail.clone().into(), query: row.query.clone().into() })
        .collect()
}

pub fn render(ui: &MainWindow, state: &DesktopState) {
    let data = ui.global::<TraceData>();
    data.set_busy(state.trace_busy);
    data.set_status(state.trace_status.clone().into());
    if data.get_console_text().as_str() != state.trace_console {
        data.set_console_text(state.trace_console.clone().into());
        data.set_console_lines(super::model(state.trace_console.lines().map(Into::into).collect()));
    }
    data.set_targets(super::model(recent_targets(state)));
    data.set_saved(super::model(
        saved_traces(&state.workspace)
            .into_iter()
            .map(|(name, label, detail)| TraceTarget { label: label.into(), detail: detail.into(), query: name.into() })
            .collect(),
    ));
    data.set_saved_folder(shown_path(&traces_dir(&state.workspace)).into());
    let Some(session) = state.trace.as_ref().filter(|session| !session.is_empty()) else {
        data.set_loaded(false);
        return;
    };
    data.set_loaded(true);
    data.set_title(session.title.clone().into());
    data.set_total(session.steps.len() as i32);
    data.set_cursor(session.cursor as i32);
    let step = session.current().cloned().unwrap_or_else(|| session.steps[0].clone());
    let location = session.location(session.cursor);
    data.set_location(match &location {
        Some(location) => format!("{}:{}  pc {}  {}", location.file, location.line, step.pc, step.op),
        None => format!("pc {}  {}  (no source for {})", step.pc, step.op, short(&step.address)),
    }.into());
    data.set_summary(format!(
        "{}  gas used {}  {} steps{}{}",
        if session.failed { "REVERTED" } else { "success" },
        session.gas_used,
        session.steps.len(),
        if session.truncated { " (cut at the step limit)" } else { "" },
        if state.trace_status.is_empty() { String::new() } else { format!("  {}", state.trace_status) },
    ).into());

    // Source listing with per-line hit counts.
    match session.source_lines(session.cursor) {
        Some((file, lines)) => {
            let current = location.as_ref().map(|location| location.line).unwrap_or(0);
            data.set_file(file.into());
            data.set_source_current(current as i32 - 1);
            data.set_source(super::model(
                lines
                    .iter()
                    .enumerate()
                    .map(|(index, text)| TraceLine {
                        number: index as i32 + 1,
                        text: text.replace('\t', "    ").into(),
                        current: index + 1 == current,
                        breakpoint: session.breakpoints.contains(&(index + 1)),
                        hits: session.line_hits(&step.address, index + 1),
                    })
                    .collect(),
            ));
        }
        None => {
            data.set_file("".into());
            data.set_source_current(-1);
            data.set_source(super::model(Vec::new()));
        }
    }

    // Opcode window around the cursor.
    let start = session.cursor.saturating_sub(OPS_WINDOW / 2);
    let end = (start + OPS_WINDOW).min(session.steps.len());
    data.set_ops(super::model(
        (start..end)
            .map(|index| {
                let item = &session.steps[index];
                TraceOp { index: index as i32, pc: item.pc.to_string().into(), op: item.op.clone().into(), gas: item.gas.to_string().into(), depth: item.depth as i32, current: index == session.cursor }
            })
            .collect(),
    ));
    data.set_ops_current((session.cursor - start) as i32);

    let labels = super::address_book::labels(state);
    // Machine state, with values changed by the previous step highlighted.
    let previous = session.cursor.checked_sub(1).and_then(|index| session.steps.get(index));
    let stack_len = step.stack.len();
    data.set_stack(super::model(
        step.stack
            .iter()
            .rev()
            .enumerate()
            .map(|(depth, word)| {
                // Highlight only words this step pushed or rewrote.
                let position = stack_len - 1 - depth;
                let changed = previous.is_some_and(|prev| prev.stack.get(position) != Some(word));
                let value = match super::address_book::word_label(word, &labels) {
                    Some(label) => format!("{}  ({label})", trace_debug::describe_word(word)),
                    None => trace_debug::describe_word(word),
                };
                TraceValue { label: if depth == 0 { "top".into() } else { format!("[{depth}]").into() }, value: value.into(), changed }
            })
            .collect(),
    ));
    data.set_memory(super::model(
        step.memory
            .iter()
            .enumerate()
            .map(|(index, word)| TraceValue {
                label: format!("{:#06x}", index * 32).into(),
                value: format!("{}  {}", word, printable(word)).into(),
                changed: previous.is_some_and(|prev| prev.memory.get(index) != Some(word)),
            })
            .collect(),
    ));
    data.set_storage(super::model(
        step.storage
            .iter()
            .map(|(slot, value)| TraceValue {
                label: short_word(slot).into(),
                value: format!(
                    "slot 0x{}\n{}{}",
                    slot.trim_start_matches("0x"),
                    trace_debug::describe_word(&compact_word(value)),
                    super::address_book::word_label(value, &labels).map(|label| format!("  ({label})")).unwrap_or_default()
                )
                .into(),
                changed: previous.is_some_and(|prev| !prev.storage.iter().any(|(old_slot, old)| old_slot == slot && old == value)),
            })
            .collect(),
    ));
    data.set_variables(super::model(variables(session, &labels)));
    data.set_frames(super::model(
        session
            .frames()
            .into_iter()
            .map(|(depth, address, name)| TraceValue { label: format!("depth {depth}").into(), value: match labels.get(&address) { Some(label) if label != &name => format!("{name}  {address} ({label})"), _ => format!("{name}  {address}") }.into(), changed: false })
            .collect(),
    ));
    data.set_step_info(super::model(vec![
        TraceValue { label: "opcode".into(), value: format!("{}  at pc {}", step.op, step.pc).into(), changed: false },
        TraceValue { label: "gas".into(), value: format!("{} left, costs {}", step.gas, step.gas_cost).into(), changed: false },
        TraceValue { label: "depth".into(), value: step.depth.to_string().into(), changed: false },
        TraceValue { label: "code".into(), value: format!("{}  {}", session.mapped.get(&step.address).map(|program| session.programs[*program].name.as_str()).unwrap_or("not a workspace contract"), step.address).into(), changed: false },
        TraceValue { label: "return".into(), value: if session.return_value.is_empty() { "none".into() } else { session.return_value.clone().into() }, changed: false },
    ]));
}

/// Arguments of the outermost call, then every storage write up to the cursor
/// by variable name with its old and new value. Writes after the cursor are
/// listed dimmed as "later", so the whole effect of the call is visible.
fn variables(session: &Session, labels: &HashMap<String, String>) -> Vec<TraceValue> {
    let mut rows = Vec::new();
    if let Some((function, args)) = session.call_arguments() {
        rows.push(TraceValue { label: "call".into(), value: function.into(), changed: false });
        for (name, value) in args {
            let value = super::address_book::annotate(&value, labels);
            rows.push(TraceValue { label: name.into(), value: value.into(), changed: false });
        }
    }
    let mut layouts: HashMap<String, Option<super::storage_layout::Layout>> = HashMap::new();
    for write in &session.writes {
        let layout = layouts.entry(write.address.clone()).or_insert_with(|| session.layout_at(&write.address));
        let named = layout.as_ref().and_then(|layout| layout.name_slot(write.slot, &session.preimages));
        let later = write.step > session.cursor;
        let fields = named.unwrap_or_else(|| vec![super::storage_layout::SlotField { path: format!("slot {:#x}", write.slot), type_label: "bytes32".into(), offset: 0, size: 32 }]);
        for field in fields {
            let before = super::storage_layout::scalar(&field.type_label, write.before, field.offset, field.size);
            let after = super::storage_layout::scalar(&field.type_label, write.after, field.offset, field.size);
            if before == after && field.size < 32 {
                continue; // another variable packed in the same slot changed
            }
            let mut value = format!("{before} → {after}");
            // Wei amounts are easier to read in ether.
            if field.type_label.starts_with("uint") {
                if let Some(ether) = as_ether(&after) {
                    value.push_str(&format!("  ({ether} ether)"));
                }
            }
            if let Some(label) = labels.get(&after.to_ascii_lowercase()) {
                value.push_str(&format!("  ({label})"));
            }
            if later {
                value.push_str("  (later, step ");
                value.push_str(&(write.step + 1).to_string());
                value.push(')');
            }
            rows.push(TraceValue { label: super::address_book::annotate(&field.path, labels).into(), value: value.into(), changed: !later && write.step + 1 >= session.cursor && write.step <= session.cursor });
        }
    }
    rows
}

/// `value` in ether when it is at least 0.001 ether, with trailing zeros trimmed.
fn as_ether(value: &str) -> Option<String> {
    let wei = ethers::types::U256::from_dec_str(value).ok()?;
    if wei < ethers::types::U256::exp10(15) {
        return None;
    }
    let unit = ethers::types::U256::exp10(18);
    let fraction = format!("{:018}", (wei % unit).as_u64());
    let fraction = fraction.trim_end_matches('0');
    Some(if fraction.is_empty() { (wei / unit).to_string() } else { format!("{}.{fraction}", wei / unit) })
}

fn compact_word(word: &str) -> String {
    let digits = word.trim_start_matches("0x").trim_start_matches('0');
    format!("0x{}", if digits.is_empty() { "0" } else { digits })
}

fn short(address: &str) -> String {
    if address.len() > 12 { format!("{}…{}", &address[..6], &address[address.len() - 4..]) } else { address.to_string() }
}

fn short_word(word: &str) -> String {
    let digits = word.trim_start_matches("0x").trim_start_matches('0');
    if digits.len() <= 8 { format!("0x{}", if digits.is_empty() { "0" } else { digits }) } else { format!("0x{}…", &digits[..6]) }
}

/// ASCII view of a 32-byte memory word, dots for non-printable bytes.
fn printable(word: &str) -> String {
    ethers::utils::hex::decode(word.trim_start_matches("0x"))
        .map(|bytes| bytes.iter().map(|byte| if byte.is_ascii_graphic() || *byte == b' ' { *byte as char } else { '.' }).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_traces_round_trip_and_reject_paths_outside_the_folder() {
        let workspace = tempfile::tempdir().unwrap();
        let logs = vec![json!({"pc":0,"op":"STOP","gas":1,"gasCost":0,"depth":1,"stack":[],"memory":[]})];
        let saved = SavedTrace {
            format: TRACE_FORMAT.into(),
            title: "Call Bank.withdraw(uint256)".into(),
            target: "0x00000000000000000000000000000000000000aa".into(),
            block: "latest".into(),
            trace: json!({"failed":true,"gas":21000,"returnValue":"","structLogs":logs}),
            code: HashMap::from([("0x00000000000000000000000000000000000000aa".to_string(), "0x00".to_string())]),
            input: String::new(),
        };
        let hash = format!("0x{}", "ab".repeat(32));
        let path = save_trace(workspace.path(), &saved, Some(&hash)).unwrap();
        assert_eq!(path, traces_dir(workspace.path()).join(format!("{hash}.json")));
        let call = save_trace(workspace.path(), &saved, None).unwrap();
        assert!(call.file_name().unwrap().to_string_lossy().ends_with("-call-bank-withdraw-uint256.json"));
        let listed = saved_traces(workspace.path());
        assert_eq!(listed.len(), 2);
        let reopened: SavedTrace = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let session = reopened.session(Vec::new()).unwrap();
        assert!(session.failed);
        assert_eq!(session.steps.len(), 1);
        for bad in ["../x.json", "a/b.json", "C:x.json", ".hidden.json", "x.txt", ""] {
            assert!(saved_path(workspace.path(), bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn crlf_sources_use_the_offsets_solc_saw() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("A.sol"), "line one\r\nline two\r\n").unwrap();
        let mut state = DesktopState::new(workspace.path().to_path_buf(), 1.0);
        state.runtimes.insert("A".into(), super::super::session::RuntimeInfo { code: vec![0x00], source_map: "9:8:0".into(), source_path: "A.sol".into(), source_id: Some(0) });
        let programs = programs(&state);
        assert_eq!(programs.len(), 1);
        // Offset 9 is the start of line two in the LF text solc compiled.
        assert_eq!(&programs[0].source[9..17], "line two");
    }
}
