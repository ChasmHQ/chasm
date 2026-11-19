use super::debug_session::{Acceptance, DebugSession, Envelope, Phase, Ticket};
use super::session::{self, DesktopState};
use super::{CodeRow, DebugData, DebugLogic, ExecStat, Logic, MainWindow};
use serde_json::{json, Value};
use slint::{ComponentHandle, Model, ModelRc, VecModel, Weak};
use std::sync::{Arc, Mutex};
use std::time::Duration;

type State = Arc<Mutex<DesktopState>>;

const MAX_DRAFT_BYTES: usize = 4096;
const DRAFT_FIELDS: [&str; 6] = [
    "run-to",
    "evaluate-input",
    "memory-offset",
    "storage-slots",
    "source-start",
    "around-pc",
];

/// Updates one debugger input shared by all pane hosts.
pub fn edit_draft(state: &mut DesktopState, field: &str, value: &str) -> bool {
    if state.transition_pending()
        || !state.workspace_ready
        || !DRAFT_FIELDS.contains(&field)
        || value.len() > MAX_DRAFT_BYTES
    {
        return false;
    }
    state
        .debug_drafts
        .insert(field.to_string(), value.to_string());
    true
}

fn draft<'a>(state: &'a DesktopState, field: &str) -> &'a str {
    state
        .debug_drafts
        .get(field)
        .map(String::as_str)
        .unwrap_or("")
}

#[derive(Clone)]
struct Client {
    http: reqwest::Client,
    csrf: String,
    workspace_epoch: u64,
}

pub fn wire(ui: &MainWindow, state: &State, runtime: tokio::runtime::Handle) {
    let weak = ui.as_weak();
    let held = state.clone();
    let rt = runtime.clone();
    ui.global::<DebugLogic>()
        .on_start(move || start(weak.clone(), held.clone(), rt.clone()));
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<DebugLogic>().on_launch_edited(move || {
        let Some(ui) = weak.upgrade() else { return };
        let data = ui.global::<DebugData>();
        let mut state = held.lock().unwrap();
        if state.transition_pending() || !state.debug.can_start() {
            return;
        }
        state.debug_launch_mode = data.get_launch_mode().clamp(0, 2);
        state.debug_foundry_target = data.get_foundry_target().to_string();
        state.debug_foundry_match = data.get_foundry_match().to_string();
        state.debug_foundry_trusted = data.get_foundry_trusted();
        state.debug_capture_id = data.get_capture_id().to_string();
        state.debug_build_info_id = data.get_build_info_id().to_string();
        state.debug_prepared_launch = None;
        state.debug_launch_summary = launch_request(&state)
            .and_then(|request| request.prepare().map(|_| launch_summary(&state)))
            .unwrap_or_else(|error| error);
        drop(state);
        super::paint(&weak, &held);
    });
    let weak = ui.as_weak();
    let held = state.clone();
    let rt = runtime.clone();
    ui.global::<Logic>()
        .on_start_lab(move || start(weak.clone(), held.clone(), rt.clone()));
    let weak = ui.as_weak();
    let held = state.clone();
    let rt = runtime.clone();
    ui.global::<DebugLogic>().on_navigate(move |action| {
        command(
            weak.clone(),
            held.clone(),
            rt.clone(),
            "navigate",
            json!({"action":action.as_str()}),
        )
    });
    let weak = ui.as_weak();
    let held = state.clone();
    let rt = runtime.clone();
    ui.global::<Logic>().on_debug_continue(move || {
        command(
            weak.clone(),
            held.clone(),
            rt.clone(),
            "navigate",
            json!({"action":"continue"}),
        )
    });
    let weak = ui.as_weak();
    let held = state.clone();
    let rt = runtime.clone();
    ui.global::<DebugLogic>()
        .on_cancel(move || cancel(weak.clone(), held.clone(), rt.clone()));
    let weak = ui.as_weak();
    let held = state.clone();
    let rt = runtime.clone();
    ui.global::<Logic>()
        .on_debug_cancel(move || cancel(weak.clone(), held.clone(), rt.clone()));
    let weak = ui.as_weak();
    let held = state.clone();
    let rt = runtime.clone();
    ui.global::<DebugLogic>()
        .on_refresh(move || command(weak.clone(), held.clone(), rt.clone(), "refresh", json!({})));
    let weak = ui.as_weak();
    let held = state.clone();
    let rt = runtime.clone();
    ui.global::<DebugLogic>().on_breakpoint_here(move || {
        let location = held
            .lock()
            .unwrap()
            .debug
            .snapshot
            .as_ref()
            .and_then(|snapshot| breakpoint_location(&snapshot.status));
        if let Some(location) = location {
            command(
                weak.clone(),
                held.clone(),
                rt.clone(),
                "breakpoint",
                json!({"action":"add","location":location}),
            );
        }
    });
    let weak = ui.as_weak();
    let held = state.clone();
    let rt = runtime.clone();
    ui.global::<DebugLogic>().on_run_to(move || {
        if let Some(ui) = weak.upgrade() {
            let location = ui.global::<DebugData>().get_run_to().to_string();
            command(
                weak.clone(),
                held.clone(),
                rt.clone(),
                "navigate",
                json!({"action":"run_to","location":location}),
            );
        }
    });
    let weak = ui.as_weak();
    let held = state.clone();
    let rt = runtime.clone();
    ui.global::<DebugLogic>().on_save_capture(move || save_capture(weak.clone(), held.clone(), rt.clone()));
    let weak = ui.as_weak();
    let held = state.clone();
    let rt = runtime.clone();
    ui.global::<DebugLogic>().on_open_capture(move |name| open_capture(weak.clone(), held.clone(), rt.clone(), name.to_string()));
    let weak = ui.as_weak();
    let held = state.clone();
    let rt = runtime.clone();
    ui.global::<DebugLogic>().on_evaluate(move || {
        if let Some(ui) = weak.upgrade() {
            let expression = ui.global::<DebugData>().get_evaluate_input().to_string();
            command(
                weak.clone(),
                held.clone(),
                rt.clone(),
                "evaluate",
                json!({"expression":expression,"keep":false}),
            );
        }
    });
    let weak = ui.as_weak();
    let held = state.clone();
    let rt = runtime.clone();
    ui.global::<Logic>().on_read_fee(move || {
        command(
            weak.clone(),
            held.clone(),
            rt.clone(),
            "evaluate",
            json!({"expression":"feeBps","keep":false}),
        )
    });
    let weak = ui.as_weak();
    let held = state.clone();
    let rt = runtime.clone();
    ui.global::<DebugLogic>().on_read_window(move || {
        let Some(ui) = weak.upgrade() else { return };
        let data = ui.global::<DebugData>();
        let parsed = window_params(
            data.get_memory_offset().as_str(),
            data.get_storage_slots().as_str(),
            data.get_source_start().as_str(),
            data.get_around_pc().as_str(),
        );
        match parsed {
            Ok(params) => command(weak.clone(), held.clone(), rt.clone(), "snapshot", params),
            Err(error) => {
                let mut state = held.lock().unwrap();
                if state.transition_pending() || !state.workspace_ready {
                    return;
                }
                state.debug.notice = error;
                drop(state);
                super::paint(&weak, &held);
            }
        }
    });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<DebugLogic>()
        .on_draft_edited(move |field, value| {
            edit_draft(&mut held.lock().unwrap(), field.as_str(), value.as_str());
            super::paint(&weak, &held);
        });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<DebugLogic>().on_code_mode(move |mode| {
        {
            let mut state = held.lock().unwrap();
            if state.transition_pending() {
                return;
            }
            state.debug.select_code(mode);
        }
        super::paint(&weak, &held);
    });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<DebugLogic>().on_inspector_tab(move |tab| {
        {
            let mut state = held.lock().unwrap();
            if state.transition_pending() {
                return;
            }
            state.debug.select_inspector(tab);
        }
        super::paint(&weak, &held);
    });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<DebugLogic>().on_inspect_tab(move |tab| {
        {
            let mut state = held.lock().unwrap();
            if state.transition_pending() {
                return;
            }
            state.debug.select_inspection(tab);
        }
        super::paint(&weak, &held);
    });
}

/// Exports the paused session's capture through the worker and saves it in
/// `.chasm/captures/`. Synthetic and Foundry runs have no replay baseline.
fn save_capture(weak: Weak<MainWindow>, held: State, runtime: tokio::runtime::Handle) {
    let prepared = {
        let mut state = held.lock().unwrap();
        if state.transition_pending() || !state.workspace_ready {
            return;
        }
        if matches!(state.debug_launch_mode, 0 | 1) {
            state.debug_capture_status = "Only a replay of a chain transaction has a capture to save. Synthetic and Foundry runs have no replay baseline.".into();
            drop(state);
            super::paint(&weak, &held);
            return;
        }
        let Some(envelope) = state.debug.envelope.clone().filter(|_| state.debug.can_command()) else {
            state.debug_capture_status = "Pause a debugger session first.".into();
            drop(state);
            super::paint(&weak, &held);
            return;
        };
        state.debug_capture_status = "Exporting the capture…".into();
        (client(&state), envelope, state.workspace.clone(), state.debug_capture_id.clone())
    };
    super::paint(&weak, &held);
    let (client, envelope, workspace, label) = prepared;
    runtime.spawn(async move {
        let sent = session::debugger_command(&client.http, &client.csrf, &envelope.id, envelope.generation, envelope.revision, "export_capture", json!({})).await;
        let mut outcome = sent.map(|_| ());
        let started = tokio::time::Instant::now();
        let mut capture = Value::Null;
        while outcome.is_ok() && started.elapsed() < Duration::from_secs(60) {
            match session::debugger_snapshot(client.http.clone(), &envelope.id).await {
                Ok(value) if value["busy"] == false && value["revision"].as_u64() > Some(envelope.revision) => {
                    if let Some(error) = value["error"].as_object() {
                        outcome = Err(error.get("message").and_then(Value::as_str).unwrap_or("The worker refused the export.").to_string());
                    }
                    capture = value["result"].clone();
                    break;
                }
                Ok(_) => tokio::time::sleep(Duration::from_millis(200)).await,
                Err(error) => outcome = Err(error),
            }
        }
        let saved = outcome.and_then(|_| super::captures::save(&workspace, &label, &capture));
        let _ = slint::invoke_from_event_loop(move || {
            let mut state = held.lock().unwrap();
            if same_workspace(&state, &client) {
                state.debug_capture_status = match saved {
                    Ok(path) => format!("Saved {}", path.display().to_string().trim_start_matches(r"\\?\")),
                    Err(error) if error.contains("baseline") => "This session has no replay baseline to export. Only a replay of a chain transaction can be saved as a capture.".into(),
                    Err(error) => format!("Capture not saved: {error}"),
                };
            }
            drop(state);
            super::paint(&weak, &held);
        });
    });
}

/// Uploads a saved capture as a gateway resource and selects it for an
/// Offline capture launch; Start then replays it without the network.
fn open_capture(weak: Weak<MainWindow>, held: State, runtime: tokio::runtime::Handle, name: String) {
    let prepared = {
        let mut state = held.lock().unwrap();
        if state.transition_pending() || !state.workspace_ready || !state.debug.can_start() {
            return;
        }
        match super::captures::read(&state.workspace, &name) {
            Ok(content) => {
                state.debug_capture_status = format!("Uploading {name}…");
                Some((client(&state), content))
            }
            Err(error) => {
                state.debug_capture_status = error;
                None
            }
        }
    };
    super::paint(&weak, &held);
    let Some((mut client, content)) = prepared else { return };
    runtime.spawn(async move {
        if client.csrf.is_empty() {
            if let Ok(csrf) = session::establish_debugger(&client.http).await {
                client.csrf = csrf;
            }
        }
        let uploaded = session::debugger_upload_capture(&client.http, &client.csrf, &name, content).await;
        let _ = slint::invoke_from_event_loop(move || {
            let mut state = held.lock().unwrap();
            if same_workspace(&state, &client) {
                if !client.csrf.is_empty() {
                    state.csrf = client.csrf.clone();
                }
                match uploaded {
                    Ok(id) => {
                        state.debug_launch_mode = 2;
                        state.debug_capture_id = id;
                        state.debug_launch_summary = launch_summary(&state);
                        state.debug_capture_status = format!("{name} is ready. Press Start offline capture.");
                    }
                    Err(error) => state.debug_capture_status = format!("Could not open {name}: {error}"),
                }
            }
            drop(state);
            super::paint(&weak, &held);
        });
    });
}

fn launch_request(state: &DesktopState) -> Result<super::debug_transport::DebugLaunchRequest, String> {
    match state.debug_launch_mode {
        0 => Ok(super::debug_transport::DebugLaunchRequest::Synthetic),
        1 => Ok(super::debug_transport::DebugLaunchRequest::Foundry {
            target: state.debug_foundry_target.clone(),
            r#match: (!state.debug_foundry_match.is_empty())
                .then(|| state.debug_foundry_match.clone()),
            trusted: state.debug_foundry_trusted,
        }),
        2 => Ok(super::debug_transport::DebugLaunchRequest::Offline {
            capture_id: state.debug_capture_id.clone(),
            build_info_id: (!state.debug_build_info_id.is_empty())
                .then(|| state.debug_build_info_id.clone()),
        }),
        _ => Err("Choose a debugger launch source.".into()),
    }
}

fn launch_summary(state: &DesktopState) -> String {
    match state.debug_launch_mode {
        0 => "Synthetic WorkshopVault fixture. It is not a replay of this project or a chain transaction.".into(),
        1 => format!(
            "Trusted local Foundry target: {}{}",
            state.debug_foundry_target,
            if state.debug_foundry_match.is_empty() {
                String::new()
            } else {
                format!(" · match {}", state.debug_foundry_match)
            }
        ),
        2 => format!(
            "Offline capture resource: {}{}",
            state.debug_capture_id,
            if state.debug_build_info_id.is_empty() {
                " · no build-info associated".into()
            } else {
                format!(" · build-info {}", state.debug_build_info_id)
            }
        ),
        _ => "Choose a debugger launch source.".into(),
    }
}

fn client(state: &DesktopState) -> Client {
    Client {
        http: state.client.clone(),
        csrf: state.csrf.clone(),
        workspace_epoch: state.workspace_epoch,
    }
}
fn same_workspace(state: &DesktopState, client: &Client) -> bool {
    state.active
        && state.workspace_ready
        && !state.workspace_busy
        && state.workspace_epoch == client.workspace_epoch
}

fn update_summary(state: &mut DesktopState) {
    let debug = &state.debug;
    state.debugger_live = matches!(
        debug.phase,
        Phase::Preparing | Phase::Running | Phase::Paused | Phase::Cancelling
    );
    state.debugger_label = format!("Debugger {}", debug.phase.label());
    if let Some(envelope) = &debug.envelope {
        state.session_id = Some(envelope.id.clone());
        state.generation = envelope.generation;
        state.revision = envelope.revision;
    }
}

fn start(weak: Weak<MainWindow>, held: State, runtime: tokio::runtime::Handle) {
    let prepared = {
        let mut state = held.lock().unwrap();
        if state.transition_pending() || !state.workspace_ready {
            return;
        }
        let request = match launch_request(&state).and_then(|request| request.prepare()) {
            Ok(request) => request,
            Err(error) => {
                state.debug.notice = error;
                drop(state);
                super::paint(&weak, &held);
                return;
            }
        };
        let Some(ticket) = state.debug.begin_start() else {
            return;
        };
        state.debug_launch_summary = launch_summary(&state);
        state
            .console
            .cancel_all_pending("Debugger session changed.");
        state.activity = 4;
        state.debug_prepared_launch = Some(request.clone());
        let synthetic = state.debug_launch_mode == 0;
        update_summary(&mut state);
        (client(&state), ticket, request, synthetic)
    };
    super::paint(&weak, &held);
    runtime.spawn(async move {
        let (mut client, ticket, request, synthetic) = prepared;
        let issued = client.csrf.clone();
        let result = super::debug_transport::start(&client.http, &mut client.csrf, &request).await;
        if client.csrf != issued {
            let mut state = held.lock().unwrap();
            if same_workspace(&state, &client) {
                state.csrf = client.csrf.clone();
                state.debugger_label = "Debugger ready".into();
            }
        }
        match result {
            Ok(value) => {
                let id = value.get("id").and_then(Value::as_str).map(str::to_string);
                match deliver(&weak, &held, &client, &ticket, Ok(value)).await {
                    Acceptance::Accepted => {
                        if let Some(id) = id {
                            poll(weak, held, client, ticket, id, synthetic).await;
                        }
                    }
                    Acceptance::Cancel(id) => {
                        let result =
                            session::cancel_debugger(&client.http, &client.csrf, &id).await;
                        let _ = slint::invoke_from_event_loop(move || {
                            let mut state = held.lock().unwrap();
                            if same_workspace(&state, &client)
                                && state.debug.phase == Phase::Cancelling
                                && (state
                                    .debug
                                    .envelope
                                    .as_ref()
                                    .is_some_and(|value| value.id == id)
                                    || state.debug.orphan_session.as_ref() == Some(&id))
                            {
                                let epoch = state.debug.cancel_epoch();
                                state.debug.cancelled(epoch, result);
                                update_summary(&mut state);
                            }
                            drop(state);
                            super::paint(&weak, &held);
                        });
                    }
                    Acceptance::Ignored => {}
                }
            }
            Err(error) => {
                let _ = slint::invoke_from_event_loop(move || {
                    let mut state = held.lock().unwrap();
                    if same_workspace(&state, &client) {
                        let request_id = error.request.request_id().to_owned();
                        state.debug_prepared_launch = error.ambiguous.then_some(error.request);
                        state
                            .debug
                            .launch_failed(request_id, error.message, error.ambiguous);
                        update_summary(&mut state);
                    }
                    drop(state);
                    super::paint(&weak, &held);
                });
            }
        }
    });
}

#[derive(Clone)]
struct ConsoleOperation {
    entry: u64,
    method: &'static str,
    params: Value,
    session_id: String,
    generation: u64,
    allow_generation_change: bool,
    revision: u64,
    sequence: u64,
}

impl ConsoleOperation {
    fn new(entry: u64, method: &'static str, params: Value, envelope: &Envelope) -> Self {
        Self {
            entry,
            method,
            allow_generation_change: method == "navigate"
                && params.get("action").and_then(Value::as_str) == Some("restart"),
            params,
            session_id: envelope.id.clone(),
            generation: envelope.generation,
            revision: envelope.revision,
            sequence: envelope.sequence,
        }
    }

    fn completion(&self, envelope: &Envelope) -> Option<Result<String, String>> {
        if envelope.id != self.session_id
            || envelope.busy
            || envelope.revision <= self.revision
            || envelope.sequence <= self.sequence
        {
            return None;
        }
        if envelope.generation != self.generation && !self.allow_generation_change {
            return Some(Err(
                "Debugger generation changed before this command completed.".into(),
            ));
        }
        if let Some(error) = &envelope.error {
            return Some(Err(format!("{}: {}", error.code, error.message)));
        }
        if matches!(self.method, "navigate" | "snapshot") && envelope.result.is_some() {
            return Some(Err(
                "Unexpected result for this command; refresh before retrying.".into(),
            ));
        }
        if matches!(
            envelope.state,
            Phase::Failed | Phase::Cancelled | Phase::Cancelling
        ) {
            return Some(Err(format!(
                "Debugger {} without a command result.",
                envelope.state.label()
            )));
        }
        let output = match self.method {
            "evaluate" => envelope
                .result
                .as_ref()
                .and_then(|value| value.get("evaluation")),
            "breakpoint" => envelope.result.as_ref(),
            "snapshot" => envelope.snapshot.as_ref().map(|snapshot| {
                if self.params.get("memoryOffset").is_some() {
                    &snapshot.panes.memory
                } else if self.params.get("aroundPc").is_some() {
                    &snapshot.panes.disassembly
                } else if self.params.get("sourceStart").is_some() {
                    &snapshot.panes.source
                } else {
                    &snapshot.status
                }
            }),
            _ => None,
        };
        if self.method == "evaluate" && output.is_none() {
            return Some(Err(
                "Evaluation completed without an evaluation result.".into()
            ));
        }
        if self.method == "breakpoint" && output.is_none() {
            return Some(Err("Breakpoint completed without a result.".into()));
        }
        Some(Ok(output.map(|value| {
            if self.method == "evaluate" { console_evaluation(value) } else { pretty(value) }
        }).unwrap_or_else(|| format!("Debugger {}.", envelope.state.label()))))
    }
}

fn console_evaluation(value: &Value) -> String {
    match (value.get("display").and_then(Value::as_str), value.get("type").and_then(Value::as_str), value.get("kept").and_then(Value::as_bool)) {
        (Some(display), Some(kind), Some(false)) => format!("{kind}: {display} (effects discarded, kept=false)"),
        _ => pretty(value),
    }
}

fn begin_console_command(
    debug: &mut DebugSession,
    method: &str,
    params: &Value,
) -> Result<Ticket, String> {
    debug.begin_command(method, params.get("action").and_then(Value::as_str)).ok_or_else(||
        "Debugger is busy, finished or not paused in a current context. Commands are not queued or retried.".into())
}

fn command(
    weak: Weak<MainWindow>,
    held: State,
    runtime: tokio::runtime::Handle,
    method: &'static str,
    params: Value,
) {
    command_tracked(weak, held, runtime, method, params, None);
}

fn command_tracked(
    weak: Weak<MainWindow>,
    held: State,
    runtime: tokio::runtime::Handle,
    method: &'static str,
    params: Value,
    console_entry: Option<u64>,
) {
    let prepared = {
        let mut state = held.lock().unwrap();
        let error = if state.transition_pending() || !state.workspace_ready {
            Some("A local confirmation or workspace transition is pending.".to_string())
        } else {
            allowed_command(&state.debug, method, &params).err()
        };
        if let Some(error) = error {
            if let Some(entry) = console_entry {
                state.console.finish(entry, Err(error.clone()));
            }
            state.debug.notice = error;
            drop(state);
            super::paint(&weak, &held);
            return;
        }
        let Some(envelope) = state.debug.envelope.clone() else {
            if let Some(entry) = console_entry {
                state
                    .console
                    .finish(entry, Err("No debugger session is available.".into()));
            }
            drop(state);
            super::paint(&weak, &held);
            return;
        };
        let ticket = match begin_console_command(&mut state.debug, method, &params) {
            Ok(ticket) => ticket,
            Err(error) => {
                if let Some(entry) = console_entry {
                    state.console.finish(entry, Err(error));
                }
                drop(state);
                super::paint(&weak, &held);
                return;
            }
        };
        let operation = console_entry.map(|entry| {
            let mut operation = ConsoleOperation::new(entry, method, params.clone(), &envelope);
            if method == "snapshot" && params.as_object().is_some_and(|object| object.is_empty()) {
                if let Some(command) = state
                    .console
                    .entries()
                    .iter()
                    .find(|item| item.id == entry)
                    .map(|item| item.command.as_str())
                {
                    match command.split_whitespace().next() {
                        Some("list" | "l") => operation.params["sourceStart"] = Value::Null,
                        Some("disassemble" | "disas") => operation.params["aroundPc"] = Value::Null,
                        _ => {}
                    }
                }
            }
            operation
        });
        (
            client(&state),
            ticket,
            envelope.id,
            envelope.generation,
            envelope.revision,
            operation,
        )
    };
    super::paint(&weak, &held);
    runtime.spawn(async move {
        let (client, ticket, id, generation, revision, operation) = prepared;
        let result = if method == "refresh" {
            session::debugger_snapshot(client.http.clone(), &id).await
        } else {
            session::debugger_command(
                &client.http,
                &client.csrf,
                &id,
                generation,
                revision,
                method,
                params,
            )
            .await
        };
        let accepted = deliver_command(
            &weak,
            &held,
            &client,
            &ticket,
            result,
            operation.as_ref(),
            method != "refresh",
        )
        .await;
        if accepted == Acceptance::Accepted && method != "refresh" {
            poll_command(weak, held, client, ticket, id, operation).await;
        }
    });
}

fn apply_delivery(
    state: &mut DesktopState,
    client: &Client,
    ticket: &Ticket,
    result: Result<Value, String>,
    operation: Option<&ConsoleOperation>,
    acknowledgement: bool,
) -> Acceptance {
    let accepted = if same_workspace(state, client) {
        match result {
            Ok(value) => {
                match state.debug.accept(ticket, &value) {
                    Ok(acceptance) => {
                        if matches!(acceptance, Acceptance::Cancel(_)) {
                            state.debug.track_cancelled_launch(&value);
                        }
                        if let Some(operation) = operation {
                            match &acceptance {
                                Acceptance::Accepted if !acknowledgement => {
                                    let completion = state
                                        .debug
                                        .envelope
                                        .as_ref()
                                        .and_then(|envelope| operation.completion(envelope));
                                    if let Some(result) = completion {
                                        state.console.finish(operation.entry, result);
                                    } else if state
                                        .debug
                                        .envelope
                                        .as_ref()
                                        .is_some_and(|envelope| !envelope.busy)
                                    {
                                        state.console.finish(operation.entry, Err("Could not attribute this debugger result to the command; refresh before retrying.".into()));
                                    }
                                }
                                Acceptance::Ignored | Acceptance::Cancel(_) => {
                                    if state.debug.accepts(ticket) {
                                        let error = "Debugger returned an out-of-order or unexpected session response; refresh before retrying.".to_string();
                                        state.console.finish(operation.entry, Err(error.clone()));
                                        state.debug.failure(ticket, error);
                                    } else {
                                        state.console.cancel(operation.entry, "Command was cancelled or superseded; late response ignored.");
                                    }
                                }
                                _ => {}
                            }
                        }
                        acceptance
                    }
                    Err(error) => {
                        if let Some(id) = value
                            .get("id")
                            .and_then(Value::as_str)
                            .filter(|id| ticket.is_launch() && !id.is_empty() && id.len() <= 128)
                        {
                            state.debug.orphan(id.into());
                            Acceptance::Cancel(id.into())
                        } else {
                            if let Some(operation) = operation {
                                if state.debug.accepts(ticket) {
                                    state.console.finish(operation.entry, Err(error.clone()));
                                } else {
                                    state.console.cancel(operation.entry, "Command was cancelled or superseded; late response ignored.");
                                }
                            }
                            state.debug.failure(ticket, error);
                            Acceptance::Ignored
                        }
                    }
                }
            }
            Err(error) => {
                if let Some(operation) = operation {
                    if state.debug.accepts(ticket) {
                        state.console.finish(operation.entry, Err(error.clone()));
                    } else {
                        state.console.cancel(
                            operation.entry,
                            "Command was cancelled or superseded; late response ignored.",
                        );
                    }
                }
                state.debug.failure(ticket, error);
                Acceptance::Ignored
            }
        }
    } else {
        if state.workspace_epoch == client.workspace_epoch {
            if let Some(operation) = operation {
                state.console.cancel(
                    operation.entry,
                    "Workspace changed before command completion.",
                );
            }
        }
        if ticket.is_launch() {
            result
                .ok()
                .and_then(|value| value.get("id").and_then(Value::as_str).map(str::to_string))
                .map(Acceptance::Cancel)
                .unwrap_or(Acceptance::Ignored)
        } else {
            Acceptance::Ignored
        }
    };
    if same_workspace(state, client) {
        update_summary(state);
    }
    accepted
}

async fn deliver_command(
    weak: &Weak<MainWindow>,
    held: &State,
    client: &Client,
    ticket: &Ticket,
    result: Result<Value, String>,
    operation: Option<&ConsoleOperation>,
    acknowledgement: bool,
) -> Acceptance {
    let accepted = {
        let mut state = held.lock().unwrap();
        apply_delivery(
            &mut state,
            client,
            ticket,
            result,
            operation,
            acknowledgement,
        )
    };
    let weak = weak.clone();
    let held = held.clone();
    let _ = slint::invoke_from_event_loop(move || super::paint(&weak, &held));
    accepted
}

async fn deliver(
    weak: &Weak<MainWindow>,
    held: &State,
    client: &Client,
    ticket: &Ticket,
    result: Result<Value, String>,
) -> Acceptance {
    deliver_command(weak, held, client, ticket, result, None, false).await
}

async fn poll_command(
    weak: Weak<MainWindow>,
    held: State,
    client: Client,
    ticket: Ticket,
    id: String,
    operation: Option<ConsoleOperation>,
) {
    let started = tokio::time::Instant::now();
    loop {
        let current = {
            let mut state = held.lock().unwrap();
            let current = same_workspace(&state, &client) && state.debug.accepts(&ticket);
            if !current && state.workspace_epoch == client.workspace_epoch {
                if let Some(operation) = &operation {
                    state.console.cancel(
                        operation.entry,
                        "Command was cancelled or superseded; late response ignored.",
                    );
                }
            }
            current
        };
        if !current {
            let weak = weak.clone();
            let held = held.clone();
            let _ = slint::invoke_from_event_loop(move || super::paint(&weak, &held));
            return;
        }
        if started.elapsed() > Duration::from_secs(65) {
            let _ = deliver_command(
                &weak, &held, &client, &ticket,
                Err("Debugger is still busy. Use Refresh to reconcile or Cancel; commands are not retried.".into()),
                operation.as_ref(), false,
            ).await;
            return;
        }
        let result = session::debugger_snapshot(client.http.clone(), &id).await;
        let settled = result
            .as_ref()
            .ok()
            .is_some_and(|value| value.get("busy") == Some(&json!(false)));
        let accepted = deliver_command(
            &weak,
            &held,
            &client,
            &ticket,
            result,
            operation.as_ref(),
            false,
        )
        .await;
        if let Acceptance::Cancel(orphan) = &accepted {
            let _ = session::cancel_debugger(&client.http, &client.csrf, orphan).await;
            return;
        }
        if accepted != Acceptance::Accepted {
            return;
        }
        if settled {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn poll(
    weak: Weak<MainWindow>,
    held: State,
    client: Client,
    ticket: Ticket,
    id: String,
    preset: bool,
) {
    let started = tokio::time::Instant::now();
    loop {
        let ongoing = {
            let state = held.lock().unwrap();
            same_workspace(&state, &client) && state.debug.accepts(&ticket)
        };
        if !ongoing {
            return;
        }
        if started.elapsed() > Duration::from_secs(65) {
            let _=deliver(&weak,&held,&client,&ticket,Err("Debugger is still busy. Use Refresh to reconcile or Cancel; commands are not retried.".into())).await;
            return;
        }
        let result = session::debugger_snapshot(client.http.clone(), &id).await;
        let settled = result
            .as_ref()
            .ok()
            .is_some_and(|value| value.get("busy") == Some(&json!(false)));
        if settled
            && preset
            && result
                .as_ref()
                .ok()
                .is_some_and(|value| value.get("state").and_then(Value::as_str) == Some("paused"))
        {
            let value = result.unwrap();
            let generation = value["generation"].as_u64().unwrap_or(0);
            let revision = value["revision"].as_u64().unwrap_or(0);
            post_preset_breakpoint(weak, held, client, ticket, id, generation, revision).await;
            return;
        }
        let accepted = deliver(&weak, &held, &client, &ticket, result).await;
        if let Acceptance::Cancel(orphan) = accepted {
            let _ = session::cancel_debugger(&client.http, &client.csrf, &orphan).await;
            return;
        }
        if settled {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn post_preset_breakpoint(
    weak: Weak<MainWindow>,
    held: State,
    client: Client,
    ticket: Ticket,
    id: String,
    generation: u64,
    revision: u64,
) {
    {
        let state = held.lock().unwrap();
        if !same_workspace(&state, &client) || !state.debug.accepts(&ticket) {
            return;
        }
    }
    let result = session::debugger_command(
        &client.http,
        &client.csrf,
        &id,
        generation,
        revision,
        "breakpoint",
        json!({"action":"add","location":"WorkshopVault.withdraw"}),
    )
    .await;
    if deliver(&weak, &held, &client, &ticket, result).await == Acceptance::Accepted {
        poll_simple(weak, held, client, ticket, id).await;
    }
}
async fn poll_simple(
    weak: Weak<MainWindow>,
    held: State,
    client: Client,
    ticket: Ticket,
    id: String,
) {
    for _ in 0..325 {
        {
            let state = held.lock().unwrap();
            if !same_workspace(&state, &client) || !state.debug.accepts(&ticket) {
                return;
            }
        }
        let result = session::debugger_snapshot(client.http.clone(), &id).await;
        let settled = result
            .as_ref()
            .ok()
            .is_some_and(|value| value.get("busy") == Some(&json!(false)));
        let accepted = deliver(&weak, &held, &client, &ticket, result).await;
        if let Acceptance::Cancel(orphan) = accepted {
            let _ = session::cancel_debugger(&client.http, &client.csrf, &orphan).await;
            return;
        }
        if settled {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn cancel(weak: Weak<MainWindow>, held: State, runtime: tokio::runtime::Handle) {
    let prepared = {
        let mut state = held.lock().unwrap();
        if !state.active || !state.workspace_ready || !state.debug.can_cancel() {
            return;
        }
        state
            .console
            .cancel_all_pending("Worker cancellation requested.");
        let (id, epoch) = state.debug.cancel();
        update_summary(&mut state);
        (
            client(&state),
            id,
            epoch,
            state.debug_prepared_launch.clone(),
        )
    };
    super::paint(&weak, &held);
    runtime.spawn(async move {
        let (client, id, epoch, recovery) = prepared;
        let result = match (id, recovery) {
            (Some(id), _) => session::cancel_debugger(&client.http, &client.csrf, &id).await,
            (None, Some(request)) => {
                super::debug_transport::reconcile_cancel(&client.http, &client.csrf, &request).await
            }
            (None, None) => return,
        };
        let _ = slint::invoke_from_event_loop(move || {
            let mut state = held.lock().unwrap();
            if !state.active
                || !state.workspace_ready
                || state.workspace_epoch != client.workspace_epoch
            {
                return;
            }
            state.debug.cancelled(epoch, result);
            if state.debug.phase == Phase::Cancelled {
                state.debug_prepared_launch = None;
            }
            update_summary(&mut state);
            drop(state);
            super::paint(&weak, &held);
        });
    });
}

fn allowed_command(debug: &DebugSession, method: &str, params: &Value) -> Result<(), String> {
    let capabilities = debug.snapshot.as_ref().map(|value| &value.capabilities);
    match method {
        "navigate" => {
            let action = params
                .get("action")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if ![
                "continue",
                "step_opcode",
                "step_line",
                "next_line",
                "finish",
                "restart",
                "run_to",
            ]
            .contains(&action)
            {
                return Err("Unsupported navigation action.".into());
            }
            if ["step_line", "next_line"].contains(&action)
                && !capabilities.is_some_and(|value| value.source)
            {
                return Err("Source stepping is unavailable for this frame.".into());
            }
            if action == "run_to"
                && !params
                    .get("location")
                    .and_then(Value::as_str)
                    .is_some_and(|value| !value.trim().is_empty() && value.len() <= 1024)
            {
                return Err("Enter a location such as File.sol:42 or *0x20.".into());
            }
        }
        "evaluate" => {
            if !capabilities.is_some_and(|value| value.solidity) {
                return Err("Solidity evaluation requires matching, supported build-info.".into());
            }
            if params.get("keep") != Some(&json!(false)) {
                return Err("Native evaluation discards effects; Keep is not exposed.".into());
            }
            if !params
                .get("expression")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.trim().is_empty() && value.len() <= 4096)
            {
                return Err("Enter a bounded Solidity expression.".into());
            }
        }
        "breakpoint" => {
            let action = params["action"].as_str().unwrap_or_default();
            if !["add", "delete", "list", "opcode", "watch"].contains(&action) {
                return Err("Unsupported breakpoint action.".into());
            }
        }
        "snapshot" | "refresh" => {}
        "export_capture" => {}
        _ => return Err("Unsupported native debugger method.".into()),
    }
    Ok(())
}

fn local_snapshot(debug: &DebugSession, pane: Option<&str>) -> String {
    let Some(snapshot) = &debug.snapshot else {
        return "No snapshot available.".into();
    };
    let value = match pane {
        None | Some("registers") => &snapshot.status,
        Some("frames") => &snapshot.panes.frames,
        Some("locals") => &snapshot.panes.locals,
        Some("arguments") => &snapshot.panes.arguments,
        Some("gas") => &snapshot.panes.gas,
        Some("logs") => &snapshot.panes.logs,
        Some("storage") => &snapshot.panes.storage,
        Some("breakpoints") => &snapshot.breakpoints,
        _ => &Value::Null,
    };
    let prefix = if debug.can_command() {
        ""
    } else {
        "Retained snapshot, not a live execution context:\n"
    };
    if pane.is_none() || pane == Some("registers") {
        let keys = ["pc", "opcode", "gas", "gas_remaining", "gas_used", "depth", "stopped", "finished", "location", "stack_size", "memory_size"];
        let public = value.as_object().map(|object| object.iter().filter(|(key, _)| keys.contains(&key.as_str())).map(|(key, value)| (key.clone(), value.clone())).collect::<serde_json::Map<_, _>>()).unwrap_or_default();
        return format!("{prefix}{}\n{}", debug.phase.label(), pretty(&Value::Object(public)));
    }
    format!("{prefix}{}", pretty(value))
}

fn console_admitted(
    state: &DesktopState,
    action: &Result<super::console::ConsoleAction, String>,
) -> bool {
    state.active
        && state.workspace_ready
        && (!state.transition_pending()
            || matches!(action, Ok(super::console::ConsoleAction::Cancel)))
}

pub fn console(
    weak: Weak<MainWindow>,
    held: State,
    runtime: tokio::runtime::Handle,
    input: String,
) {
    use super::console::{ConsoleAction, LocalQuery};
    let action = super::console::parse(&input);
    let entry = {
        let mut state = held.lock().unwrap();
        if !console_admitted(&state, &action) {
            return;
        }
        state.console.submit(&input)
    };
    match action {
        Err(error) => {
            held.lock().unwrap().console.finish(entry, Err(error));
        }
        Ok(ConsoleAction::Cancel) => {
            let can_cancel = {
                let mut state = held.lock().unwrap();
                let can_cancel = state.active && state.workspace_ready && state.debug.can_cancel();
                let result = if can_cancel {
                    Ok("Cancelling the shared worker, not pausing it.".into())
                } else {
                    Err("No active debugger worker to cancel.".into())
                };
                state.console.finish(entry, result);
                can_cancel
            };
            if can_cancel {
                cancel(weak.clone(), held.clone(), runtime);
            }
        }
        Ok(ConsoleAction::Remote { method, params }) => {
            command_tracked(
                weak.clone(),
                held.clone(),
                runtime,
                method,
                params,
                Some(entry),
            );
        }
        Ok(ConsoleAction::Local(query)) => {
            let mut state = held.lock().unwrap();
            let output=match query {
                LocalQuery::Help=>"Commands: c/continue; s/step [count]; si/stepi [count]; n/next [count]; finish; until File.sol:line or *PC; run/reset (capability); p/print EXPR (discard); ptype EXPR; bt; info locals/args/storage/gas/logs/registers/breakpoints; list [line]; disas [PC]; x/32xb OFFSET (bounded64word window); break LOCATION [if EXPR]; breakop OPCODE; watch EXPR; delete ID; history; clear; cancel. No shell, files, set, Keep, Yul/vm, frame selection, nexti or $N history.".into(),
                LocalQuery::History=>state.console.history().iter().enumerate().map(|(index,text)|format!("{} {text}",index+1)).collect::<Vec<_>>().join("\n"),
                LocalQuery::Clear=>{state.console.clear();"Transcript cleared; running commands were retained.".into()},
                LocalQuery::Status=>local_snapshot(&state.debug, None),
                LocalQuery::Pane(name)=>local_snapshot(&state.debug, Some(name)),
            };
            state.console.finish(entry, Ok(output));
        }
    }
    held.lock().unwrap().console.set_draft("");
    if let Some(ui) = weak.upgrade() {
        ui.global::<super::PanelData>().set_console_input("".into());
    }
    super::paint(&weak, &held);
}

pub fn render(ui: &MainWindow, state: &DesktopState) {
    let debug = &state.debug;
    let data = ui.global::<DebugData>();
    if data.get_launch_mode() != state.debug_launch_mode {
        data.set_launch_mode(state.debug_launch_mode);
    }
    macro_rules! project_launch {
        ($value:expr, $get:ident, $set:ident) => {
            if data.$get().as_str() != $value {
                data.$set($value.into());
            }
        };
    }
    project_launch!(state.debug_foundry_target.as_str(), get_foundry_target, set_foundry_target);
    project_launch!(state.debug_foundry_match.as_str(), get_foundry_match, set_foundry_match);
    project_launch!(state.debug_capture_id.as_str(), get_capture_id, set_capture_id);
    project_launch!(state.debug_build_info_id.as_str(), get_build_info_id, set_build_info_id);
    project_launch!(state.debug_launch_summary.as_str(), get_launch_summary, set_launch_summary);
    project_launch!(state.debug_capture_status.as_str(), get_capture_status, set_capture_status);
    if state.debug_launch_mode == 2 {
        data.set_saved_captures(super::model(super::captures::list(&state.workspace).into_iter().map(Into::into).collect()));
    }
    if data.get_foundry_trusted() != state.debug_foundry_trusted {
        data.set_foundry_trusted(state.debug_foundry_trusted);
    }
    data.set_phase(debug.phase.label().into());
    data.set_status(
        if debug.pending() {
            format!(
                "{}: operation in flight, Cancel remains independent",
                debug.phase.label()
            )
        } else {
            format!("{}: local worker only, no broadcast", debug.phase.label())
        }
        .into(),
    );
    data.set_notice(debug.notice.clone().into());
    data.set_can_start(
        state.workspace_ready
            && !state.transition_pending()
            && debug.can_start(),
    );
    let commands_enabled = state.workspace_ready && !state.transition_pending();
    data.set_can_command(commands_enabled && debug.can_command());
    data.set_can_cancel(state.active && state.workspace_ready && debug.can_cancel());
    data.set_has_snapshot(debug.snapshot.is_some());
    data.set_can_refresh(commands_enabled && debug.can_refresh());
    let capabilities = debug
        .snapshot
        .as_ref()
        .map(|snapshot| &snapshot.capabilities);
    data.set_source_available(capabilities.is_some_and(|value| value.source));
    data.set_solidity_available(capabilities.is_some_and(|value| value.solidity));
    data.set_restart_available(commands_enabled && debug.can_restart());
    data.set_capability_reason(capabilities.map(|value| value.reason.clone()).unwrap_or_else(|| {
        match state.debug_launch_mode {
            0 => "Synthetic fixture selected; source, locals and Solidity evaluation depend on its bundled build-info.",
            1 => "Trusted Foundry source selected; viaIR or optimized locals and Solidity evaluation may be unavailable.",
            _ => "Offline capture selected; capabilities depend on the capture and matching build-info resource.",
        }
        .into()
    }).into());
    data.set_code_mode(debug.code_mode);
    data.set_inspector_tab(debug.inspector_tab);
    data.set_inspect_tab(debug.inspect_tab);
    macro_rules! project_draft {
        ($key:literal, $get:ident, $set:ident) => {
            let value = draft(state, $key);
            if data.$get().as_str() != value {
                data.$set(value.into());
            }
        };
    }
    project_draft!("run-to", get_run_to, set_run_to);
    project_draft!("evaluate-input", get_evaluate_input, set_evaluate_input);
    project_draft!("memory-offset", get_memory_offset, set_memory_offset);
    project_draft!("storage-slots", get_storage_slots, set_storage_slots);
    project_draft!("source-start", get_source_start, set_source_start);
    project_draft!("around-pc", get_around_pc, set_around_pc);
    if let Some(snapshot) = &debug.snapshot {
        let status = &snapshot.status;
        let execution = execution_stats(status);
        if data.get_execution().row_count() != execution.len()
            || execution.iter().enumerate().any(|(index, stat)| data.get_execution().row_data(index).as_ref() != Some(stat))
        {
            data.set_execution(super::model(execution));
        }
        data.set_breakpoint_here(breakpoint_location(status).unwrap_or_default().into());
        data.set_location(
            format!(
                "PC {} · {} · {}:{}",
                display(&status["pc"]),
                display(&status["mnemonic"]),
                display(&status["location"]["file"]),
                display(&status["location"]["line"])
            )
            .into(),
        );
        let fork = &snapshot.fork;
        let baseline = fork
            .get("baseline")
            .or_else(|| status.get("baseline"))
            .cloned()
            .unwrap_or(Value::Null);
        data.set_baseline(
            if baseline.get("validated") == Some(&json!(true)) {
                "Captured receipt agreement, offline / may be synthetic"
            } else {
                "Not independently validated; synthetic offline fixture"
            }
            .into(),
        );
        let experiment = fork.get("experiment_result").unwrap_or(&Value::Null);
        data.set_outcome(
            match experiment.get("success").or_else(|| status.get("success")) {
                Some(Value::Bool(true)) => "EVM outcome: success".into(),
                Some(Value::Bool(false)) => "EVM outcome: reverted / failed".into(),
                _ => format!("EVM outcome: {}", display(&status["stop_reason"])).into(),
            },
        );
        data.set_completion(
            if status.get("finished") == Some(&json!(true)) {
                "Worker completion: completed"
            } else {
                "Worker completion: not completed"
            }
            .into(),
        );
        data.set_divergence(
            if fork.get("diverged") == Some(&json!(true))
                || status.get("diverged") == Some(&json!(true))
            {
                "Local divergence reported"
            } else {
                "No local divergence reported"
            }
            .into(),
        );
        data.set_frames_text(pretty(&snapshot.panes.frames).into());
        data.set_locals_text(
            if snapshot.capabilities.locals {
                pretty(&snapshot.panes.locals)
            } else {
                format!("Locals unavailable: {}", snapshot.capabilities.reason)
            }
            .into(),
        );
        data.set_arguments_text(pretty(&snapshot.panes.arguments).into());
        let (title, rows) = code_rows(snapshot, debug.code_mode);
        data.set_code_title(title.into());
        let previous = data.get_code_rows();
        let same = previous.row_count() == rows.len()
            && rows
                .iter()
                .enumerate()
                .all(|(index, row)| previous.row_data(index).as_ref() == Some(row));
        if !same {
            data.set_current_code_index(
                rows.iter()
                    .position(|row| row.current)
                    .map(|index| index as i32)
                    .unwrap_or(-1),
            );
            data.set_code_rows(ModelRc::new(VecModel::from(rows)));
            data.set_code_revision(data.get_code_revision().wrapping_add(1));
        }
        let pane = match debug.inspect_tab {
            0 => &snapshot.panes.stack,
            1 => &snapshot.panes.memory,
            2 => &snapshot.panes.storage,
            3 => &snapshot.panes.calldata,
            4 => &snapshot.panes.logs,
            5 => &snapshot.panes.gas,
            _ => &snapshot.panes.diff,
        };
        let text = pretty(pane);
        if data.get_inspect_text().as_str() != text {
            data.set_inspect_text(text.into());
        }
        macro_rules! pane_text {
            ($value:expr,$get:ident,$set:ident) => {{
                let text = pretty($value);
                if data.$get().as_str() != text {
                    data.$set(text.into());
                }
            }};
        }
        pane_text!(&snapshot.panes.stack, get_stack_text, set_stack_text);
        pane_text!(&snapshot.panes.memory, get_memory_text, set_memory_text);
        pane_text!(&snapshot.panes.storage, get_storage_text, set_storage_text);
        pane_text!(
            &snapshot.panes.calldata,
            get_calldata_text,
            set_calldata_text
        );
        pane_text!(&snapshot.panes.logs, get_logs_text, set_logs_text);
        pane_text!(&snapshot.panes.gas, get_gas_text, set_gas_text);
        pane_text!(&snapshot.panes.diff, get_diff_text, set_diff_text);
    } else {
        data.set_location("No paused execution context".into());
        if data.get_execution().row_count() > 0 {
            data.set_execution(super::model(Vec::<ExecStat>::new()));
        }
        data.set_breakpoint_here("".into());
        data.set_baseline("No baseline supplied".into());
        data.set_outcome("EVM outcome: not reported".into());
        data.set_completion("Worker completion: not started".into());
        data.set_divergence("Local offline only".into());
        data.set_code_title("Source / Disassembly".into());
        if data.get_code_rows().row_count() > 0 {
            data.set_code_rows(super::model(Vec::<CodeRow>::new()));
        }
        data.set_current_code_index(-1);
        for setter in [
            DebugData::set_frames_text,
            DebugData::set_locals_text,
            DebugData::set_arguments_text,
            DebugData::set_inspect_text,
            DebugData::set_stack_text,
            DebugData::set_memory_text,
            DebugData::set_storage_text,
            DebugData::set_calldata_text,
            DebugData::set_logs_text,
            DebugData::set_gas_text,
            DebugData::set_diff_text,
        ] {
            setter(
                &data,
                "No snapshot in this session. Start or wait for preparation.".into(),
            );
        }
    }
    let result =
        debug.result.as_ref().map(pretty).unwrap_or_else(|| {
            "No evaluation result. Expressions discard effects by default.".into()
        });
    if data.get_result_text().as_str() != result {
        data.set_result_text(result.into());
    }
}

fn code_rows(snapshot: &super::debug_session::Snapshot, mode: i32) -> (String, Vec<CodeRow>) {
    let pane = if mode == 0 {
        &snapshot.panes.source
    } else {
        &snapshot.panes.disassembly
    };
    let rows = pane
        .get(if mode == 0 { "items" } else { "rows" })
        .and_then(Value::as_array);
    let title = if mode == 0 {
        format!(
            "{} · source window, {}",
            display(&pane["file"]),
            if pane["truncated"] == true {
                "bounded / truncated"
            } else {
                "complete returned window"
            }
        )
    } else {
        "EVM disassembly · bounded around PC".into()
    };
    let rows = rows
        .map(|values| {
            values
                .iter()
                .take(1024)
                .map(|row| CodeRow {
                    position: display(&row[if mode == 0 { "line" } else { "pc" }]).into(),
                    text: if mode == 0 {
                        row["text"]
                            .as_str()
                            .unwrap_or("Invalid source row")
                            .to_string()
                    } else {
                        row.get("text")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .unwrap_or_else(|| {
                                format!(
                                    "{} {}",
                                    display(&row["mnemonic"]),
                                    row.get("immediate")
                                        .or_else(|| row.get("operand"))
                                        .map(display)
                                        .unwrap_or_default()
                                )
                            })
                    }
                    .into(),
                    current: row["current"].as_bool().unwrap_or(false),
                })
                .collect()
        })
        .unwrap_or_else(|| {
            vec![CodeRow {
                position: "".into(),
                text: pretty(pane).into(),
                current: false,
            }]
        });
    let rows = if mode == 0 { dedent(rows) } else { rows };
    (title, rows)
}

/// Drop the indentation every non-blank line of the window shares, so a
/// function deep inside a contract starts at the gutter. Relative
/// indentation between the lines is kept.
fn dedent(rows: Vec<CodeRow>) -> Vec<CodeRow> {
    let common = rows
        .iter()
        .filter(|row| !row.text.trim().is_empty())
        .map(|row| row.text.len() - row.text.trim_start_matches([' ', '\t']).len())
        .min()
        .unwrap_or(0);
    if common == 0 {
        return rows;
    }
    rows.into_iter()
        .map(|row| {
            let text = row.text.as_str();
            let cut = text.len() - text.trim_start_matches([' ', '\t']).len();
            CodeRow { text: text[cut.min(common)..].into(), ..row }
        })
        .collect()
}
/// The at-a-glance execution card: PC, gas left, call depth, opcode and step.
fn execution_stats(status: &Value) -> Vec<ExecStat> {
    let pc = status["pc"]
        .as_u64()
        .map(|pc| format!("0x{pc:x}"))
        .unwrap_or_else(|| display(&status["pc"]));
    let gas = &status["gas"];
    let gas = gas["remaining"]
        .as_u64()
        .or_else(|| status["gas_remaining"].as_u64())
        .or_else(|| gas.as_u64())
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unavailable".into());
    let opcode = status["mnemonic"]
        .as_str()
        .or_else(|| status["opcode"].as_str())
        .unwrap_or("unavailable");
    [
        ("PC", pc),
        ("Gas left", gas),
        ("Depth", display(&status["depth"])),
        ("Opcode", opcode.into()),
        ("Step", display(&status["step"])),
    ]
    .into_iter()
    .map(|(label, value)| ExecStat { label: label.into(), value: value.into() })
    .collect()
}

/// `File.sol:LINE` for the paused source line, the location a breakpoint at
/// the current position uses. Opcode-only stops have no source location.
fn breakpoint_location(status: &Value) -> Option<String> {
    let location = &status["location"];
    let file = location["file"].as_str().filter(|file| !file.is_empty())?;
    let line = location["line"].as_u64()?;
    let name = file.rsplit(['/', '\\']).next().unwrap_or(file);
    Some(format!("{name}:{line}"))
}

fn display(value: &Value) -> String {
    match value {
        Value::Null => "unavailable".into(),
        Value::String(text) => text.clone(),
        _ => value.to_string(),
    }
}
fn pretty(value: &Value) -> String {
    if value.is_null() {
        "Unavailable at this state. Last-stop diff is not a complete trace or restorable checkpoint.".into()
    } else {
        serde_json::to_string_pretty(value).unwrap_or_else(|_| "Invalid pane data".into())
    }
}
fn window_params(memory: &str, slots: &str, source: &str, pc: &str) -> Result<Value, String> {
    let mut params = json!({});
    for (key, text, minimum, maximum) in [
        ("memoryOffset", memory, 0, 16 * 1024 * 1024u64),
        ("sourceStart", source, 1, 9_007_199_254_740_991u64),
        ("aroundPc", pc, 0, u32::MAX as u64),
    ] {
        if text.trim().is_empty() {
            continue;
        }
        let value = text
            .trim()
            .parse::<u64>()
            .map_err(|_| format!("{key} must be an exact decimal integer."))?;
        if value < minimum || value > maximum {
            return Err(format!("{key} is outside its allowed window."));
        }
        params[key] = json!(value.to_string());
    }
    if !slots.trim().is_empty() {
        let values = slots.split(',').map(str::trim).collect::<Vec<_>>();
        if values.len() > 64
            || values.iter().any(|value| {
                value.is_empty()
                    || !value.bytes().all(|b| b.is_ascii_digit())
                    || ethers::types::U256::from_dec_str(value).is_err()
            })
        {
            return Err(
                "Storage slots must be at most64 comma-separated uint256 decimal strings.".into(),
            );
        }
        params["storageSlots"] = json!(values);
    }
    Ok(params)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_window_drops_only_shared_indentation() {
        let row = |text: &str| CodeRow { position: "1".into(), text: text.into(), current: false };
        let rows = dedent(vec![row("    function f() {"), row(""), row("        x = 1;"), row("    }")]);
        let texts: Vec<_> = rows.iter().map(|row| row.text.to_string()).collect();
        assert_eq!(texts, ["function f() {", "", "    x = 1;", "}"]);
        let flush = dedent(vec![row("contract A {"), row("    uint x;")]);
        assert_eq!(flush[1].text, "    uint x;");
    }

    #[test]
    fn execution_card_and_breakpoint_follow_the_paused_status() {
        let status = json!({
            "pc": 50, "mnemonic": "SSTORE", "depth": 1, "step": 812,
            "gas": {"remaining": 29_000_000u64, "used": 1000},
            "location": {"file": "src/contracts/WorkshopVault.sol", "line": 20}
        });
        let stats: Vec<_> = execution_stats(&status)
            .into_iter()
            .map(|stat| format!("{}={}", stat.label, stat.value))
            .collect();
        assert_eq!(stats, ["PC=0x32", "Gas left=29000000", "Depth=1", "Opcode=SSTORE", "Step=812"]);
        assert_eq!(breakpoint_location(&status).as_deref(), Some("WorkshopVault.sol:20"));
        // A bytecode-only stop offers no source breakpoint; the card still renders.
        let bytecode = json!({"pc": 7, "opcode": "0x5b"});
        assert_eq!(breakpoint_location(&bytecode), None);
        assert_eq!(execution_stats(&bytecode)[3].value, "0x5b");
        assert_eq!(execution_stats(&bytecode)[1].value, "unavailable");
    }

    #[test]
    fn rendering_reads_opcode_text_and_clears_previous_session_panes() {
        i_slint_backend_testing::init_no_event_loop();
        let root = tempfile::tempdir().unwrap();
        let ui = MainWindow::new().unwrap();
        let mut state = DesktopState::new(root.path().into(), 1.0);
        let ticket = state.debug.begin_start().unwrap();
        let envelope = json!({"id":"fixture","generation":1,"revision":1,"sequence":1,"state":"paused","busy":false,"snapshot":{"status":{"stopped":true,"pc":32},"capabilities":{"source":false},"panes":{"disassembly":{"rows":[{"pc":32,"text":"PUSH1 0x20","current":true}]},"frames":{"frames":[]}}},"error":null,"result":null});
        state.debug.accept(&ticket, &envelope).unwrap();
        state.debug.select_code(1);
        render(&ui, &state);
        assert_eq!(
            ui.global::<DebugData>()
                .get_code_rows()
                .row_data(0)
                .unwrap()
                .text,
            "PUSH1 0x20"
        );
        let (_, epoch) = state.debug.cancel();
        state.debug.cancelled(epoch, Ok(()));
        state.debug.begin_start().unwrap();
        render(&ui, &state);
        assert_eq!(ui.global::<DebugData>().get_code_rows().row_count(), 0);
        assert!(ui
            .global::<DebugData>()
            .get_frames_text()
            .contains("No snapshot"));
    }

    #[test]
    fn console_summaries_keep_exact_values_and_discard_status_without_private_status_fields() {
        assert_eq!(console_evaluation(&json!({"display":"9007199254740993","type":"uint256","kept":false,"hex":"0x20000000000001"})), "uint256: 9007199254740993 (effects discarded, kept=false)");
        let (mut state, _) = fixture_state();
        state.debug.snapshot.as_mut().unwrap().status = json!({"pc":32,"stopped":true,"password":"synthetic-secret"});
        let status = local_snapshot(&state.debug, None);
        assert!(status.contains("32"));
        assert!(!status.contains("synthetic-secret"));
    }

    fn fixture_envelope(revision: u64, sequence: u64, busy: bool, result: Value) -> Value {
        json!({
            "id":"fixture", "generation":1, "revision":revision, "sequence":sequence,
            "state":if busy { "running" } else { "paused" }, "busy":busy,
            "snapshot":{
                "status":{"stopped":true,"pc":32},
                "capabilities":{"source":true,"solidity":true,"restart":true},
                "panes":{"source":{"items":[{"line":42}]},"memory":{"items":[{"offset":64}]}}
            },
            "error":null, "result":result
        })
    }

    fn fixture_state() -> (DesktopState, Client) {
        let mut state = DesktopState::new(std::path::PathBuf::from("fixture"), 1.0);
        state.active = true;
        state.workspace_ready = true;
        state.workspace_busy = false;
        let launch = state.debug.begin_start().unwrap();
        state
            .debug
            .accept(&launch, &fixture_envelope(1, 1, false, Value::Null))
            .unwrap();
        let client = client(&state);
        (state, client)
    }

    #[test]
    fn command_completion_uses_its_own_envelope_and_is_one_use() {
        use super::super::console::ConsoleStatus;
        let (mut state, client) = fixture_state();
        let entry = state.console.submit("p feeBps");
        let params = json!({"expression":"feeBps","keep":false});
        let operation = ConsoleOperation::new(
            entry,
            "evaluate",
            params.clone(),
            state.debug.envelope.as_ref().unwrap(),
        );
        let ticket = begin_console_command(&mut state.debug, "evaluate", &params).unwrap();
        assert_eq!(
            apply_delivery(
                &mut state,
                &client,
                &ticket,
                Ok(fixture_envelope(1, 2, true, Value::Null)),
                Some(&operation),
                true
            ),
            Acceptance::Accepted
        );
        assert_eq!(
            state.console.entries().back().unwrap().status,
            ConsoleStatus::Pending
        );
        let evaluation = fixture_envelope(
            2,
            3,
            false,
            json!({"evaluation":{"value":"old-evaluation"}}),
        );
        assert_eq!(
            apply_delivery(
                &mut state,
                &client,
                &ticket,
                Ok(evaluation.clone()),
                Some(&operation),
                false
            ),
            Acceptance::Accepted
        );
        assert!(state
            .console
            .entries()
            .back()
            .unwrap()
            .output
            .contains("old-evaluation"));
        apply_delivery(
            &mut state,
            &client,
            &ticket,
            Ok(evaluation),
            Some(&operation),
            false,
        );
        assert_eq!(
            state
                .console
                .entries()
                .iter()
                .filter(|row| row.id == entry)
                .count(),
            1
        );

        let entry = state.console.submit("step");
        let params = json!({"action":"step_line"});
        let operation = ConsoleOperation::new(
            entry,
            "navigate",
            params.clone(),
            state.debug.envelope.as_ref().unwrap(),
        );
        let ticket = begin_console_command(&mut state.debug, "navigate", &params).unwrap();
        apply_delivery(
            &mut state,
            &client,
            &ticket,
            Ok(fixture_envelope(2, 4, true, Value::Null)),
            Some(&operation),
            true,
        );
        apply_delivery(
            &mut state,
            &client,
            &ticket,
            Ok(fixture_envelope(3, 5, false, Value::Null)),
            Some(&operation),
            false,
        );
        let row = state.console.entries().back().unwrap();
        assert_eq!(row.status, ConsoleStatus::Ok);
        assert!(!row.output.contains("old-evaluation"));
        assert!(state
            .debug
            .result
            .as_ref()
            .unwrap()
            .to_string()
            .contains("old-evaluation"));
    }

    #[test]
    fn busy_rejection_and_cancellation_do_not_adopt_late_results() {
        use super::super::console::ConsoleStatus;
        let (mut state, client) = fixture_state();
        let first = state.console.submit("step");
        let params = json!({"action":"step_line"});
        let operation = ConsoleOperation::new(
            first,
            "navigate",
            params.clone(),
            state.debug.envelope.as_ref().unwrap(),
        );
        let ticket = begin_console_command(&mut state.debug, "navigate", &params).unwrap();
        let second = state.console.submit("next");
        let error =
            begin_console_command(&mut state.debug, "navigate", &json!({"action":"next_line"}))
                .unwrap_err();
        state.console.finish(second, Err(error));
        assert_eq!(
            state.console.entries().back().unwrap().status,
            ConsoleStatus::Error
        );
        state
            .console
            .cancel_all_pending("Worker cancellation requested.");
        state.debug.cancel();
        assert_eq!(
            apply_delivery(
                &mut state,
                &client,
                &ticket,
                Ok(fixture_envelope(2, 3, false, json!({"evaluation":"late"}))),
                Some(&operation),
                false
            ),
            Acceptance::Ignored
        );
        let row = state
            .console
            .entries()
            .iter()
            .find(|row| row.id == first)
            .unwrap();
        assert_eq!(row.status, ConsoleStatus::Cancelled);
        assert!(!row.output.contains("late"));
        assert!(local_snapshot(&state.debug, Some("source")).contains("Retained snapshot"));
    }

    #[test]
    fn old_workspace_delivery_cannot_cancel_a_new_workspace_entry() {
        use super::super::console::ConsoleStatus;
        let (mut old_state, old_client) = fixture_state();
        let old_entry = old_state.console.submit("step");
        let params = json!({"action":"step_line"});
        let operation = ConsoleOperation::new(
            old_entry,
            "navigate",
            params.clone(),
            old_state.debug.envelope.as_ref().unwrap(),
        );
        let ticket = begin_console_command(&mut old_state.debug, "navigate", &params).unwrap();
        let (mut new_state, _) = fixture_state();
        let new_entry = new_state.console.submit("step");
        assert_eq!(old_entry, new_entry);
        apply_delivery(
            &mut new_state,
            &old_client,
            &ticket,
            Err("late response".into()),
            Some(&operation),
            false,
        );
        assert_eq!(
            new_state.console.entries().back().unwrap().status,
            ConsoleStatus::Pending
        );
    }

    #[test]
    fn debug_drafts_are_allowlisted_bounded_and_blocked_during_transition() {
        let (mut state, _) = fixture_state();
        for field in DRAFT_FIELDS {
            assert!(edit_draft(&mut state, field, "value"));
            assert_eq!(draft(&state, field), "value");
        }
        assert!(!edit_draft(&mut state, "wallet-key", "not stored"));
        assert!(!edit_draft(&mut state, "run-to", &"界".repeat(1366)));
        assert_eq!(draft(&state, "run-to"), "value");
        state.workspace_busy = true;
        assert!(state.transition_pending());
        assert!(!edit_draft(&mut state, "run-to", "changed"));
        assert_eq!(draft(&state, "run-to"), "value");
    }

    #[test]
    fn pending_security_allows_only_console_cancel() {
        let (mut state, _) = fixture_state();
        state.security_confirmation = true;
        assert!(state.transition_pending());
        assert!(console_admitted(
            &state,
            &super::super::console::parse("cancel")
        ));
        assert!(!console_admitted(
            &state,
            &super::super::console::parse("status")
        ));
        assert!(!console_admitted(
            &state,
            &super::super::console::parse("step")
        ));
        assert!(!console_admitted(
            &state,
            &super::super::console::parse("cancel;step")
        ));
        state.active = false;
        assert!(!console_admitted(
            &state,
            &super::super::console::parse("cancel")
        ));
        state.active = true;
        state.workspace_ready = false;
        assert!(!console_admitted(
            &state,
            &super::super::console::parse("cancel")
        ));
    }

    #[test]
    fn render_projects_shared_debug_drafts_into_each_host() {
        i_slint_backend_testing::init_no_event_loop();
        let (mut state, _) = fixture_state();
        assert!(edit_draft(&mut state, "run-to", "Token.sol:42"));
        assert!(edit_draft(&mut state, "evaluate-input", "feeBps"));
        assert!(edit_draft(&mut state, "memory-offset", "64"));
        assert!(edit_draft(&mut state, "storage-slots", "1,2"));
        assert!(edit_draft(&mut state, "source-start", "42"));
        assert!(edit_draft(&mut state, "around-pc", "32"));
        let first = MainWindow::new().unwrap();
        let second = MainWindow::new().unwrap();
        for ui in [&first, &second] {
            render(ui, &state);
            let data = ui.global::<DebugData>();
            assert_eq!(data.get_run_to(), "Token.sol:42");
            assert_eq!(data.get_evaluate_input(), "feeBps");
            assert_eq!(data.get_memory_offset(), "64");
            assert_eq!(data.get_storage_slots(), "1,2");
            assert_eq!(data.get_source_start(), "42");
            assert_eq!(data.get_around_pc(), "32");
        }
        assert!(edit_draft(&mut state, "run-to", "Token.sol:50"));
        render(&second, &state);
        assert_eq!(second.global::<DebugData>().get_run_to(), "Token.sol:50");
        assert!(second.global::<DebugData>().get_can_command());
        state.security_confirmation = true;
        render(&second, &state);
        let data = second.global::<DebugData>();
        assert!(!data.get_can_command());
        assert!(!data.get_can_refresh());
        assert!(!data.get_restart_available());
        assert!(data.get_can_cancel());
    }

    #[test]
    fn source_and_memory_window_results_are_selected_from_current_snapshot() {
        let baseline: Envelope =
            serde_json::from_value(fixture_envelope(1, 1, false, Value::Null)).unwrap();
        let current: Envelope =
            serde_json::from_value(fixture_envelope(2, 3, false, Value::Null)).unwrap();
        let source = ConsoleOperation::new(1, "snapshot", json!({"sourceStart":"42"}), &baseline);
        let memory = ConsoleOperation::new(2, "snapshot", json!({"memoryOffset":"64"}), &baseline);
        assert!(source.completion(&current).unwrap().unwrap().contains("42"));
        assert!(memory.completion(&current).unwrap().unwrap().contains("64"));
        assert!(source.completion(&baseline).is_none());
    }

    #[test]
    fn window_quantities_are_exact_bounded_and_no_paths_or_rpc_fields_exist() {
        assert_eq!(
            window_params("32", "0,42", "1", "0").unwrap(),
            json!({"memoryOffset":"32","storageSlots":["0","42"],"sourceStart":"1","aroundPc":"0"})
        );
        assert!(window_params("-1", "", "", "").is_err());
        assert!(window_params("", "1e10", "", "").is_err());
        assert!(window_params("", "", "0", "").is_err());
    }
}
