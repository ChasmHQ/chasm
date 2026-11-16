use super::application::{self, AppError, Context};
use super::chain_read::{BlockRef, CallRequest, ReadRequest};
use super::investigation::{AnalysisRequest, Investigation};
use super::platform;
use super::{DesktopState, InvestigationData, InvestigationLogic, InvestigationRow, MainWindow};
use serde_json::{json, Value};
use slint::ComponentHandle;
use std::cell::RefCell;
use std::future::Future;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

type State = Arc<Mutex<DesktopState>>;

static NEXT_HOST_GENERATION: AtomicI32 = AtomicI32::new(1);

#[derive(Clone)]
struct ViewRef {
    weak: slint::Weak<MainWindow>,
    generation: i32,
}

impl ViewRef {
    fn new(ui: &MainWindow) -> Self {
        Self {
            weak: ui.as_weak(),
            generation: ui.global::<InvestigationData>().get_binding_generation(),
        }
    }

    fn upgrade(&self) -> Option<MainWindow> {
        self.weak.upgrade().filter(|ui| {
            ui.global::<InvestigationData>().get_binding_generation() == self.generation
        })
    }
}

#[derive(Clone, Default)]
struct Row {
    id: String,
    title: String,
    detail: String,
}

#[derive(Clone, Default)]
struct Drafts {
    query: String,
    block: String,
    call_from: String,
    call_to: String,
    call_data: String,
    call_value: String,
    call_gas: String,
    case_name: String,
    note: String,
    compare_left: String,
    compare_right: String,
    import: String,
}

impl Drafts {
    fn edit(&mut self, field: &str, value: String) -> bool {
        let target = match field {
            "query-input" => &mut self.query,
            "block-input" => &mut self.block,
            "call-from" => &mut self.call_from,
            "call-to" => &mut self.call_to,
            "call-data" => &mut self.call_data,
            "call-value" => &mut self.call_value,
            "call-gas" => &mut self.call_gas,
            "case-name" => &mut self.case_name,
            "note-input" => &mut self.note,
            "compare-left" => &mut self.compare_left,
            "compare-right" => &mut self.compare_right,
            "import-input" => &mut self.import,
            _ => return false,
        };
        *target = value;
        true
    }
}

#[derive(Clone)]
struct ViewModel {
    visible: bool,
    workspace_label: String,
    busy: bool,
    failed: bool,
    status: String,
    detail_title: String,
    output: String,
    inventory_summary: String,
    loaded_case: String,
    drafts: Drafts,
    history: Vec<Value>,
    capture_rows: Vec<Row>,
    cases: Vec<Value>,
    case_rows: Vec<Row>,
    case: Option<Value>,
    evidence_rows: Vec<Row>,
    selected_capture: i32,
    selected_case: i32,
    selected_evidence: i32,
    cases_loaded: bool,
    next_operation: u64,
    pending: Option<u64>,
    detail_revision: u64,
}

impl Default for ViewModel {
    fn default() -> Self {
        Self {
            visible: false,
            workspace_label: String::new(),
            busy: false,
            failed: false,
            status: String::new(),
            detail_title: String::new(),
            output: String::new(),
            inventory_summary: String::new(),
            loaded_case: String::new(),
            drafts: Drafts {
                block: "latest".into(),
                ..Drafts::default()
            },
            history: Vec::new(),
            capture_rows: Vec::new(),
            cases: Vec::new(),
            case_rows: Vec::new(),
            case: None,
            evidence_rows: Vec::new(),
            selected_capture: -1,
            selected_case: -1,
            selected_evidence: -1,
            cases_loaded: false,
            next_operation: 0,
            pending: None,
            detail_revision: 0,
        }
    }
}

#[derive(Clone, Copy)]
struct Operation {
    id: u64,
    detail_revision: u64,
}

struct Controller {
    state: State,
    investigation: Arc<Investigation>,
    runtime: tokio::runtime::Handle,
    model: Mutex<ViewModel>,
}

struct Registry {
    controller: Arc<Controller>,
    main: ViewRef,
    hosts: Vec<ViewRef>,
}

thread_local! {
    static HOSTS: RefCell<Option<Registry>> = const { RefCell::new(None) };
}

fn next_host_generation() -> i32 {
    NEXT_HOST_GENERATION.fetch_add(1, Ordering::Relaxed)
}

fn workspace_label(state: &DesktopState) -> String {
    if state.workspace_ready {
        format!(
            "Workspace {}, reads use its selected node",
            state.workspace.display()
        )
    } else {
        "No workspace open, chain reads need one; cases still work offline".into()
    }
}

/// Creates one Investigation controller for this desktop state. The service is
/// process-lifetime; attaching another host never rebinds its selected source.
pub fn wire(
    ui: &MainWindow,
    state: &State,
    investigation: &Arc<Investigation>,
    runtime: tokio::runtime::Handle,
) {
    let label = {
        let state = state.lock().unwrap();
        investigation.bind_source(state.workspace_ready.then(|| application::context(&state)));
        workspace_label(&state)
    };
    let mut model = ViewModel::default();
    model.workspace_label = label;
    let controller = Arc::new(Controller {
        state: state.clone(),
        investigation: investigation.clone(),
        runtime,
        model: Mutex::new(model),
    });
    ui.global::<InvestigationData>()
        .set_binding_generation(next_host_generation());
    let main = ViewRef::new(ui);
    HOSTS.with(|hosts| {
        *hosts.borrow_mut() = Some(Registry {
            controller: controller.clone(),
            main: main.clone(),
            hosts: vec![main.clone()],
        });
    });
    wire_callbacks(ui, main, controller);
    refresh_all();
}

/// Attaches a floating host to the existing controller without starting a
/// second service or rebinding the selected node.
pub fn wire_host(
    ui: &MainWindow,
    state: &State,
    investigation: &Arc<Investigation>,
    _runtime: tokio::runtime::Handle,
) {
    let controller = HOSTS.with(|hosts| {
        let hosts = hosts.borrow();
        let registry = hosts.as_ref()?;
        (registry.main.upgrade().is_some()
            && Arc::ptr_eq(&registry.controller.state, state)
            && Arc::ptr_eq(&registry.controller.investigation, investigation))
        .then(|| registry.controller.clone())
    });
    let Some(controller) = controller else { return };
    let old_generation = ui.global::<InvestigationData>().get_binding_generation();
    ui.global::<InvestigationData>()
        .set_binding_generation(next_host_generation());
    let view = ViewRef::new(ui);
    HOSTS.with(|hosts| {
        if let Some(registry) = hosts.borrow_mut().as_mut() {
            registry
                .hosts
                .retain(|host| host.generation != old_generation && host.upgrade().is_some());
            registry.hosts.push(view.clone());
        }
    });
    wire_callbacks(ui, view, controller);
    refresh_all();
}

/// Invalidates callbacks for a removed host. In-flight work belongs to the
/// controller, not to this host, and can still complete into another host.
pub fn detach_host(ui: &MainWindow) {
    let generation = ui.global::<InvestigationData>().get_binding_generation();
    HOSTS.with(|hosts| {
        if let Some(registry) = hosts.borrow_mut().as_mut() {
            registry.hosts.retain(|host| host.generation != generation);
        }
    });
    ui.global::<InvestigationData>()
        .set_binding_generation(next_host_generation());
}

fn is_current(controller: &Arc<Controller>) -> bool {
    HOSTS.with(|hosts| {
        hosts.borrow().as_ref().is_some_and(|registry| {
            Arc::ptr_eq(&registry.controller, controller) && registry.main.upgrade().is_some()
        })
    })
}

fn accepts_host(view: &ViewRef, controller: &Arc<Controller>) -> bool {
    view.upgrade().is_some() && is_current(controller)
}

/// Projects the Rust model to each still-bound host. It does not perform IO.
pub fn refresh_all() {
    refresh_except(None);
}

fn refresh_except(excluded: Option<i32>) {
    let snapshot = HOSTS.with(|hosts| {
        let mut hosts = hosts.borrow_mut();
        let registry = hosts.as_mut()?;
        registry.main.upgrade()?;
        registry.hosts.retain(|host| host.upgrade().is_some());
        Some((registry.controller.clone(), registry.hosts.clone()))
    });
    let Some((controller, hosts)) = snapshot else {
        return;
    };
    let model = controller.model.lock().unwrap().clone();
    for view in hosts {
        if excluded == Some(view.generation) {
            continue;
        }
        if let Some(ui) = view.upgrade() {
            project(&ui, &model);
        }
    }
}

fn project(ui: &MainWindow, model: &ViewModel) {
    let data = ui.global::<InvestigationData>();
    data.set_visible(model.visible);
    data.set_workspace_label(model.workspace_label.clone().into());
    data.set_busy(model.busy);
    data.set_failed(model.failed);
    data.set_status(model.status.clone().into());
    data.set_detail_title(model.detail_title.clone().into());
    data.set_output(model.output.clone().into());
    data.set_inventory_summary(model.inventory_summary.clone().into());
    data.set_case_loaded(model.case.is_some());
    data.set_loaded_case(model.loaded_case.clone().into());
    data.set_captures(project_rows(&model.capture_rows));
    data.set_cases(project_rows(&model.case_rows));
    data.set_evidence(project_rows(&model.evidence_rows));
    data.set_selected_capture(model.selected_capture);
    data.set_selected_case(model.selected_case);
    data.set_selected_evidence(model.selected_evidence);
    data.set_query_input(model.drafts.query.clone().into());
    data.set_block_input(model.drafts.block.clone().into());
    data.set_call_from(model.drafts.call_from.clone().into());
    data.set_call_to(model.drafts.call_to.clone().into());
    data.set_call_data(model.drafts.call_data.clone().into());
    data.set_call_value(model.drafts.call_value.clone().into());
    data.set_call_gas(model.drafts.call_gas.clone().into());
    data.set_case_name(model.drafts.case_name.clone().into());
    data.set_note_input(model.drafts.note.clone().into());
    data.set_compare_left(model.drafts.compare_left.clone().into());
    data.set_compare_right(model.drafts.compare_right.clone().into());
    data.set_import_input(model.drafts.import.clone().into());
}

fn project_rows(rows: &[Row]) -> slint::ModelRc<InvestigationRow> {
    super::model(
        rows.iter()
            .map(|row| InvestigationRow {
                id: row.id.clone().into(),
                title: row.title.clone().into(),
                detail: row.detail.clone().into(),
            })
            .collect(),
    )
}

fn begin(controller: &Arc<Controller>, message: impl Into<String>) -> Option<Operation> {
    if !is_current(controller) || !controller.state.lock().unwrap().active {
        return None;
    }
    let operation = {
        let mut model = controller.model.lock().unwrap();
        if model.pending.is_some() {
            return None;
        }
        model.next_operation = model.next_operation.wrapping_add(1);
        let operation = Operation {
            id: model.next_operation,
            detail_revision: model.detail_revision,
        };
        model.pending = Some(operation.id);
        model.busy = true;
        model.failed = false;
        model.status = message.into();
        operation
    };
    refresh_all();
    Some(operation)
}

fn complete(
    controller: &Arc<Controller>,
    operation: Operation,
    apply: impl FnOnce(&mut ViewModel),
) {
    if !is_current(controller) || !controller.state.lock().unwrap().active {
        return;
    }
    {
        let mut model = controller.model.lock().unwrap();
        if model.pending != Some(operation.id) {
            return;
        }
        model.pending = None;
        model.busy = false;
        apply(&mut model);
    }
    refresh_all();
}

fn fail(controller: &Arc<Controller>, message: impl Into<String>) {
    if !is_current(controller) {
        return;
    }
    {
        let mut model = controller.model.lock().unwrap();
        if model.pending.is_some() {
            return;
        }
        model.failed = true;
        model.status = message.into();
    }
    refresh_all();
}

fn available(controller: &Controller, workspace_required: bool) -> Result<(), AppError> {
    let state = controller.state.lock().unwrap();
    if state.transition_pending() {
        return Err(AppError::new(
            "busy",
            "Finish the current confirmation or operation first.",
        ));
    }
    if workspace_required && !state.workspace_ready {
        return Err(AppError::new(
            "workspace_unavailable",
            "Open a workspace before querying its selected node.",
        ));
    }
    Ok(())
}

fn require_available(controller: &Arc<Controller>, workspace_required: bool) -> bool {
    match available(controller, workspace_required) {
        Ok(()) => true,
        Err(error) => {
            fail(controller, error.message);
            false
        }
    }
}

fn wire_callbacks(ui: &MainWindow, view: ViewRef, controller: Arc<Controller>) {
    let host = view.clone();
    let held = controller.clone();
    ui.global::<InvestigationLogic>().on_open(move || {
        if !accepts_host(&host, &held) || !require_available(&held, false) {
            return;
        }
        let label = workspace_label(&held.state.lock().unwrap());
        let list = {
            let mut model = held.model.lock().unwrap();
            model.visible = true;
            model.workspace_label = label;
            !model.cases_loaded && model.pending.is_none()
        };
        refresh_all();
        let in_project = held.state.lock().unwrap().workspace_ready;
        if in_project {
            if let Some(ui) = host.upgrade() {
                ui.global::<super::AppData>().set_explorer_tab(1);
                ui.global::<super::Logic>().invoke_open_activity(1);
            }
        } else {
            super::panels::open("investigation");
        }
        if list {
            list_cases(&held);
        }
    });

    let host = view.clone();
    let held = controller.clone();
    // Shown inside the Explorer tab: same model, no separate pane.
    ui.global::<InvestigationLogic>().on_open_embedded(move || {
        if !accepts_host(&host, &held) || !require_available(&held, false) {
            return;
        }
        let label = workspace_label(&held.state.lock().unwrap());
        let list = {
            let mut model = held.model.lock().unwrap();
            model.visible = true;
            model.workspace_label = label;
            !model.cases_loaded && model.pending.is_none()
        };
        refresh_all();
        if list {
            list_cases(&held);
        }
    });

    let host = view.clone();
    let held = controller.clone();
    ui.global::<InvestigationLogic>().on_close(move || {
        if !accepts_host(&host, &held) {
            return;
        }
        held.model.lock().unwrap().visible = false;
        refresh_all();
        super::panels::hide("investigation");
    });

    let host = view.clone();
    let held = controller.clone();
    ui.global::<InvestigationLogic>()
        .on_draft_edited(move |field, value| {
            if !accepts_host(&host, &held) {
                return;
            }
            let changed = held
                .model
                .lock()
                .unwrap()
                .drafts
                .edit(field.as_str(), value.to_string());
            if changed {
                refresh_except(Some(host.generation));
            }
        });

    let host = view.clone();
    let held = controller.clone();
    ui.global::<InvestigationLogic>().on_run_account(move || {
        if !accepts_host(&host, &held) {
            return;
        }
        let (address, block) = {
            let model = held.model.lock().unwrap();
            (
                model.drafts.query.trim().to_string(),
                model.drafts.block.trim().to_string(),
            )
        };
        if !is_hex(&address, 40) {
            fail(&held, "Enter a 20-byte 0x address for an account read.");
            return;
        }
        match block_ref(&block) {
            Ok(block) => dispatch(&held, ReadRequest::Account { address, block }),
            Err(message) => fail(&held, message),
        }
    });

    let host = view.clone();
    let held = controller.clone();
    ui.global::<InvestigationLogic>()
        .on_run_transaction(move || {
            if !accepts_host(&host, &held) {
                return;
            }
            let hash = held.model.lock().unwrap().drafts.query.trim().to_string();
            if !is_hex(&hash, 64) {
                fail(&held, "Enter a 32-byte 0x transaction or block hash.");
                return;
            }
            dispatch(&held, ReadRequest::Transaction { hash });
        });

    let host = view.clone();
    let held = controller.clone();
    ui.global::<InvestigationLogic>().on_run_block(move || {
        if !accepts_host(&host, &held) {
            return;
        }
        let query = held.model.lock().unwrap().drafts.query.trim().to_string();
        match block_ref(&query) {
            Ok(selector) => dispatch(
                &held,
                ReadRequest::Block {
                    selector,
                    full_tx: true,
                },
            ),
            Err(message) => fail(&held, message),
        }
    });

    let host = view.clone();
    let held = controller.clone();
    ui.global::<InvestigationLogic>().on_run_simulate(move || {
        if !accepts_host(&host, &held) {
            return;
        }
        let (call, block) = {
            let model = held.model.lock().unwrap();
            let drafts = &model.drafts;
            (
                CallRequest {
                    from: non_empty(Some(drafts.call_from.trim().to_string())),
                    to: non_empty(Some(drafts.call_to.trim().to_string())),
                    data: non_empty(Some(drafts.call_data.trim().to_string())),
                    value: non_empty(Some(drafts.call_value.trim().to_string())),
                    gas: non_empty(Some(drafts.call_gas.trim().to_string())),
                    gas_price: None,
                },
                drafts.block.trim().to_string(),
            )
        };
        if call.to.is_none() {
            fail(
                &held,
                "Enter the contract address the call is made against.",
            );
            return;
        }
        match block_ref(&block) {
            Ok(block) => dispatch(&held, ReadRequest::Simulate { call, block }),
            Err(message) => fail(&held, message),
        }
    });

    let host = view.clone();
    let held = controller.clone();
    ui.global::<InvestigationLogic>()
        .on_refresh_history(move || {
            if accepts_host(&host, &held) {
                refresh_history(&held);
            }
        });

    let host = view.clone();
    let held = controller.clone();
    ui.global::<InvestigationLogic>()
        .on_select_capture(move |index| {
            if !accepts_host(&host, &held) {
                return;
            }
            {
                let mut model = held.model.lock().unwrap();
                let Some(capture) = get_at(&model.history, index).cloned() else {
                    return;
                };
                let id = capture["id"].as_str().unwrap_or_default();
                model.selected_capture = index;
                model.detail_revision = model.detail_revision.wrapping_add(1);
                model.detail_title = format!("Capture {id}, immutable, read from process history");
                model.output = pretty(
                    &json!({"request": capture["request"], "response": capture["response"]}),
                );
                model.failed = false;
                model.status = format!("Selected capture {id}.");
            }
            refresh_all();
        });

    let host = view.clone();
    let held = controller.clone();
    ui.global::<InvestigationLogic>().on_create_case(move || {
        if accepts_host(&host, &held) {
            create_case(&held);
        }
    });

    let host = view.clone();
    let held = controller.clone();
    ui.global::<InvestigationLogic>().on_list_cases(move || {
        if accepts_host(&host, &held) {
            list_cases(&held);
        }
    });

    let host = view.clone();
    let held = controller.clone();
    ui.global::<InvestigationLogic>()
        .on_load_case(move |index| {
            if !accepts_host(&host, &held) {
                return;
            }
            let id = {
                let model = held.model.lock().unwrap();
                get_at(&model.cases, index)
                    .and_then(|case| case["id"].as_str())
                    .unwrap_or_default()
                    .to_string()
            };
            if !id.is_empty() {
                load_case(&held, id);
            }
        });

    let host = view.clone();
    let held = controller.clone();
    ui.global::<InvestigationLogic>()
        .on_select_evidence(move |index| {
            if !accepts_host(&host, &held) {
                return;
            }
            {
                let mut model = held.model.lock().unwrap();
                let evidence = model
                    .case
                    .as_ref()
                    .and_then(|case| case["evidence"].as_array())
                    .and_then(|items| get_at(items, index))
                    .cloned();
                let Some(evidence) = evidence else { return };
                let id = evidence["id"].as_str().unwrap_or_default().to_string();
                model.selected_evidence = index;
                model.detail_revision = model.detail_revision.wrapping_add(1);
                model.detail_title = format!("Evidence {id}, immutable case record");
                model.output = pretty(&evidence);
                model.failed = false;
                model.status = format!(
                    "Selected evidence {id}. Digest proves integrity, not provider correctness."
                );
                if model.drafts.compare_left.is_empty() {
                    model.drafts.compare_left = id;
                } else if model.drafts.compare_right.is_empty() && model.drafts.compare_left != id {
                    model.drafts.compare_right = id;
                }
            }
            refresh_all();
        });

    let host = view.clone();
    let held = controller.clone();
    ui.global::<InvestigationLogic>()
        .on_save_capture_to_case(move || {
            if accepts_host(&host, &held) {
                save_capture(&held);
            }
        });

    let host = view.clone();
    let held = controller.clone();
    ui.global::<InvestigationLogic>().on_save_note(move || {
        if accepts_host(&host, &held) {
            annotate(&held, true);
        }
    });

    let host = view.clone();
    let held = controller.clone();
    ui.global::<InvestigationLogic>()
        .on_bookmark_evidence(move || {
            if accepts_host(&host, &held) {
                annotate(&held, false);
            }
        });

    let host = view.clone();
    let held = controller.clone();
    ui.global::<InvestigationLogic>()
        .on_compare_evidence(move || {
            if accepts_host(&host, &held) {
                compare(&held);
            }
        });

    let host = view.clone();
    let held = controller.clone();
    ui.global::<InvestigationLogic>().on_export_case(move || {
        if accepts_host(&host, &held) {
            export_case(&held);
        }
    });

    let host = view.clone();
    let held = controller.clone();
    ui.global::<InvestigationLogic>().on_import_case(move || {
        if accepts_host(&host, &held) {
            import_case(&held);
        }
    });

    let host = view;
    let held = controller;
    ui.global::<InvestigationLogic>()
        .on_inspect_project(move || {
            if accepts_host(&host, &held) {
                inspect_project(&held);
            }
        });
}

fn dispatch(controller: &Arc<Controller>, request: ReadRequest) {
    if !require_available(controller, true) {
        return;
    }
    let (rpc, expected) = {
        let state = controller.state.lock().unwrap();
        (state.rpc.clone(), application::context(&state))
    };
    let Some(operation) = begin(controller, "Reading the workspace node…") else {
        return;
    };
    let held = controller.clone();
    controller.runtime.spawn(async move {
        let result = match read_context(&held, &expected, &rpc) {
            Ok(()) => bounded(held.investigation.read(&rpc, expected.clone(), request)).await,
            Err(error) => Err(error),
        };
        let history = if result.is_ok() {
            held.investigation
                .execute(None, AnalysisRequest::History)
                .await
                .ok()
        } else {
            None
        };
        let _ = slint::invoke_from_event_loop(move || {
            complete_read(&held, operation, expected, rpc, result, history);
        });
    });
}

fn read_context(controller: &Controller, expected: &Context, rpc: &str) -> Result<(), AppError> {
    let state = controller.state.lock().unwrap();
    application::check_context(&state, expected)?;
    if !state.workspace_ready || state.transition_pending() || state.rpc != rpc {
        return Err(AppError::new(
            "stale_context",
            "The workspace or selected node is no longer available.",
        ));
    }
    Ok(())
}

fn complete_read(
    controller: &Arc<Controller>,
    operation: Operation,
    expected: Context,
    rpc: String,
    result: Result<Value, AppError>,
    history: Option<Value>,
) {
    let context_current = read_context(controller, &expected, &rpc).is_ok();
    complete(controller, operation, move |model| {
        if !context_current {
            model.failed = true;
            model.status =
                "The workspace or view changed during the read; any capture stays in history."
                    .into();
            return;
        }
        match result {
            Ok(value) => {
                let id = value["captureId"].as_str().unwrap_or_default();
                if model.detail_revision == operation.detail_revision {
                    model.detail_title = format!("Capture {id}, typed read result");
                    model.output = pretty(&value);
                }
                if let Some(history) = history {
                    set_history(
                        model,
                        history["captures"].as_array().cloned().unwrap_or_default(),
                    );
                }
                model.failed = false;
                model.status = format!("Capture {id} stored in history.");
            }
            Err(error) => set_error(model, error),
        }
    });
}

fn background<T, W, D>(controller: &Arc<Controller>, operation: Operation, work: W, done: D)
where
    T: Send + 'static,
    W: FnOnce(&Controller) -> Result<T, AppError> + Send + 'static,
    D: FnOnce(&mut ViewModel, Result<T, AppError>) + Send + 'static,
{
    let held = controller.clone();
    controller.runtime.spawn(async move {
        let worker = held.clone();
        let result = tokio::task::spawn_blocking(move || work(&worker))
            .await
            .unwrap_or_else(|_| Err(join_error()));
        let _ = slint::invoke_from_event_loop(move || {
            complete(&held, operation, |model| done(model, result));
        });
    });
}

fn local_query(controller: &Controller, request: AnalysisRequest) -> Result<Value, AppError> {
    controller
        .runtime
        .block_on(controller.investigation.execute(None, request))
}

fn refresh_history(controller: &Arc<Controller>) {
    if !require_available(controller, false) {
        return;
    }
    let Some(operation) = begin(controller, "Loading capture history…") else {
        return;
    };
    background(
        controller,
        operation,
        |controller| {
            available(controller, false)?;
            local_query(controller, AnalysisRequest::History)
        },
        |model, result| match result {
            Ok(value) => {
                let captures = value["captures"].as_array().cloned().unwrap_or_default();
                let count = captures.len();
                set_history(model, captures);
                model.failed = false;
                model.status = format!("{count} captures in this process (bounded last 50), not complete chain history.");
            }
            Err(error) => set_error(model, error),
        },
    );
}

fn set_history(model: &mut ViewModel, captures: Vec<Value>) {
    let selected = selected_id(&model.capture_rows, model.selected_capture);
    model.capture_rows = captures
        .iter()
        .map(|item| Row {
            id: item["id"].as_str().unwrap_or_default().to_string(),
            title: summarize(&item["request"]),
            detail: format!("capture {}", item["id"].as_str().unwrap_or_default()),
        })
        .collect();
    model.history = captures;
    model.selected_capture = row_index(&model.capture_rows, selected.as_deref());
}

fn list_cases(controller: &Arc<Controller>) {
    if !require_available(controller, false) {
        return;
    }
    let Some(operation) = begin(controller, "Listing offline cases…") else {
        return;
    };
    background(
        controller,
        operation,
        |controller| {
            available(controller, false)?;
            local_query(controller, AnalysisRequest::CaseList)
        },
        |model, result| match result {
            Ok(value) => {
                let cases = value.as_array().cloned().unwrap_or_default();
                let count = cases.len();
                set_cases(model, cases);
                model.failed = false;
                model.status = format!("{count} cases in the offline store. Click one to load it.");
            }
            Err(error) => set_error(model, error),
        },
    );
}

fn set_cases(model: &mut ViewModel, cases: Vec<Value>) {
    let selected = selected_id(&model.case_rows, model.selected_case).or_else(|| {
        model
            .case
            .as_ref()
            .and_then(|case| case["id"].as_str())
            .map(str::to_string)
    });
    model.case_rows = cases.iter().map(case_row).collect();
    model.cases = cases;
    model.cases_loaded = true;
    model.selected_case = row_index(&model.case_rows, selected.as_deref());
}

fn case_row(case: &Value) -> Row {
    Row {
        id: case["id"].as_str().unwrap_or_default().to_string(),
        title: case["name"].as_str().unwrap_or_default().to_string(),
        detail: format!(
            "{} evidence · {} notes · {} bookmarks",
            case["evidenceCount"], case["noteCount"], case["bookmarkCount"]
        ),
    }
}

fn create_case(controller: &Arc<Controller>) {
    if !require_available(controller, false) {
        return;
    }
    let draft = controller.model.lock().unwrap().drafts.case_name.clone();
    let name = draft.trim().to_string();
    if name.is_empty() {
        fail(controller, "Enter a name for the new case.");
        return;
    }
    let Some(operation) = begin(controller, "Creating case…") else {
        return;
    };
    background(
        controller,
        operation,
        move |controller| {
            available(controller, false)?;
            let value = controller.investigation.create_case(&name)?;
            Ok((value, cases_after_mutation(controller)))
        },
        move |model, result| match result {
            Ok((value, cases)) => {
                if model.drafts.case_name == draft {
                    model.drafts.case_name.clear();
                }
                if let Some(cases) = cases {
                    set_cases(model, cases);
                }
                model.failed = false;
                model.status = format!(
                    "Created case {}. Load it in the list below.",
                    value["name"].as_str().unwrap_or_default()
                );
            }
            Err(error) => set_error(model, error),
        },
    );
}

fn cases_after_mutation(controller: &Controller) -> Option<Vec<Value>> {
    available(controller, false).ok()?;
    local_query(controller, AnalysisRequest::CaseList)
        .ok()?
        .as_array()
        .cloned()
}

fn load_case(controller: &Arc<Controller>, case_id: String) {
    if !require_available(controller, false) {
        return;
    }
    let Some(operation) = begin(controller, format!("Loading case {case_id}…")) else {
        return;
    };
    background(
        controller,
        operation,
        move |controller| {
            available(controller, false)?;
            local_query(controller, AnalysisRequest::CaseLoad { id: case_id })
        },
        move |model, result| match result {
            Ok(value) => {
                let name = value["name"].as_str().unwrap_or_default().to_string();
                let count = value["evidence"].as_array().map_or(0, Vec::len);
                set_case(model, value, true, operation.detail_revision);
                model.failed = false;
                model.status = format!("Loaded case {name} with {count} evidence records.");
            }
            Err(error) => set_error(model, error),
        },
    );
}

fn set_case(model: &mut ViewModel, value: Value, show_details: bool, detail_revision: u64) {
    let case_id = value["id"].as_str().unwrap_or_default().to_string();
    let name = value["name"].as_str().unwrap_or_default().to_string();
    let same_case =
        model.case.as_ref().and_then(|case| case["id"].as_str()) == Some(case_id.as_str());
    let selected = same_case
        .then(|| selected_id(&model.evidence_rows, model.selected_evidence))
        .flatten();
    let items = value["evidence"].as_array().cloned().unwrap_or_default();
    model.evidence_rows = items
        .iter()
        .map(|item| Row {
            id: item["id"].as_str().unwrap_or_default().to_string(),
            title: format!(
                "{} {}",
                item["operation"].as_str().unwrap_or("evidence"),
                item["id"].as_str().unwrap_or_default()
            ),
            detail: format!("captured {}", item["captured"].as_str().unwrap_or_default()),
        })
        .collect();
    let count = model.evidence_rows.len();
    model.selected_evidence = row_index(&model.evidence_rows, selected.as_deref());
    model.selected_case = row_index(&model.case_rows, Some(&case_id));
    model.loaded_case = format!("{name} ({case_id}) · {count} evidence");
    if let Some(summary) = model
        .cases
        .iter_mut()
        .find(|item| item["id"].as_str() == Some(case_id.as_str()))
    {
        summary["evidenceCount"] = json!(count);
        summary["noteCount"] = json!(value["notes"].as_array().map_or(0, Vec::len));
        summary["bookmarkCount"] = json!(value["bookmarks"].as_array().map_or(0, Vec::len));
    }
    model.case_rows = model.cases.iter().map(case_row).collect();
    if show_details && model.detail_revision == detail_revision {
        model.detail_title = format!("Case {name} ({case_id})");
        model.output = pretty(&value);
    }
    model.case = Some(value);
}

fn current_case_id(model: &ViewModel) -> String {
    model
        .case
        .as_ref()
        .and_then(|case| case["id"].as_str())
        .unwrap_or_default()
        .to_string()
}

fn reload_after_mutation(controller: &Controller, case_id: &str) -> Option<Value> {
    available(controller, false).ok()?;
    local_query(
        controller,
        AnalysisRequest::CaseLoad {
            id: case_id.to_string(),
        },
    )
    .ok()
}

fn save_capture(controller: &Arc<Controller>) {
    if !require_available(controller, false) {
        return;
    }
    let (capture_id, case_id) = {
        let model = controller.model.lock().unwrap();
        (
            get_at(&model.history, model.selected_capture)
                .and_then(|capture| capture["id"].as_str())
                .unwrap_or_default()
                .to_string(),
            current_case_id(&model),
        )
    };
    if capture_id.is_empty() || case_id.is_empty() {
        fail(controller, "Select a capture and load a case first.");
        return;
    }
    let Some(operation) = begin(controller, format!("Saving capture {capture_id} to case…"))
    else {
        return;
    };
    background(
        controller,
        operation,
        move |controller| {
            available(controller, false)?;
            controller
                .investigation
                .save_capture(&case_id, &capture_id)?;
            let value = reload_after_mutation(controller, &case_id);
            Ok((case_id, value))
        },
        move |model, result| match result {
            Ok((case_id, value)) => {
                if current_case_id(model) == case_id {
                    if let Some(value) = value {
                        set_case(model, value, false, operation.detail_revision);
                    }
                }
                model.failed = false;
                model.status = "Capture saved to the case.".into();
            }
            Err(error) => set_error(model, error),
        },
    );
}

fn annotate(controller: &Arc<Controller>, note: bool) {
    if !require_available(controller, false) {
        return;
    }
    let (case_id, evidence_id, text) = {
        let model = controller.model.lock().unwrap();
        let evidence_id = model
            .case
            .as_ref()
            .and_then(|case| case["evidence"].as_array())
            .and_then(|items| get_at(items, model.selected_evidence))
            .and_then(|item| item["id"].as_str())
            .unwrap_or_default()
            .to_string();
        (
            current_case_id(&model),
            evidence_id,
            if note {
                model.drafts.note.trim().to_string()
            } else {
                String::new()
            },
        )
    };
    if case_id.is_empty() {
        fail(controller, "Load a case first.");
        return;
    }
    if evidence_id.is_empty() {
        fail(controller, "Select an evidence record first.");
        return;
    }
    let Some(operation) = begin(
        controller,
        if note {
            "Saving note…"
        } else {
            "Bookmarking evidence…"
        },
    ) else {
        return;
    };
    background(
        controller,
        operation,
        move |controller| {
            available(controller, false)?;
            controller
                .investigation
                .annotate(&case_id, &evidence_id, &text, !note)?;
            let value = reload_after_mutation(controller, &case_id);
            Ok((case_id, value))
        },
        move |model, result| match result {
            Ok((case_id, value)) => {
                if current_case_id(model) == case_id {
                    if let Some(value) = value {
                        set_case(model, value, false, operation.detail_revision);
                    }
                }
                model.failed = false;
                model.status = if note {
                    "Note saved; evidence untouched."
                } else {
                    "Evidence bookmarked."
                }
                .into();
            }
            Err(error) => set_error(model, error),
        },
    );
}

fn compare(controller: &Arc<Controller>) {
    if !require_available(controller, false) {
        return;
    }
    let (case_id, left, right) = {
        let model = controller.model.lock().unwrap();
        (
            current_case_id(&model),
            model.drafts.compare_left.trim().to_string(),
            model.drafts.compare_right.trim().to_string(),
        )
    };
    if case_id.is_empty() || left.is_empty() || right.is_empty() {
        fail(controller, "Load a case and fill both evidence ids.");
        return;
    }
    let Some(operation) = begin(controller, "Comparing evidence…") else {
        return;
    };
    background(
        controller,
        operation,
        move |controller| {
            available(controller, false)?;
            local_query(
                controller,
                AnalysisRequest::CaseCompare {
                    case_id,
                    left,
                    right,
                },
            )
        },
        move |model, result| match result {
            Ok(value) => {
                if model.detail_revision == operation.detail_revision {
                    model.detail_title = "Comparison".into();
                    model.output = pretty(&value);
                }
                model.failed = false;
                model.status = "Comparison complete.".into();
            }
            Err(error) => set_error(model, error),
        },
    );
}

fn import_case(controller: &Arc<Controller>) {
    if !require_available(controller, false) {
        return;
    }
    let text = controller.model.lock().unwrap().drafts.import.clone();
    if text.trim().is_empty() {
        fail(
            controller,
            "Paste a case JSON into the box above the button.",
        );
        return;
    }
    if text.len() > 8 * 1024 * 1024 {
        fail(controller, "The pasted case JSON is too large.");
        return;
    }
    let submitted = text.clone();
    let Some(operation) = begin(controller, "Importing the pasted case…") else {
        return;
    };
    background(
        controller,
        operation,
        move |controller| {
            available(controller, false)?;
            let value = controller.investigation.import(text.as_bytes())?;
            Ok((value, cases_after_mutation(controller)))
        },
        move |model, result| match result {
            Ok((value, cases)) => {
                if model.drafts.import == submitted {
                    model.drafts.import.clear();
                }
                if let Some(cases) = cases {
                    set_cases(model, cases);
                }
                model.failed = false;
                model.status = format!(
                    "Imported case {}.",
                    value["name"].as_str().unwrap_or_default()
                );
            }
            Err(error) => set_error(model, error),
        },
    );
}

fn acquire_picker(controller: &Arc<Controller>) -> Result<Option<std::path::PathBuf>, AppError> {
    let mut state = controller.state.lock().unwrap();
    if state.transition_pending() {
        return Err(AppError::new(
            "busy",
            "Finish the current confirmation or operation first.",
        ));
    }
    state.folder_picker_busy = true;
    Ok(state.workspace_ready.then(|| state.workspace.clone()))
}

fn picker<T, W, D>(controller: &Arc<Controller>, operation: Operation, work: W, done: D)
where
    T: Send + 'static,
    W: FnOnce(&Controller, Option<std::path::PathBuf>) -> Result<T, AppError> + Send + 'static,
    D: FnOnce(&mut ViewModel, Result<T, AppError>) + Send + 'static,
{
    let initial = match acquire_picker(controller) {
        Ok(initial) => initial,
        Err(error) => {
            complete(controller, operation, |model| set_error(model, error));
            return;
        }
    };
    super::panels::refresh_all();
    let held = controller.clone();
    controller.runtime.spawn(async move {
        let worker = held.clone();
        let result = tokio::task::spawn_blocking(move || work(&worker, initial))
            .await
            .unwrap_or_else(|_| Err(join_error()));
        let _ = slint::invoke_from_event_loop(move || {
            held.state.lock().unwrap().folder_picker_busy = false;
            if is_current(&held) {
                super::panels::refresh_all();
            }
            complete(&held, operation, |model| done(model, result));
        });
    });
}

fn export_case(controller: &Arc<Controller>) {
    if !require_available(controller, false) {
        return;
    }
    let case_id = current_case_id(&controller.model.lock().unwrap());
    if case_id.is_empty() {
        fail(controller, "Load a case to export first.");
        return;
    }
    let Some(operation) = begin(controller, "Preparing the case export…") else {
        return;
    };
    picker(
        controller,
        operation,
        move |controller, _| {
            let text = controller.investigation.export(&case_id)?;
            if !controller.state.lock().unwrap().active {
                return Err(AppError::new(
                    "stale_context",
                    "The workspace closed before the save dialog opened.",
                ));
            }
            match platform::save_text_file(&text) {
                Ok(Some(path)) => Ok(format!("Case exported to {}.", path.display())),
                Ok(None) => Err(AppError::new(
                    "cancelled",
                    "Export cancelled; no file was written.",
                )),
                Err(message) => Err(AppError::new("save_error", message)),
            }
        },
        |model, result| match result {
            Ok(message) => {
                model.failed = false;
                model.status = message;
            }
            Err(error) => set_error(model, error),
        },
    );
}

fn inspect_project(controller: &Arc<Controller>) {
    if !require_available(controller, false) {
        return;
    }
    let Some(operation) = begin(controller, "Choose a project folder in the dialog…") else {
        return;
    };
    picker(
        controller,
        operation,
        |_, initial| {
            let initial = initial.filter(|path| path.is_dir());
            let path = platform::pick_folder(initial.as_deref())
                .map_err(|message| AppError::new("picker_error", message))?
                .ok_or_else(|| AppError::new("cancelled", "Inventory cancelled."))?;
            super::cases::inspect_project(&path)
                .map_err(|message| AppError::new("inventory_error", message))
        },
        move |model, result| match result {
            Ok(value) => {
                let counts = &value["counts"];
                let truncated = if value["truncated"].as_bool().unwrap_or(false) {
                    " (truncated)"
                } else {
                    ""
                };
                model.inventory_summary = format!("Inventory of {}: {} sources, {} ABI files, {} build-info entries{}. Names and sizes only; source bodies were not read.", value["root"].as_str().unwrap_or_default(), counts["sources"], counts["abis"], counts["buildInfo"], truncated);
                if model.detail_revision == operation.detail_revision {
                    model.detail_title = "Project inventory (file names and sizes only)".into();
                    model.output = pretty(&value);
                }
                model.failed = false;
                model.status =
                    "Inventory complete. Nothing was compiled, booted or written.".into();
            }
            Err(error) if error.code == "cancelled" => {
                model.failed = false;
                model.status = error.message;
            }
            Err(error) => set_error(model, error),
        },
    );
}

fn set_error(model: &mut ViewModel, error: AppError) {
    model.failed = true;
    model.status = error.message;
}

fn get_at<T>(items: &[T], index: i32) -> Option<&T> {
    usize::try_from(index)
        .ok()
        .and_then(|index| items.get(index))
}

fn selected_id(rows: &[Row], index: i32) -> Option<String> {
    get_at(rows, index).map(|row| row.id.clone())
}

fn row_index(rows: &[Row], id: Option<&str>) -> i32 {
    id.and_then(|id| rows.iter().position(|row| row.id == id))
        .and_then(|index| i32::try_from(index).ok())
        .unwrap_or(-1)
}

async fn bounded<T>(future: impl Future<Output = Result<T, AppError>>) -> Result<T, AppError> {
    match tokio::time::timeout(Duration::from_secs(12), future).await {
        Ok(result) => result,
        Err(_) => Err(AppError::new(
            "timeout",
            "The operation exceeded its 12 second budget. Use a smaller query.",
        )),
    }
}

fn is_hex(value: &str, digits: usize) -> bool {
    value.len() == 2 + digits
        && value.starts_with("0x")
        && value[2..].chars().all(|c| c.is_ascii_hexdigit())
}

fn block_ref(value: &str) -> Result<BlockRef, String> {
    if value.is_empty() {
        return Ok(BlockRef::Latest);
    }
    BlockRef::parse(value).map_err(|error| error.message)
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.is_empty())
}

fn summarize(request: &Value) -> String {
    let op = request["op"].as_str().unwrap_or("read");
    let block = |value: &Value| {
        if value.is_string() {
            value.as_str().unwrap_or("latest").to_string()
        } else {
            value.to_string().replace('"', "")
        }
    };
    let detail = match op {
        "block" => block(&request["selector"]),
        "transaction" => request["hash"].as_str().unwrap_or_default().to_string(),
        "account" => format!(
            "{} @ {}",
            request["address"].as_str().unwrap_or_default(),
            block(&request["block"])
        ),
        "storage" => format!(
            "{} slot {} @ {}",
            request["address"].as_str().unwrap_or_default(),
            request["slot"].as_str().unwrap_or_default(),
            block(&request["block"])
        ),
        "simulate" => format!(
            "call {} @ {}",
            request["call"]["to"].as_str().unwrap_or("?"),
            block(&request["block"])
        ),
        "history" => format!(
            "blocks from {} (max {})",
            block(&request["from_block"]),
            request["count"].as_i64().unwrap_or(0)
        ),
        _ => String::new(),
    };
    if detail.is_empty() {
        op.to_string()
    } else {
        format!("{op} {detail}")
    }
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".into())
}

fn join_error() -> AppError {
    AppError::new(
        "background_task_failed",
        "The background task stopped unexpectedly.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    struct DelayedReply {
        entered: tokio::sync::oneshot::Sender<()>,
        release: tokio::sync::oneshot::Receiver<()>,
    }

    fn delayed_response() -> (
        DelayedReply,
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        (
            DelayedReply {
                entered: entered_tx,
                release: release_rx,
            },
            entered_rx,
            release_tx,
        )
    }

    async fn wait_then(reply: DelayedReply, value: Value) -> Result<Value, AppError> {
        let _ = reply.entered.send(());
        let _ = reply.release.await;
        Ok(value)
    }

    fn drive_until(runtime: &tokio::runtime::Runtime, mut ready: impl FnMut() -> bool) {
        let limit = Instant::now() + Duration::from_secs(3);
        while !ready() {
            assert!(
                Instant::now() < limit,
                "timed out waiting for delayed operation"
            );
            runtime.block_on(tokio::task::yield_now());
        }
    }

    fn setup() -> (
        tempfile::TempDir,
        MainWindow,
        MainWindow,
        State,
        Arc<Investigation>,
        tokio::runtime::Runtime,
        Arc<Controller>,
    ) {
        i_slint_backend_testing::init_no_event_loop();
        let fixture = tempfile::tempdir().unwrap();
        let state = Arc::new(Mutex::new(DesktopState::new(
            fixture.path().to_path_buf(),
            1.0,
        )));
        let investigation = Arc::new(Investigation::new(fixture.path().to_path_buf()));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let main = MainWindow::new().unwrap();
        let floating = MainWindow::new().unwrap();
        wire(&main, &state, &investigation, runtime.handle().clone());
        wire_host(&floating, &state, &investigation, runtime.handle().clone());
        let controller = HOSTS.with(|hosts| hosts.borrow().as_ref().unwrap().controller.clone());
        (
            fixture,
            main,
            floating,
            state,
            investigation,
            runtime,
            controller,
        )
    }

    #[test]
    fn detached_host_is_not_repainted_but_current_host_receives_completion() {
        let (_fixture, main, floating, state, _investigation, runtime, controller) = setup();
        let operation = begin(&controller, "Reading…").unwrap();
        let (context, rpc) = {
            let state = state.lock().unwrap();
            (application::context(&state), state.rpc.clone())
        };
        let (reply, mut entered, release) = delayed_response();
        let mut delayed = runtime.spawn(wait_then(reply, json!({"captureId":"test","result":42})));
        drive_until(&runtime, || entered.try_recv().is_ok());
        detach_host(&floating);
        floating
            .global::<InvestigationData>()
            .set_output("detached".into());
        release.send(()).unwrap();
        drive_until(&runtime, || delayed.is_finished());
        let result = runtime.block_on(&mut delayed).unwrap();
        complete_read(&controller, operation, context, rpc, result, None);
        assert!(main
            .global::<InvestigationData>()
            .get_output()
            .contains("captureId"));
        assert_eq!(
            floating.global::<InvestigationData>().get_output(),
            "detached"
        );
    }

    #[test]
    fn changed_context_discards_delayed_result_even_after_host_detaches() {
        let (_fixture, main, floating, state, _investigation, runtime, controller) = setup();
        controller.model.lock().unwrap().output = "current selection".into();
        refresh_all();
        let operation = begin(&controller, "Reading…").unwrap();
        let (context, rpc) = {
            let state = state.lock().unwrap();
            (application::context(&state), state.rpc.clone())
        };
        let (reply, mut entered, release) = delayed_response();
        let mut delayed = runtime.spawn(wait_then(reply, json!({"captureId":"stale"})));
        drive_until(&runtime, || entered.try_recv().is_ok());
        state.lock().unwrap().view_revision += 1;
        detach_host(&floating);
        release.send(()).unwrap();
        drive_until(&runtime, || delayed.is_finished());
        let result = runtime.block_on(&mut delayed).unwrap();
        complete_read(&controller, operation, context, rpc, result, None);
        assert_eq!(
            main.global::<InvestigationData>().get_output(),
            "current selection"
        );
        assert!(!main.global::<InvestigationData>().get_busy());
        assert!(main.global::<InvestigationData>().get_failed());
    }

    #[test]
    fn drafts_capture_and_selection_survive_projection_and_reattachment() {
        let (_fixture, main, floating, state, investigation, runtime, controller) = setup();
        main.global::<InvestigationLogic>()
            .invoke_draft_edited("query-input".into(), "draft query".into());
        main.global::<InvestigationLogic>()
            .invoke_draft_edited("note-input".into(), "draft note".into());
        assert_eq!(
            floating.global::<InvestigationData>().get_query_input(),
            "draft query"
        );
        assert_eq!(
            floating.global::<InvestigationData>().get_note_input(),
            "draft note"
        );
        {
            let mut model = controller.model.lock().unwrap();
            set_history(
                &mut model,
                vec![
                    json!({"id":"capture-a","request":{"op":"block","selector":"latest"},"response":{"value":7}}),
                ],
            );
            set_cases(
                &mut model,
                vec![
                    json!({"id":"case-a", "name":"Example", "evidenceCount":1, "noteCount":1, "bookmarkCount":0}),
                ],
            );
            set_case(
                &mut model,
                json!({"id":"case-a", "name":"Example", "evidence":[{"id":"evidence-a", "operation":"chain.read"}], "notes":[{"note":"saved observation"}], "bookmarks":[]}),
                false,
                0,
            );
        }
        main.global::<InvestigationLogic>().invoke_select_capture(0);
        main.global::<InvestigationLogic>()
            .invoke_select_evidence(0);
        detach_host(&floating);
        let replacement = MainWindow::new().unwrap();
        wire_host(
            &replacement,
            &state,
            &investigation,
            runtime.handle().clone(),
        );
        let data = replacement.global::<InvestigationData>();
        assert_eq!(data.get_query_input(), "draft query");
        assert_eq!(data.get_note_input(), "draft note");
        assert_eq!(data.get_selected_capture(), 0);
        assert_eq!(data.get_selected_case(), 0);
        assert_eq!(data.get_selected_evidence(), 0);
        assert!(data.get_case_loaded());
        assert_eq!(
            controller.model.lock().unwrap().case.as_ref().unwrap()["notes"][0]["note"],
            "saved observation"
        );
        assert!(data.get_output().contains("evidence-a"));
    }

    #[test]
    fn one_pending_operation_is_shared_between_hosts() {
        let (_fixture, _main, floating, state, investigation, runtime, controller) = setup();
        let executions = Arc::new(AtomicUsize::new(0));
        let first = begin(&controller, "Working…").unwrap();
        let (reply, mut entered, release) = delayed_response();
        let count = executions.clone();
        let mut delayed = runtime.spawn(async move {
            count.fetch_add(1, Ordering::SeqCst);
            wait_then(reply, json!({"result": 42})).await
        });
        drive_until(&runtime, || entered.try_recv().is_ok());
        detach_host(&floating);
        let replacement = MainWindow::new().unwrap();
        wire_host(
            &replacement,
            &state,
            &investigation,
            runtime.handle().clone(),
        );
        if begin(&controller, "Duplicate…").is_some() {
            executions.fetch_add(1, Ordering::SeqCst);
        }
        assert_eq!(executions.load(Ordering::SeqCst), 1);
        release.send(()).unwrap();
        drive_until(&runtime, || delayed.is_finished());
        assert!(runtime.block_on(&mut delayed).unwrap().is_ok());
        complete(&controller, first, |_| {});
        assert!(begin(&controller, "Next operation").is_some());
    }

    #[test]
    fn security_confirmation_blocks_local_and_chain_admission() {
        let (_fixture, _main, _floating, state, _investigation, _runtime, controller) = setup();
        state.lock().unwrap().security_confirmation = true;
        assert_eq!(available(&controller, false).unwrap_err().code, "busy");
        assert_eq!(available(&controller, true).unwrap_err().code, "busy");
    }
}
