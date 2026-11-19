mod application;
mod call_result;
mod call_trace;
mod contract_state;
mod contract_service;
mod native_contract;
mod console;
mod layout;
mod panels;
mod owned_node;
mod cases;
mod chain_read;
mod investigation;
#[cfg(test)]
mod investigation_tests;
mod control;
mod control_http;
mod native_control;
mod native_investigation;
mod converter;
#[cfg(test)]
mod copy_tests;
mod deployment;
mod request_form;
mod storage_layout;
mod trace_debug;
mod address_book;
mod notice_action;
mod native_palette;
mod toolchain;
mod templates;
mod sourcify;
mod gas_profile;
mod trace_locals;
mod captures;
mod revert_cause;
mod call_tree;
mod native_toolchain;
mod chain_tools;
mod native_chain;
mod history;
mod native_history;
mod native_trace;
mod trace_console;
mod macro_form;
pub(crate) mod debug_session;
mod debug_transport;
#[cfg(test)]
mod debug_layout_tests;
#[cfg(test)]
mod view_capture_tests;
mod native_debugger;
mod jobs;
mod macro_model;
mod macro_runner;
mod macro_backend;
mod macro_adapter;
mod native_macro;
mod macro_platform;
mod macro_executor;
mod foundry_build;
mod native_foundry;
mod native_foundry_ui;
mod mutation_gate;
mod keystore;
mod platform;
mod session;

use crate::anvil::AnvilNode;
use crate::config::ChasmConfig;
use crate::debugger::{self, Manager};
use crate::Cli;
use session::{
    cast_block_number, cast_rpc,
    cast_unlock, compile_workspace_report,
    create_keystore, establish_debugger, latest_chain, load_zoom,
    load_sidebar, save_sidebar, save_zoom, DesktopState,
};
use slint::{ComponentHandle, Model, ModelRc, VecModel, Weak};
use std::sync::{Arc, Mutex};
use jobs::Operation;

slint::include_modules!();

struct Services {
    anvil: AnvilNode,
    manager: Arc<Manager>,
    gateway_shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    gateway: tokio::task::JoinHandle<()>,
}

impl Drop for Services {
    fn drop(&mut self) {
        self.gateway.abort();
        self.anvil.stop();
    }
}

impl Services {
    async fn shutdown(mut self) -> anyhow::Result<()> {
        // Release port 8545 first: the remaining steps can wait several
        // seconds, and a user who force-closes then must not orphan the node.
        self.anvil.stop();
        self.manager.shutdown().await;
        if let Some(shutdown) = self.gateway_shutdown.take() {
            let _ = shutdown.send(());
        }
        match tokio::time::timeout(std::time::Duration::from_secs(5), &mut self.gateway).await {
            Ok(result) => result.map_err(|error| anyhow::anyhow!("Debugger server shutdown failed: {error}"))?,
            Err(_) => {
                self.gateway.abort();
                let _ = (&mut self.gateway).await;
            }
        }
        Ok(())
    }
}

pub fn show_startup_error(message: &str) {
    platform::show_error("Chasm", message);
}

pub fn launch(cli: Cli, explicit_path: bool) -> anyhow::Result<()> {
    run(cli, explicit_path)
}

fn run(mut cli: Cli, explicit_path: bool) -> anyhow::Result<()> {
    if std::env::var_os("SLINT_BACKEND").is_none() {
        std::env::set_var("SLINT_BACKEND", "winit-skia");
    }
    let ui = MainWindow::new()?;
    let control_runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let control_hub = control::Hub::new();
    let investigation_root = std::env::var_os("CHASM_CASE_DIR").map(std::path::PathBuf::from).filter(|path|path.is_absolute())
        .unwrap_or_else(||std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir).join("Chasm").join("investigations"));
    let investigation = Arc::new(investigation::Investigation::new(investigation_root));
    control_hub.set_investigation(investigation.clone());
    let control_host = control_http::ControlHost::new(control_hub.clone());
    #[cfg(debug_assertions)]
    let token_file = cli.control_token_file.clone();
    #[cfg(debug_assertions)]
    if let Some(path) = &token_file {
        let endpoint = control_runtime.block_on(control_host.enable()).map_err(|error| anyhow::anyhow!(error.message))?;
        let (_, token) = control_hub.pair_local(control::Tier::Full).map_err(|error| anyhow::anyhow!(error.message))?;
        std::fs::write(path, format!("{endpoint}\n{token}\n"))?;
    }
    let mut project = explicit_path.then(|| cli.path.clone());
    loop {
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
        let booted = if let Some(path) = project.as_ref() {
            let opening = Arc::new(Mutex::new(DesktopState::opening(path.clone())));
            clear_session_inputs(&ui);
            render(&ui, &opening.lock().unwrap());
            wire(&ui, &opening, runtime.handle().clone());
            let _control_timer = native_control::wire(&ui, &opening, &control_hub, &control_host, control_runtime.handle().clone());
            native_investigation::wire(&ui, &opening, &investigation, control_runtime.handle().clone());
            ui.show()?;
            cli.path = path.clone();
            let root = path.clone();
            let boot_cli = cli.clone();
            let weak = ui.as_weak();
            let task = runtime.spawn(async move {
                let booted = recover_startup(async {
                    match platform::validate_workspace(&root) {
                        Ok(root) => boot(boot_cli, load_zoom(&root)).await,
                        Err(error) => Err(anyhow::Error::msg(error)),
                    }
                }).await;
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.global::<AppData>().set_status_left("Project startup finished".into());
                    }
                    let _ = slint::quit_event_loop();
                });
                booted
            });
            slint::run_event_loop_until_quit()?;
            control_hub.unbind();
            Some(runtime.block_on(task).map_err(|error| anyhow::anyhow!("Project startup task failed: {error}"))?)
        } else {
            None
        };
        let (services, mut state) = match booted {
            Some(Ok((services, state))) => (Some(services), state),
            Some(Err(error)) => {
                let mut state = DesktopState::welcome();
                state.startup_screen = false;
                state.workspace = project.clone().unwrap_or_default();
                state.status_left = "Could not open project. Choose another folder or retry.".into();
                state.notice = format!("{error:#}");
                (None, state)
            }
            None => (None, DesktopState::welcome()),
        };
        state.active = true;
        let state = Arc::new(Mutex::new(state));
        clear_session_inputs(&ui);
        render(&ui, &state.lock().unwrap());
        wire(&ui, &state, runtime.handle().clone());
        let control_timer = native_control::wire(&ui, &state, &control_hub, &control_host, control_runtime.handle().clone());
        native_investigation::wire(&ui, &state, &investigation, control_runtime.handle().clone());
        panels::bind(&ui, &state, runtime.handle().clone(), investigation.clone());
        let ui_result = ui.show().and_then(|_| slint::run_event_loop_until_quit());
        control_timer.stop();
        panels::shutdown();
        native_macro::shutdown(&state);
        let foundry = native_foundry::FoundryController::for_session(&state, runtime.handle().clone());
        let foundry_stop = runtime.block_on(foundry.cancel_and_wait());
        foundry.shutdown();
        control_hub.unbind();
        investigation.bind_source(None);
        let generation=ui.global::<InvestigationData>().get_binding_generation().wrapping_add(1);
        ui.global::<InvestigationData>().set_binding_generation(generation);
        let next = {
            let mut state = state.lock().unwrap();
            state.active = false;
            state.wallet_generation = state.wallet_generation.wrapping_add(1);
            state.wallet_key = None;
            state.console.cancel_all_pending("Workspace closed.");
            if let Some(node)=&state.owned_node{node.revoke();}
            state.pending_keystore_delete = None;
            state.next_workspace.take()
        };
        let stopped = match services {
            Some(services) => runtime.block_on(services.shutdown()),
            None => Ok(()),
        };
        // Runtime drop also joins blocking compiler jobs before the next project boots.
        drop(runtime);
        ui_result?;
        if let Err(error) = foundry_stop {
            show_startup_error(&format!("Could not stop the Foundry build: {error}"));
            return Err(anyhow::Error::msg(error));
        }
        if let Err(error) = stopped {
            show_startup_error(&format!("Could not close the previous project: {error:#}"));
            return Err(error);
        }
        let Some(next) = next else { break };
        project = Some(next);
    }
    control_host.disable();
    #[cfg(debug_assertions)]
    if let Some(path) = &token_file {
        let _ = std::fs::remove_file(path);
    }
    ui.hide()?;
    Ok(())
}

fn clear_session_inputs(ui: &MainWindow) {
    let data = ui.global::<AppData>();
    data.set_wallet_password("".into());
    data.set_keystore_password("".into());
    data.set_keystore_confirm("".into());
    data.set_keystore_import("".into());
    data.set_keystore_name("".into());
    data.set_call_args("".into());
    data.set_call_value("".into());
    data.set_storage_slot("".into());
    data.set_converter_input("".into());
    data.set_explorer_input("".into());
    ui.global::<AutomationData>().set_pairing_token("".into());
    ui.global::<AutomationData>().set_visible(false);
    ui.global::<InvestigationData>().set_visible(false);
    ui.global::<InvestigationData>().set_busy(false);
    ui.global::<InvestigationData>().set_case_loaded(false);
    ui.global::<InvestigationData>().set_selected_capture(-1);
    ui.global::<InvestigationData>().set_selected_case(-1);
    ui.global::<InvestigationData>().set_selected_evidence(-1);
    ui.global::<InvestigationData>().set_import_input("".into());
    let debug=ui.global::<DebugData>();
    debug.set_workspace_tab(0);
    debug.set_show_dock(false);
    debug.set_run_to("".into());
    debug.set_evaluate_input("".into());
    debug.set_memory_offset("".into());
    debug.set_storage_slots("".into());
    debug.set_source_start("".into());
    debug.set_around_pc("".into());
    ui.set_hover_label("".into());
}

async fn recover_startup<T>(future: impl std::future::Future<Output = anyhow::Result<T>>) -> anyhow::Result<T> {
    use futures::FutureExt;
    std::panic::AssertUnwindSafe(future).catch_unwind().await
        .unwrap_or_else(|_| Err(anyhow::anyhow!("Project startup failed unexpectedly. Check dependencies and try another project.")))
}

async fn boot(cli: Cli, zoom: f32) -> anyhow::Result<(Services, DesktopState)> {
    let root = platform::validate_workspace(&cli.path).map_err(anyhow::Error::msg)?;
    // A broken chasm.toml is reported and left untouched rather than replaced.
    let (mut config, mut config_warning) = match ChasmConfig::load(&root) {
        Ok(config) => (Some(config), None),
        Err(error) => (None, Some(format!("{error}. Remappings from it are not applied; fix the file and recompile."))),
    };
    // Only for Chasm-managed builds: a Foundry project brings its own remappings.
    let foundry_build = crate::build_config::resolve(&root).is_ok_and(|effective| effective.active == crate::build_config::Source::Foundry);
    if let (Some(config), Some(oz), false) = (config.as_mut(), crate::config::scan_openzeppelin(&root), foundry_build) {
        let remapping = crate::config::generate_remapping(&oz, &root);
        if !config.remappings.iter().any(|item| item.contains("@openzeppelin")) {
            config.add_remapping(remapping);
            if let Err(error) = config.save(&root) {
                config_warning = Some(format!("Could not save .chasm/chasm.toml: {error}"));
            }
        }
    }
    let network = config.as_ref().map(|config| config.network.clone()).unwrap_or_default();
    let remappings = config.map(|config| config.remappings).unwrap_or_default();

    let mut listener = None;
    for _ in 0..75 {
        match tokio::net::TcpListener::bind("127.0.0.1:3000").await {
            Ok(bound) => {
                listener = Some(bound);
                break;
            }
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(400)).await,
        }
    }
    let listener = listener.ok_or_else(|| anyhow::anyhow!("Port 3000 stayed busy; close other Chasm instances."))?;

    let reservation = std::net::TcpListener::bind("127.0.0.1:8545")
        .map_err(|_| anyhow::anyhow!("Local Anvil port 8545 is already in use. Close the owning application before opening this project."))?;
    drop(reservation);
    let mut anvil = AnvilNode::new_silent(8545);
    if network.is_local() {
        anvil.start()?;
    } else {
        anvil.start_fork(network.fork_url.clone(), network.fork_block)?;
    }
    let probe = reqwest::Client::builder().timeout(std::time::Duration::from_millis(400)).build()?;
    let mut ready = false;
    // A fork fetches the remote head first, so it gets longer to come up.
    let attempts = if network.is_local() { 40 } else { 600 };
    for _ in 0..attempts {
        anvil.check_process()?;
        if let Ok(response) = probe.post("http://127.0.0.1:8545")
            .json(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"eth_chainId","params":[]})).send().await {
            if let Ok(body) = response.json::<serde_json::Value>().await {
                if body.get("result").and_then(serde_json::Value::as_str) == Some("0x7a69") { ready = true; break; }
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    if !ready {
        if network.is_local() {
            anyhow::bail!("Local Anvil did not become ready. Check Foundry and try opening the project again.");
        }
        anyhow::bail!("The fork of {} did not become ready within a minute. Check the RPC URL in Workspace > Network, or clear it to run a local chain.", network.fork_url);
    }
    // The node reports 31337; the forked chain's own id names it in the UI.
    let fork_chain_id = if network.is_local() {
        None
    } else {
        let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(10)).build()?;
        let body = client.post(&network.fork_url)
            .json(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"eth_chainId","params":[]}))
            .send().await.ok();
        let body = match body { Some(response) => response.json::<serde_json::Value>().await.ok(), None => None };
        body.and_then(|body| body["result"].as_str().and_then(|id| u64::from_str_radix(id.trim_start_matches("0x"), 16).ok()))
    };
    let mut debugger_config = debugger::Config::new(root.clone(), cli.debugger_python, None, cli.debugger_example);
    let build_root = root.clone();
    let build_remappings = remappings.clone();
    debugger_config.build_info = Some(Arc::new(move || {
        crate::compiler::debug_build_info(build_root.clone(), build_remappings.clone())
    }));
    let manager = Manager::new(debugger_config).await;
    let gateway_manager = manager.clone();
    let (gateway_shutdown, stop_gateway) = tokio::sync::oneshot::channel();
    let gateway = tokio::spawn(async move {
        let app = axum::Router::new().nest("/api/debugger", debugger::router(gateway_manager));
        let _ = axum::serve(listener, app).with_graceful_shutdown(async { let _ = stop_gateway.await; }).await;
    });
    let services = Services { anvil, manager, gateway_shutdown: Some(gateway_shutdown), gateway };

    let mut state = DesktopState::new(root.clone(), zoom);
    state.network = network;
    state.fork_chain_id = fork_chain_id;
    state.owned_node = services.anvil.duplicate_process_handle().ok()
        .map(|handle| owned_node::OwnedNode::from_child_handle(handle, services.anvil.port()));
    (state.sidebar_width, state.sidebar_collapsed) = load_sidebar(&root);
    state.refresh_keystores();
    let _ = &remappings;
    match compile_workspace_report(&root) {
        Ok(report) => {
            let warning = report.warning();
            state.compile_label = format!("{} contracts", report.contracts.len());
            state.storage_layouts = report.layouts;
            state.runtimes = report.runtimes;
            state.replace_compiled_contracts(report.contracts);
            if let Some(warning) = warning { state.notice = warning; }
            state.push_log("Workspace compile finished.".into());
        }
        Err(error) => {
            state.compile_label = "Compile failed".into();
            state.notice = error.clone();
            state.push_log(error);
        }
    }
    if let Some(warning) = config_warning {
        state.notice = warning.clone();
        state.push_log(warning);
    }
    let rpc = state.rpc.clone();
    state.block_number = tokio::task::spawn_blocking(move || cast_block_number(&rpc)).await.ok().and_then(Result::ok).unwrap_or(0);
    match establish_debugger(&state.client).await {
        Ok(csrf) => {
            state.csrf = csrf;
            state.debugger_label = "Debugger ready".into();
        }
        Err(error) => {
            state.debugger_label = "Debugger unavailable".into();
            state.notice = error.clone();
            state.push_log(error);
        }
    }
    state.status_left = format!("Workspace {}", root.display().to_string().trim_start_matches("\\\\?\\"));
    Ok((services, state))
}

macro_rules! on_int {
    ($ui:expr, $state:expr, $method:ident, $apply:expr) => {{
        let weak = $ui.as_weak();
        let held = $state.clone();
        $ui.global::<Logic>().$method(move |value| {
            let mut state = held.lock().unwrap();
            if state.transition_pending() { return; }
            $apply(&mut state, value);
            drop(state);
            if matches!(stringify!($method), "on_open_activity" | "on_select_contract" | "on_select_deployment" | "on_select_document" | "on_close_document") { panels::focus_current(); }
            paint(&weak, &held);
        });
    }};
}

macro_rules! on_length {
    ($ui:expr, $state:expr, $method:ident, $apply:expr) => {{
        let weak = $ui.as_weak();
        let held = $state.clone();
        $ui.global::<Logic>().$method(move |value| {
            let mut state = held.lock().unwrap();
            if state.transition_pending() { return; }
            $apply(&mut state, value);
            drop(state);
            if matches!(stringify!($method), "on_open_activity" | "on_select_contract" | "on_select_deployment" | "on_select_document" | "on_close_document") { panels::focus_current(); }
            paint(&weak, &held);
        });
    }};
}

macro_rules! on_void {
    ($ui:expr, $state:expr, $method:ident, $apply:expr) => {{
        let weak = $ui.as_weak();
        let held = $state.clone();
        $ui.global::<Logic>().$method(move || {
            let mut state = held.lock().unwrap();
            if state.transition_pending() && !matches!(stringify!($method), "on_cancel_delete_keystore" | "on_cancel_open_folder" | "on_close_menu" | "on_dismiss_notice") { return; }
            $apply(&mut state);
            drop(state);
            if matches!(stringify!($method), "on_open_wallet" | "on_open_deploy" | "on_new_macro" | "on_open_foundry_build" | "on_open_toolchain") { panels::focus_current(); }
            paint(&weak, &held);
        });
    }};
}

fn wire(ui: &MainWindow, state: &Arc<Mutex<DesktopState>>, runtime: tokio::runtime::Handle) {
    on_int!(ui, state, on_open_activity, |state: &mut DesktopState, value| state.open_activity(value));
    {
        let held = state.clone();
        ui.global::<Logic>().on_request_input_edited(move || {
            let mut state = held.lock().unwrap();
            if state.transition_pending() { return; }
            state.view_revision = state.view_revision.wrapping_add(1);
        });
    }
    on_int!(ui, state, on_select_contract, |state: &mut DesktopState, value| state.select_contract(value));
    on_int!(ui, state, on_select_deployment, |state: &mut DesktopState, value| state.select_deployment(value));
    {
        let weak = ui.as_weak();
        let held = state.clone();
        let weak_toggle = ui.as_weak();
        let held_toggle = state.clone();
        ui.global::<Logic>().on_toggle_instance(move |deployment| {
            {
                let mut state = held_toggle.lock().unwrap();
                let Some(address) = state.deployments.get(deployment as usize).map(|item| item.address.to_lowercase()) else { return };
                let crowded = state.deployments.len() > 3;
                let active = state.current_contract_view().and_then(|view| view.target.as_ref()).is_some_and(|target| target.address.to_lowercase() == address);
                let open = !state.collapsed_instances.contains(&address) && (!crowded || active || state.expanded_instances.contains(&address));
                if open {
                    state.collapsed_instances.insert(address.clone());
                    state.expanded_instances.remove(&address);
                } else {
                    state.collapsed_instances.remove(&address);
                    state.expanded_instances.insert(address);
                }
            }
            paint(&weak_toggle, &held_toggle);
        });
        ui.global::<Logic>().on_open_instance_function(move |deployment, function| {
            {
                let mut state = held.lock().unwrap();
                if state.transition_pending() || state.send_busy { return; }
                state.select_deployment(deployment);
                if function >= 0 {
                    if let Some(owner) = state.current_document_id() {
                        state.select_document_function(&owner, function);
                    }
                }
            }
            panels::focus_current();
            paint(&weak, &held);
        });
    }
    on_int!(ui, state, on_select_function, |state: &mut DesktopState, value| state.select_function(value));
    on_int!(ui, state, on_select_keystore, |state: &mut DesktopState, value| state.select_keystore(value));
    on_int!(ui, state, on_select_document, |state: &mut DesktopState, value| state.select_document(value));
    on_int!(ui, state, on_close_document, |state: &mut DesktopState, value| state.close_document(value));
    on_int!(ui, state, on_set_converter_tool, |state: &mut DesktopState, value| state.set_converter_tool(value));
    on_int!(ui, state, on_set_converter_mode, |state: &mut DesktopState, value| state.set_converter_mode(value));
    on_void!(ui, state, on_open_foundry_build, |state: &mut DesktopState| state.open_foundry_build());
    on_void!(ui, state, on_open_toolchain, |state: &mut DesktopState| state.open_toolchain());
    on_void!(ui, state, on_clear_logs, |state: &mut DesktopState| state.clear_logs());
    {
        let weak = ui.as_weak();
        let held = state.clone();
        ui.global::<Logic>().on_filter_logs(move |query| {
            held.lock().unwrap().log_filter = query.to_string();
            paint(&weak, &held);
        });
    }
    {
        let weak = ui.as_weak();
        let held = state.clone();
        ui.global::<Logic>().on_select_job(move |id| {
            let mut state = held.lock().unwrap();
            state.selected_job = id.parse::<u64>().ok().map(jobs::JobId).filter(|id| state.jobs.iter().any(|job| job.id == *id));
            drop(state);
            paint(&weak, &held);
        });
    }
    on_int!(ui, state, on_dock_tab_clicked, |state: &mut DesktopState, value: i32| state.dock_tab = value.clamp(0, 1));
    on_int!(ui, state, on_toggle_menu, |state: &mut DesktopState, value| {
        state.menu_open = if state.menu_open == value { -1 } else { value };
    });
    on_void!(ui, state, on_close_menu, |state: &mut DesktopState| state.menu_open = -1);
    on_void!(ui, state, on_toggle_logs, |state: &mut DesktopState| state.show_logs = !state.show_logs);
    on_void!(ui, state, on_open_wallet, |state: &mut DesktopState| state.open_wallet_doc());
    on_void!(ui, state, on_open_deploy, |state: &mut DesktopState| { state.open_inline_deploy(); });
    on_int!(ui, state, on_use_anvil_key, |state: &mut DesktopState, index| state.use_anvil_key(index));
    on_int!(ui, state, on_remove_deployment, |state: &mut DesktopState, index| { state.remove_deployment(index); state.deployment_menu = -1; });
    on_int!(ui, state, on_open_deployment_menu, |state: &mut DesktopState, index| state.deployment_menu = index);
    on_length!(ui, state, on_sidebar_drag_started, |state: &mut DesktopState, pointer_x| state.start_sidebar_drag(pointer_x));
    on_length!(ui, state, on_sidebar_resized, |state: &mut DesktopState, pointer_x| state.drag_sidebar(pointer_x));
    on_void!(ui, state, on_sidebar_drag_finished, |state: &mut DesktopState| {
        if !state.workspace_ready { return; }
        state.sidebar_drag = None;
        if let Err(error) = save_sidebar(&state.workspace, state.sidebar_width, state.sidebar_collapsed) { state.notice = error; }
    });
    on_int!(ui, state, on_adjust_sidebar, |state: &mut DesktopState, direction: i32| {
        if !state.workspace_ready { return; }
        state.set_sidebar_width(state.sidebar_width + direction.clamp(-1, 1) as f32 * 20.0);
        state.sidebar_collapsed = false;
        if let Err(error) = save_sidebar(&state.workspace, state.sidebar_width, false) { state.notice = error; }
    });
    on_void!(ui, state, on_reset_sidebar, |state: &mut DesktopState| {
        if !state.workspace_ready { return; }
        state.sidebar_width = 250.0;
        state.sidebar_collapsed = false;
        if let Err(error) = save_sidebar(&state.workspace, 250.0, false) { state.notice = error; }
    });
    on_void!(ui, state, on_toggle_sidebar, |state: &mut DesktopState| {
        if !state.workspace_ready { return; }
        state.sidebar_collapsed = !state.sidebar_collapsed;
        if let Err(error) = save_sidebar(&state.workspace, state.sidebar_width, state.sidebar_collapsed) { state.notice = error; }
    });
    on_void!(ui, state, on_refresh_wallet, |state: &mut DesktopState| {
        if state.can_start_wallet_operation() { state.refresh_keystores(); }
    });
    {
        let weak = ui.as_weak();
        let held = state.clone();
        let runtime_create = runtime.clone();
        ui.global::<Logic>().on_create_keystore(move || {
            let generation = match held.lock().unwrap().begin_wallet_operation() {
                Some(generation) => generation,
                None => return,
            };
            let Some(ui) = weak.upgrade() else {
                held.lock().unwrap().wallet_busy = false;
                return;
            };
            let data = ui.global::<AppData>();
            let name = data.get_keystore_name().to_string();
            let password = data.get_keystore_password().to_string();
            let confirm = data.get_keystore_confirm().to_string();
            let imported = data.get_keystore_import().to_string();
            held.lock().unwrap().status_left = "Creating keystore…".into();
            paint(&weak, &held);
            let held_task = held.clone();
            let weak_task = weak.clone();
            runtime_create.spawn(async move {
                let result = tokio::task::spawn_blocking(move || create_keystore(&name, &password, &confirm, &imported)).await;
                let _ = slint::invoke_from_event_loop(move || {
                    let mut state = held_task.lock().unwrap();
                    if !state.accepts_wallet_result(generation) { return; }
                    state.wallet_busy = false;
                    match result {
                        Ok(Ok(name)) => {
                            state.refresh_keystores();
                            state.selected_keystore = name.clone();
                            state.notice.clear();
                            state.status_left = format!("Created keystore {name}");
                            state.push_log(format!("Created keystore {name}"));
                            if let Some(ui) = weak_task.upgrade() {
                                ui.global::<AppData>().set_keystore_password("".into());
                                ui.global::<AppData>().set_keystore_confirm("".into());
                                ui.global::<AppData>().set_keystore_import("".into());
                            }
                        }
                        Ok(Err(error)) => state.notice = error,
                        Err(_) => state.notice = "Keystore creation failed.".into(),
                    }
                    drop(state);
                    paint(&weak_task, &held_task);
                });
            });
        });
    }
    on_void!(ui, state, on_about, |state: &mut DesktopState| {
        state.notice = "Chasm desktop 0.1.0. Rust binary, local only. Debugger: sibling chasm_debugger worker.".into();
    });
    on_void!(ui, state, on_dismiss_notice, |state: &mut DesktopState| state.notice.clear());
    {
        let weak = ui.as_weak();
        let held = state.clone();
        ui.global::<Logic>().on_notice_action(move |action| {
            let Some(ui) = weak.upgrade() else { return };
            let logic = ui.global::<Logic>();
            let app = ui.global::<AppData>();
            match action.as_str() {
                "toolchain" => logic.invoke_open_toolchain(),
                "recompile" => logic.invoke_recompile(),
                "timeline" => {
                    app.set_explorer_tab(3);
                    logic.invoke_open_activity(1);
                }
                "dev_account" => logic.invoke_use_anvil_key(0),
                "debugger" => logic.invoke_open_activity(4),
                "contracts" => logic.invoke_open_activity(0),
                "refresh" => logic.invoke_fetch_block(),
                "jobs" => ui.global::<PanelLogic>().invoke_dock("jobs".into(), -1),
                "reopen" => {
                    let mut state = held.lock().unwrap();
                    if state.can_change_workspace() {
                        let path = state.workspace.clone();
                        state.next_workspace = Some(path);
                        state.workspace_busy = true;
                        state.status_left = "Reopening the project…".into();
                        state.active = false;
                        drop(state);
                        let _ = slint::quit_event_loop();
                        return;
                    }
                }
                _ => return,
            }
            held.lock().unwrap().notice.clear();
            paint(&weak, &held);
        });
    }

    {
        let weak = ui.as_weak();
        let held = state.clone();
        ui.global::<Logic>().on_quit(move || request_close(&weak, &held));
        let weak = ui.as_weak();
        let held = state.clone();
        ui.window().on_close_requested(move || {
            request_close(&weak, &held);
            slint::CloseRequestResponse::KeepWindowShown
        });
    }
    wire_file_actions(ui, state, runtime.clone());
    wire_keystore_actions(ui, state, runtime.clone());
    {
        let weak = ui.as_weak();
        let held = state.clone();
        let runtime_save = runtime.clone();
        ui.global::<Logic>().on_save_output(move |text| {
            if text.is_empty() { return; }
            {
                let mut state = held.lock().unwrap();
                if state.transition_pending() || state.folder_picker_busy { return; }
                state.folder_picker_busy = true;
            }
            let weak = weak.clone();
            let held = held.clone();
            runtime_save.spawn(async move {
                let result = tokio::task::spawn_blocking(move || platform::save_text_file(text.as_str())).await;
                let _ = slint::invoke_from_event_loop(move || {
                    let mut state = held.lock().unwrap();
                    state.folder_picker_busy = false;
                    if !state.active { return; }
                    match result {
                        Ok(Ok(Some(path))) => state.status_left = format!("Saved output to {}", path.display()),
                        Ok(Ok(None)) => {},
                        Ok(Err(error)) => state.notice = error,
                        Err(_) => state.notice = "Could not save output.".into(),
                    }
                    drop(state);
                    paint(&weak, &held);
                });
            });
        });
    }

    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<Logic>().on_select_zoom(move |index| {
        let (workspace, zoom) = {
            let mut state = held.lock().unwrap();
            if !state.workspace_ready || state.transition_pending() { return; }
            if !state.set_zoom_level(index) {
                return;
            }
            state.menu_open = -1;
            state.view_revision = state.view_revision.wrapping_add(1);
            (state.workspace.clone(), state.zoom)
        };
        // Updating only the presentation properties leaves models, focus and VM state intact.
        if let Some(ui) = weak.upgrade() {
            ui.global::<Theme>().set_zoom(zoom);
            ui.global::<AppData>().set_zoom_percent((zoom * 100.0).round() as i32);
            ui.global::<AppData>().set_menu_open(-1);
        }
        if let Err(error) = save_zoom(&workspace, zoom) {
            held.lock().unwrap().notice = error.clone();
            if let Some(ui) = weak.upgrade() {
                ui.global::<AppData>().set_notice(error.into());
            }
        }
    });

    {
        let held = state.clone();
        ui.global::<Logic>().on_address_edited(move |address| {
            let mut state = held.lock().unwrap();
            state.set_contract_address(address.to_string());
        });
    }

    // New contract from a template, then recompile so it shows in the sidebar.
    {
        let weak = ui.as_weak();
        let held = state.clone();
        ui.global::<Logic>().on_new_from_template(move |kind, name| {
            let mut state = held.lock().unwrap();
            if state.transition_pending() || !state.workspace_ready { return; }
            match templates::create(&state.workspace, kind.as_str(), name.as_str()) {
                Ok(path) => {
                    let shown = path.strip_prefix(&state.workspace).unwrap_or(&path).display().to_string();
                    let openzeppelin = toolchain::used_version(&state.workspace, "openzeppelin-contracts").is_some()
                        || state.workspace.join("lib/openzeppelin-contracts").is_dir()
                        || state.workspace.join("node_modules/@openzeppelin/contracts").is_dir();
                    state.notice = if openzeppelin {
                        String::new()
                    } else {
                        format!("Created {shown}. It imports OpenZeppelin, which is not mapped in this project: missing libraries. Install and Use it in Compiler and libraries.")
                    };
                    state.status_left = format!("Created {shown}. Recompiling…");
                    drop(state);
                    if let Some(ui) = weak.upgrade() {
                        ui.global::<AppData>().set_template_name("".into());
                        ui.global::<Logic>().invoke_recompile();
                    }
                }
                Err(error) => {
                    state.notice = error;
                    drop(state);
                    paint(&weak, &held);
                }
            }
        });
    }

    // Recompile
    let weak = ui.as_weak();
    let held = state.clone();
    let runtime_compile = runtime.clone();
    ui.global::<Logic>().on_recompile(move || {
        let token = {
            let mut state = held.lock().unwrap();
            if state.transition_pending() || !state.workspace_ready { return; }
            state.compile_label = "Compiling…".into();
            state.begin_request(Operation::Compile, "compile workspace", false)
        };
        paint(&weak, &held);
        let held = held.clone();
        let weak = weak.clone();
        runtime_compile.spawn(async move {
            let root = held.lock().unwrap().workspace.clone();
            let config_warning: Option<String> = None;
            let compiled = tokio::task::spawn_blocking(move || compile_workspace_report(&root)).await;
            let _ = slint::invoke_from_event_loop(move || {
                let mut state = held.lock().unwrap();
                let compiled = compiled.unwrap_or_else(|_| Err("Compile task failed.".into()));
                let summary = compiled.as_ref().map(|report| format!("Compiled {} contracts, {} invalid artifacts skipped.", report.contracts.len(), report.skipped)).map_err(Clone::clone);
                state.finish_request(&token, &summary, None);
                if state.accepts_request(&token) {
                    match compiled {
                        Ok(report) => {
                            let warning = report.warning();
                            state.compile_label = format!("{} contracts", report.contracts.len());
                            state.notice.clear();
                            state.storage_layouts = report.layouts;
                            state.runtimes = report.runtimes;
                            state.replace_compiled_contracts(report.contracts);
                            for warning in [config_warning, warning].into_iter().flatten() {
                                if !state.notice.is_empty() { state.notice.push('\n'); }
                                state.notice.push_str(&warning);
                            }
                            state.push_log("Recompile finished.".into());
                        }
                        Err(error) => {
                            state.compile_label = "Compile failed".into();
                            // An unmapped library import is the usual cause; name the fix.
                            let missing: Vec<&str> = toolchain::missing_imports(&state.workspace).iter().map(|library| library.title).collect();
                            state.notice = if missing.is_empty() {
                                error.clone()
                            } else {
                                format!("Missing libraries: {}. Open Workspace > Compiler and libraries to install and use them.
{error}", missing.join(", "))
                            };
                            state.push_log(error);
                        }
                    }
                }
                drop(state);
                paint(&weak, &held);
            });
        });
    });

    {
        let weak = ui.as_weak();
        let held = state.clone();
        ui.global::<Logic>().on_constructor_edited(move |index, value| {
            let mut state = held.lock().unwrap();
            if state.transition_pending() || state.deploy_busy { return; }
            if state.edit_constructor_argument(index, value.to_string()) {
                state.view_revision = state.view_revision.wrapping_add(1);
                if let Some(ui) = weak.upgrade() {
                    render_deployment_form(&ui, &state);
                }
            }
        });
    }
    {
        let weak = ui.as_weak();
        let held = state.clone();
        ui.global::<Logic>().on_reset_constructor(move || {
            let mut state = held.lock().unwrap();
            if state.transition_pending() || state.deploy_busy { return; }
            if state.reset_constructor_draft() {
                state.view_revision = state.view_revision.wrapping_add(1);
                if let Some(ui) = weak.upgrade() {
                    render_deployment_form(&ui, &state);
                }
            }
        });
    }
    {
        let weak = ui.as_weak();
        let held = state.clone();
        ui.global::<Logic>().on_deployment_value_edited(move |value| {
            let mut state = held.lock().unwrap();
            if state.transition_pending() || state.deploy_busy { return; }
            state.edit_deployment_value(value.to_string());
            state.view_revision = state.view_revision.wrapping_add(1);
            if let Some(ui) = weak.upgrade() {
                render_deployment_form(&ui, &state);
            }
        });
    }

    // Validate and encode all constructor fields before starting a deployment job.
    let weak = ui.as_weak();
    let held = state.clone();
    let runtime_deploy = runtime.clone();
    ui.global::<Logic>().on_deploy_contract(move || {
        let prepared = {
            let mut state = held.lock().unwrap();
            match prepare_deployment(&state) {
                Ok(prepared) => {
                    match state.owned_node.clone().filter(|node| node.valid()) {
                        Some(owned) => {
                            let Some(lease) = state.mutation_gate.try_acquire() else {
                                let error = "Another local chain mutation is in progress. Wait for it to finish before deploying.".to_string();
                                state.notice = error.clone();
                                return;
                            };
                            state.deploy_busy = true;
                            let token = state.begin_request(Operation::Deploy, "deploy contract", false);
                            state.notice.clear();
                            state.status_left = format!("Deploying {}…", prepared.name);
                            Ok((prepared, token, owned, state.current_document_id(), lease))
                        }
                        None => {
                            let error = "The owned local Anvil node is unavailable; deployment signing is disabled.".to_string();
                            state.notice = error.clone();
                            Err(error)
                        }
                    }
                }
                Err(error) => {
                    state.notice = error.clone();
                    Err(error)
                }
            }
        };
        paint(&weak, &held);
        let Ok((prepared, token, owned, deployment_document, lease)) = prepared else { return };
        let held_task = held.clone();
        let weak_task = weak.clone();
        runtime_deploy.spawn(async move {
            let _lease = lease;
            let name = prepared.name;
            let origin = prepared.origin;
            let point = chain_tools::snapshot(&prepared.rpc).await.ok();
            // The same creation as a call, for Debug this call when the estimate reverts.
            let creation_call = serde_json::json!({
                "from": prepared.from,
                "data": format!("0x{}", ethers::utils::hex::encode(&prepared.creation)),
                "value": format!("{:#x}", prepared.value.parse::<u128>().unwrap_or(0)),
                "gas": "0x1c9c380",
            });
            let result = contract_service::deploy(prepared.rpc, prepared.key, prepared.creation, prepared.value, owned).await;
            let _ = slint::invoke_from_event_loop(move || {
                let mut state = held_task.lock().unwrap();
                if let Some((id, block)) = point {
                    let keys = state.current_deployment_keys();
                    state.push_timeline(id, block, format!("before deploy {name}"), keys);
                }
                let summary = result.as_ref().map(|result| result.simple.clone()).map_err(Clone::clone);
                let transaction_hash = result.as_ref().ok().and_then(|result| result.context.request["transactionHash"].as_str()).map(str::to_string);
                state.finish_request(&token, &summary, transaction_hash.clone());
                if !state.active || state.workspace_epoch != token.workspace_epoch || state.network_epoch != token.network_epoch || state.rpc != token.rpc { return; }
                state.deploy_busy = false;
                if let Some(hash) = transaction_hash { state.last_tx = hash; }
                match result {
                    Ok(result) => {
                        let output = result.simple;
                        let address = result.raw["result"]["contractAddress"].as_str().map(str::to_string);
                        let confirmed = result.context.request["submissionState"] == "confirmed" && result.raw["result"]["status"] == "0x1";
                        if confirmed {
                            if let Some(address) = address {
                                match state.complete_deployment(&origin, &name, &address, &token) {
                                    Ok(Some(instance)) => {
                                        state.notice.clear();
                                        state.status_left = format!("Deployed {name}");
                                        // Land on the new instance so its functions are one click away.
                                        if let Some(index) = state.documents.iter().position(|doc| doc.id == instance) {
                                            state.select_document(index as i32);
                                        }
                                    }
                                    Ok(None) => state.status_left = format!("Deployed {name}; the originating tab was closed"),
                                    Err(error) => state.notice = error,
                                }
                            } else { state.notice = "The confirmed receipt has no valid contract address. Check the transaction before retrying.".into(); }
                        } else {
                            if let Some(job) = state.jobs.iter_mut().find(|job| job.id == token.id) { job.status = if result.context.request["submissionState"] == "unknown" { "unknown" } else { "failed" }.into(); }
                            state.notice = output.clone();
                            state.status_left = format!("Deployment of {name} needs attention");
                        }
                        state.deploy_output = output.clone();
                        if let Some(doc) = state.documents.iter_mut().find(|doc| Some(&doc.id) == deployment_document.as_ref()) {
                            match doc.kind {
                                1 => doc.body = output,
                                // A tab that deployed inline and is still unbound shows why.
                                0 if doc.contract_view.target.is_none() => doc.contract_view.call_status = output,
                                _ => {}
                            }
                        }
                    }
                    Err(error) => {
                        state.deploy_output = error.clone();
                        // A deploy that reverts in its estimate is traced as a creation call.
                        if error.starts_with(contract_service::REVERTED_BEFORE_SENDING) {
                            if let Some(doc) = state.documents.iter_mut().find(|doc| Some(&doc.id) == deployment_document.as_ref() && doc.kind == 0) {
                                doc.contract_view.reverted_call = Some(creation_call.clone());
                                doc.contract_view.call_status = error.clone();
                            }
                        }
                        state.notice = error;
                        state.status_left = format!("Deployment of {name} failed");
                    }
                }
                drop(state);
                panels::focus_current();
                paint(&weak_task, &held_task);
            });
        });
    });

    // Wallet unlock
    let weak = ui.as_weak();
    let held = state.clone();
    let runtime_unlock = runtime.clone();
    ui.global::<Logic>().on_unlock_wallet(move || {
        let ui_handle = weak.upgrade();
        let (name, password) = {
            let state = held.lock().unwrap();
            let password = ui_handle.as_ref().map(|ui| ui.global::<AppData>().get_wallet_password().to_string()).unwrap_or_default();
            (state.selected_keystore.clone(), password)
        };
        if name.is_empty() {
            held.lock().unwrap().notice = "Select a keystore first.".into();
            paint(&weak, &held);
            return;
        }
        let generation = match held.lock().unwrap().begin_wallet_operation() {
            Some(generation) => generation,
            None => return,
        };
        let source_name = name.clone();
        if let Some(ui) = ui_handle { ui.global::<AppData>().set_wallet_password("".into()); }
        held.lock().unwrap().status_left = format!("Unlocking {name}…");
        paint(&weak, &held);
        let held_task = held.clone();
        let weak_task = weak.clone();
        runtime_unlock.spawn(async move {
            let result = tokio::task::spawn_blocking(move || cast_unlock(&name, &password)).await;
            let _ = slint::invoke_from_event_loop(move || {
                let mut state = held_task.lock().unwrap();
                if !state.accepts_wallet_result(generation) { return; }
                state.wallet_busy = false;
                match result {
                    Ok(Ok((key, address))) => {
                        state.wallet_source = Some(source_name);
                        state.wallet_key = Some(key);
                        state.wallet_address = address.clone();
                        state.notice.clear();
                        state.push_log(format!("Unlocked {address}"));
                        state.status_left = format!("Unlocked {address}");
                    }
                    Ok(Err(error)) => {
                        state.notice = error;
                        state.status_left = "Could not unlock keystore.".into();
                    }
                    Err(_) => state.notice = "Unlock task failed.".into(),
                }
                drop(state);
                paint(&weak_task, &held_task);
            });
        });
    });

    // Explorer
    let held = state.clone();
    let weak = ui.as_weak();
    ui.global::<Logic>().on_explorer_edited(move |input| {
        let mut state = held.lock().unwrap();
        if state.transition_pending() || input.len() > 4096 { return; }
        state.explorer_input = input.to_string();
        drop(state);
        paint(&weak, &held);
    });
    let weak = ui.as_weak();
    let held = state.clone();
    let runtime_search = runtime.clone();
    ui.global::<Logic>().on_explorer_search(move |query| {
        search_chain(weak.clone(), held.clone(), runtime_search.clone(), query.to_string());
    });
    let weak = ui.as_weak();
    let held = state.clone();
    let runtime_open = runtime.clone();
    ui.global::<Logic>().on_explorer_open(move |query| {
        if let Some(ui) = weak.upgrade() {
            ui.global::<AppData>().set_explorer_input(query.clone());
        }
        search_chain(weak.clone(), held.clone(), runtime_open.clone(), query.to_string());
    });

    // Block number
    let weak = ui.as_weak();
    let held = state.clone();
    let runtime_block = runtime.clone();
    ui.global::<Logic>().on_fetch_block(move || {
        let (rpc, token) = {
            let mut state = held.lock().unwrap();
            if state.transition_pending() || !state.workspace_ready { return; }
            let token = state.begin_request(Operation::Chain, "refresh chain", false);
            (state.rpc.clone(), token)
        };
        paint(&weak, &held);
        let state = held.clone();
        let weak = weak.clone();
        runtime_block.spawn(async move {
            let result = latest_chain(&rpc, 100).await;
            let _ = slint::invoke_from_event_loop(move || {
                let mut borrowed = state.lock().unwrap();
                let summary = result.as_ref().map(|(number, _, _)| format!("Block {number}")).map_err(Clone::clone);
                borrowed.finish_request(&token, &summary, None);
                if !borrowed.accepts_request(&token) {
                    drop(borrowed);
                    paint(&weak, &state);
                    return;
                }
                match result {
                    Ok((number, blocks, transactions)) => {
                        borrowed.block_number = number;
                        borrowed.recent_blocks = blocks;
                        borrowed.recent_transactions = transactions;
                        borrowed.status_left = format!("Block {number}");
                        borrowed.notice.clear();
                    }
                    Err(error) => borrowed.notice = error,
                }
                drop(borrowed);
                paint(&weak, &state);
            });
        });
    });

    ui.global::<Logic>().on_warp(rpc_closure(ui, state, &runtime, RpcAction::Warp));
    ui.global::<Logic>().on_roll(rpc_closure(ui, state, &runtime, RpcAction::Roll));
    ui.global::<Logic>().on_snapshot(rpc_closure(ui, state, &runtime, RpcAction::Snapshot));
    ui.global::<Logic>().on_revert(rpc_closure(ui, state, &runtime, RpcAction::Revert));

    // Converter text
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<Logic>().on_converter_edited(move |text| {
        held.lock().unwrap().convert(&text);
        paint(&weak, &held);
    });

    native_macro::wire(ui, state, runtime.clone());
    native_macro::set_platform(state, Some(Arc::new(macro_platform::NativeMacroPlatform)));
    let foundry = native_foundry_ui::wire(ui, state, runtime.clone());
    native_macro::ensure_executor(state, || {
        Arc::new(macro_executor::TrustedMacroExecutor::new(state.clone(), foundry))
    });
    native_debugger::wire(ui, state, runtime.clone());
    native_trace::wire(ui, state, runtime.clone());
    native_history::wire(ui, state, runtime.clone());
    native_chain::wire(ui, state, runtime.clone());
    native_toolchain::wire(ui, state, runtime.clone());
    native_palette::wire(ui, state, runtime.clone());
    native_contract::wire(ui, state, runtime);
}

#[derive(Clone, Copy)]
enum RpcAction {
    Warp,
    Roll,
    Snapshot,
    Revert,
}

fn rpc_closure(
    ui: &MainWindow,
    state: &Arc<Mutex<DesktopState>>,
    runtime: &tokio::runtime::Handle,
    action: RpcAction,
) -> impl Fn() + 'static {
    let weak = ui.as_weak();
    let held = state.clone();
    let runtime = runtime.clone();
    move || {
        let (rpc, snapshot_id, lease) = {
            let mut state = held.lock().unwrap();
            if state.transition_pending() || state.rpc_busy || !state.workspace_ready { return; }
            let Some(lease) = state.mutation_gate.try_acquire() else {
                state.notice = "Another local chain mutation is in progress. Wait for it to finish before changing chain state.".into();
                drop(state);
                paint(&weak, &held);
                return;
            };
            if matches!(action, RpcAction::Revert) && state.snapshot_id.is_empty() {
                state.notice = "Take a snapshot first. Revert only returns to a snapshot taken in this session.".into();
                drop(state);
                paint(&weak, &held);
                return;
            }
            state.rpc_busy = true;
            (state.rpc.clone(), state.snapshot_id.clone(), lease)
        };
        let held_task = held.clone();
        let weak_task = weak.clone();
        runtime.spawn(async move {
            let _lease = lease;
            let result = tokio::task::spawn_blocking(move || match action {
                RpcAction::Warp => cast_rpc(&rpc, "evm_increaseTime", &["3600".to_string()]),
                RpcAction::Roll => cast_rpc(&rpc, "evm_mine", &[]),
                RpcAction::Snapshot => cast_rpc(&rpc, "evm_snapshot", &[]),
                RpcAction::Revert => cast_rpc(&rpc, "evm_revert", &[snapshot_id.clone()]),
            })
            .await;
            let _ = slint::invoke_from_event_loop(move || {
                let mut state = held_task.lock().unwrap();
                state.rpc_busy = false;
                match result {
                    Ok(Ok(out)) => {
                        let out = out.trim().to_string();
                        match action {
                            RpcAction::Warp => state.status_left = "Warped chain +1 hour".into(),
                            RpcAction::Roll => state.status_left = "Mined one block".into(),
                            RpcAction::Snapshot => {
                                state.record_snapshot(out.clone());
                                state.status_left = format!("Snapshot {out}");
                            }
                            // `cast` exits successfully even when Anvil answers `false`
                            // for an unknown or already-consumed snapshot.
                            RpcAction::Revert if out != "true" => {
                                state.snapshot_id.clear();
                                state.notice = "Anvil did not revert: the snapshot is unknown or was already used. Take a new snapshot.".into();
                                state.status_left = "Revert rejected".into();
                            }
                            RpcAction::Revert => {
                                state.apply_revert();
                                state.status_left = "Reverted to snapshot".into();
                            }
                        }
                        let line = state.status_left.clone();
                        state.push_log(line);
                    }
                    Ok(Err(error)) => {
                        state.notice = error.clone();
                        state.push_log(error);
                    }
                    Err(_) => state.notice = "RPC task failed.".into(),
                }
                drop(state);
                paint(&weak_task, &held_task);
            });
        });
    }
}

struct PreparedDeployment {
    name: String,
    /// Public signer address, for replaying the creation as a call.
    from: String,
    origin: String,
    rpc: String,
    key: String,
    creation: Vec<u8>,
    value: String,
}

fn prepare_deployment(state: &DesktopState) -> Result<PreparedDeployment, String> {
    let problem = state.deployment_problem();
    if !problem.is_empty() {
        return Err(problem);
    }
    let name = state.selected_contract.clone().ok_or("Select a contract before deploying.")?;
    let contract = state.contracts.iter().find(|contract| contract.name == name)
        .ok_or("The selected contract is not available in the current build.")?;
    let draft = state.selected_deployment_draft();
    let creation = deployment::prepare_creation(&contract.bytecode, contract.constructor.as_ref(), &draft)?;
    let key = state.wallet_key.clone().filter(|key| !key.trim().is_empty())
        .ok_or("Select a signing account before deploying.")?;
    Ok(PreparedDeployment { name, from: state.wallet_address.clone(), origin: state.deployment_origin_document().unwrap_or_default(), rpc: state.rpc.clone(), key, creation, value: draft.value_wei })
}

fn deployment_presentation(state: &DesktopState, doc: &session::Document) -> DeploymentView {
    let contract = doc.contract_name().and_then(|name| {
        let mut items = state.contracts.iter().filter(|item| item.name == name);
        let first = items.next();
        if items.next().is_some() { None } else { first }
    });
    let draft = state.deployment_draft_for_document(&doc.id);
    let spec = contract.and_then(|contract| contract.constructor.as_ref());
    let validation = deployment::validate_draft(spec, &draft);
    let fields = spec.map(|spec| spec.inputs.iter().enumerate().map(|(index, param)| ConstructorFieldRow {
        name: if param.name.is_empty() { format!("Argument {}", index + 1) } else { param.name.clone() }.into(),
        kind: param.kind.to_string().into(),
        value: draft.values.get(index).cloned().unwrap_or_default().into(),
        hint: deployment::field_hint(&param.kind).into(),
        placeholder: deployment::field_placeholder(&param.kind).into(),
        error: draft.values.get(index).filter(|value| !value.is_empty()).and(validation.field_errors.get(index)).cloned().unwrap_or_default().into(),
    }).collect()).unwrap_or_default();
    let mut problem = state.deployment_problem_for_document(&doc.id);
    if problem.is_empty() && !state.owned_node.as_ref().is_some_and(|node| node.valid()) {
        problem = "The owned local Anvil node is unavailable; signing is disabled.".into();
    }
    let note = match contract {
        None => "The original contract artifact is missing or ambiguous. Recompile before deploying.".into(),
        Some(contract) if contract.constructor_error.is_some() => format!("Cannot read constructor ABI: {}", contract.constructor_error.as_deref().unwrap_or("Invalid ABI")),
        Some(_) if !validation.form_error.is_empty() => validation.form_error.clone(),
        Some(_) if spec.is_none() => "No constructor is declared. No constructor arguments are required.".into(),
        Some(_) => "Enter each argument. Arrays and tuples use positional JSON arrays.".into(),
    };
    DeploymentView {
        fields: model(fields), note: note.into(), value_error: validation.value_error.into(),
        ready: problem.is_empty(), block_reason: problem.into(), busy: state.deploy_busy,
        value: draft.value_wei.into(), output: doc.body.clone().into(),
    }
}

fn render_deployment_form(ui: &MainWindow, state: &DesktopState) {
    let data = ui.global::<AppData>();
    let contract = state.selected_contract.as_ref().and_then(|name| {
        state.contracts.iter().find(|contract| &contract.name == name)
    });
    let draft = state.selected_deployment_draft();
    let spec = contract.and_then(|contract| contract.constructor.as_ref());
    let validation = deployment::validate_draft(spec, &draft);
    let fields: Vec<ConstructorFieldRow> = spec.map(|spec| {
        spec.inputs.iter().enumerate().map(|(index, param)| ConstructorFieldRow {
            name: if param.name.is_empty() { format!("Argument {}", index + 1) } else { param.name.clone() }.into(),
            kind: param.kind.to_string().into(),
            value: draft.values.get(index).cloned().unwrap_or_default().into(),
            hint: deployment::field_hint(&param.kind).into(),
            placeholder: deployment::field_placeholder(&param.kind).into(),
            error: draft.values.get(index).filter(|value| !value.is_empty()).and(validation.field_errors.get(index)).cloned().unwrap_or_default().into(),
        }).collect()
    }).unwrap_or_default();
    let current = data.get_constructor_fields();
    let same_schema = current.row_count() == fields.len() && fields.iter().enumerate().all(|(index, field)| {
        current.row_data(index).is_some_and(|row| row.name == field.name && row.kind == field.kind)
    });
    if same_schema {
        for (index, field) in fields.into_iter().enumerate() {
            if current.row_data(index).as_ref() != Some(&field) {
                current.set_row_data(index, field);
            }
        }
    } else {
        data.set_constructor_fields(model(fields));
    }
    data.set_deploy_value(draft.value_wei.into());
    data.set_deployment_value_error(validation.value_error.into());
    data.set_constructor_note(match contract {
        None => "Select a contract to load its constructor inputs.".to_string(),
        Some(contract) if contract.constructor_error.is_some() => format!("Cannot read constructor ABI: {}", contract.constructor_error.as_deref().unwrap_or("Invalid ABI")),
        Some(_) => match spec {
            None if !draft.values.is_empty() => "The constructor ABI changed. Use Reset constructor fields to remove the previous argument draft.".to_string(),
            None => "No constructor is declared. No constructor arguments are required.".to_string(),
            Some(spec) if draft.values.len() != spec.inputs.len() => "The constructor ABI changed. Your previous draft is preserved. Use Reset constructor fields to load the current parameter list.".to_string(),
            Some(spec) if spec.inputs.is_empty() => "This constructor takes no arguments.".to_string(),
            Some(_) => "Enter each constructor argument below. Arrays and tuples use JSON arrays.".to_string(),
        },
    }.into());
    data.set_deploy_busy(state.deploy_busy);
    data.set_deploy_ready(state.deployment_can_submit());
    data.set_deploy_block_reason(state.deployment_problem().into());
}

fn wire_keystore_actions(ui: &MainWindow, state: &Arc<Mutex<DesktopState>>, runtime: tokio::runtime::Handle) {
    for reveal in [false, true] {
        let weak = ui.as_weak();
        let held = state.clone();
        let runtime = runtime.clone();
        let action = move || {
            let (name, root) = {
                let state = held.lock().unwrap();
                if state.transition_pending() || state.wallet_busy { return; }
                (state.selected_keystore.clone(), state.keystore_root.clone())
            };
            let weak = weak.clone();
            let held = held.clone();
            runtime.spawn(async move {
                let result = tokio::task::spawn_blocking(move || {
                    let store = keystore::KeystoreStore::new(root.ok_or("Could not resolve the keystore folder.")?);
                    if reveal { platform::reveal_file(&store.file_path(&name)?) }
                    else { platform::open_folder(&store.directory()?) }
                }).await;
                let _ = slint::invoke_from_event_loop(move || {
                    let mut state = held.lock().unwrap();
                    if !state.active { return; }
                    match result {
                        Ok(Ok(())) => state.status_left = "Opened keystore location in File Explorer.".into(),
                        Ok(Err(error)) => state.notice = error,
                        Err(_) => state.notice = "Could not open the keystore location.".into(),
                    }
                    drop(state);
                    paint(&weak, &held);
                });
            });
        };
        if reveal { ui.global::<Logic>().on_reveal_keystore(action); }
        else { ui.global::<Logic>().on_open_keystore_folder(action); }
    }
    let weak = ui.as_weak();
    let held = state.clone();
    let runtime_prepare = runtime.clone();
    ui.global::<Logic>().on_request_delete_keystore(move || {
        let (name, root, generation) = {
            let mut state = held.lock().unwrap();
            if state.selected_keystore.is_empty() { return; }
            let Some(generation) = state.begin_wallet_operation() else { return };
            (state.selected_keystore.clone(), state.keystore_root.clone(), generation)
        };
        paint(&weak, &held);
        let held = held.clone();
        let weak = weak.clone();
        runtime_prepare.spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                keystore::KeystoreStore::new(root.ok_or("Could not resolve the keystore folder.")?).prepare_delete(&name)
            }).await;
            let _ = slint::invoke_from_event_loop(move || {
                let mut state = held.lock().unwrap();
                if !state.accepts_wallet_result(generation) { return; }
                state.wallet_busy = false;
                match result {
                    Ok(Ok(pending)) => state.pending_keystore_delete = Some(pending),
                    Ok(Err(error)) => state.notice = error,
                    Err(_) => state.notice = "Could not prepare the keystore for deletion.".into(),
                }
                drop(state);
                paint(&weak, &held);
            });
        });
    });
    on_void!(ui, state, on_cancel_delete_keystore, |state: &mut DesktopState| {
        if !state.wallet_busy { state.pending_keystore_delete = None; }
    });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<Logic>().on_confirm_delete_keystore(move || {
        let (pending, generation) = {
            let mut state = held.lock().unwrap();
            if !state.active || state.wallet_busy || state.workspace_busy { return; }
            let Some(pending) = state.pending_keystore_delete.take() else { return };
            state.wallet_generation = state.wallet_generation.wrapping_add(1);
            state.wallet_busy = true;
            (pending, state.wallet_generation)
        };
        let name = pending.name().to_string();
        paint(&weak, &held);
        let held = held.clone();
        let weak = weak.clone();
        runtime.spawn(async move {
            let result = tokio::task::spawn_blocking(move || pending.delete()).await;
            let _ = slint::invoke_from_event_loop(move || {
                let mut state = held.lock().unwrap();
                if !state.accepts_wallet_result(generation) { return; }
                state.wallet_busy = false;
                match result {
                    Ok(Ok(())) => {
                        state.finish_keystore_delete(&name);
                        if let Some(ui) = weak.upgrade() {
                            let data = ui.global::<AppData>();
                            data.set_wallet_password("".into());
                            data.set_keystore_password("".into());
                            data.set_keystore_confirm("".into());
                            data.set_keystore_import("".into());
                        }
                    }
                    Ok(Err(error)) => state.notice = error,
                    Err(_) => state.notice = "Could not delete the keystore.".into(),
                }
                drop(state);
                paint(&weak, &held);
            });
        });
    });
}

fn request_close(weak: &Weak<MainWindow>, state: &Arc<Mutex<DesktopState>>) {
    let mut state = state.lock().unwrap();
    if !state.can_change_workspace() || state.folder_picker_busy {
        state.notice = "Wait for the current wallet or chain operation to finish before closing.".into();
        if let Some(ui) = weak.upgrade() { render(&ui, &state); }
        return;
    }
    state.pending_workspace = None;
    state.active = false;
    let _ = slint::quit_event_loop();
}

fn wire_file_actions(ui: &MainWindow, state: &Arc<Mutex<DesktopState>>, runtime: tokio::runtime::Handle) {
    let weak = ui.as_weak();
    let held = state.clone();
    let runtime_open = runtime.clone();
    ui.global::<Logic>().on_open_folder(move || {
        let initial = {
            let mut state = held.lock().unwrap();
            if !state.can_change_workspace() || state.folder_picker_busy || state.pending_workspace.is_some() {
                state.notice = "Wait for the current wallet or chain operation to finish before opening another project.".into();
                drop(state);
                paint(&weak, &held);
                return;
            }
            state.folder_picker_busy = true;
            state.menu_open = -1;
            state.workspace.is_dir().then(|| state.workspace.clone())
        };
        paint(&weak, &held);
        let weak = weak.clone();
        let held = held.clone();
        runtime_open.spawn(async move {
            let result = tokio::task::spawn_blocking(move || platform::pick_folder(initial.as_deref())).await;
            let _ = slint::invoke_from_event_loop(move || {
                let mut state = held.lock().unwrap();
                if !state.active { return; }
                state.folder_picker_busy = false;
                match result {
                    Ok(Ok(Some(path))) => match platform::validate_workspace(&path) {
                        Ok(path) => {
                            if state.accept_project_folder(path) {
                                if let Some(ui) = weak.upgrade() { render(&ui, &state); }
                                state.active = false;
                                let _ = slint::quit_event_loop();
                            }
                        }
                        Err(error) => state.notice = error,
                    },
                    Ok(Ok(None)) => {},
                    Ok(Err(error)) => state.notice = error,
                    Err(_) => state.notice = "The folder picker could not be opened.".into(),
                }
                drop(state);
                paint(&weak, &held);
            });
        });
    });
    on_void!(ui, state, on_cancel_open_folder, |state: &mut DesktopState| state.pending_workspace = None);
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<Logic>().on_confirm_open_folder(move || {
        let mut state = held.lock().unwrap();
        if !state.can_change_workspace() { return; }
        let Some(path) = state.pending_workspace.as_ref() else { return };
        match platform::validate_workspace(path) {
            Ok(path) => {
                state.pending_workspace = None;
                state.next_workspace = Some(path);
                state.workspace_busy = true;
                state.wallet_generation = state.wallet_generation.wrapping_add(1);
                state.status_left = "Closing the current project…".into();
                if let Some(ui) = weak.upgrade() { render(&ui, &state); }
                state.active = false;
                let _ = slint::quit_event_loop();
            }
            Err(error) => {
                state.pending_workspace = None;
                state.notice = error;
                if let Some(ui) = weak.upgrade() { render(&ui, &state); }
            }
        }
    });
}

fn search_chain(weak: Weak<MainWindow>, state: Arc<Mutex<DesktopState>>, runtime: tokio::runtime::Handle, query: String) {
    let (rpc, token) = {
        let mut state = state.lock().unwrap();
        if state.transition_pending() || !state.workspace_ready { return; }
        state.explorer_input = query.clone();
        let token = state.begin_request(Operation::Explorer, "explorer lookup", false);
        (state.rpc.clone(), token)
    };
    paint(&weak, &state);
    let held = state.clone();
    runtime.spawn(async move {
        // Looks the item up on the node directly, so anything the node still has
        // is found even when it is older than the listed 100.
        let result = session::lookup_chain(&rpc, query.trim()).await;
        let _ = slint::invoke_from_event_loop(move || {
            let mut state = held.lock().unwrap();
            state.finish_request(&token, &result, None);
            if state.accepts_request(&token) {
                match result {
                    Ok(text) => {
                        state.explorer_result = text;
                        state.notice.clear();
                        state.push_log(format!("Explorer lookup {query}"));
                        state.status_left = format!("Looked up {query}");
                    }
                    Err(error) => {
                        state.explorer_result = error.clone();
                        state.notice = error;
                    }
                }
            }
            drop(state);
            paint(&weak, &held);
        });
    });
}

/// Host part of an RPC URL, without credentials or path (keys often sit in the path).
pub(super) fn fork_host(url: &str) -> String {
    reqwest::Url::parse(url).ok().and_then(|url| url.host_str().map(str::to_string)).unwrap_or_else(|| "remote".into())
}

/// Test hook: compile a folder as the desktop would; returns the contract count.
#[cfg(test)]
pub fn compile_workspace_probe(root: &std::path::Path) -> Result<usize, String> {
    compile_workspace_report(root).map(|report| report.contracts.len())
}

fn paint(weak: &Weak<MainWindow>, state: &Arc<Mutex<DesktopState>>) {
    {
        let state = state.lock().unwrap();
        if !state.active || state.workspace_busy { return; }
        if let Some(ui) = weak.upgrade() {render(&ui, &state);}
    }
    panels::refresh_all();
}

fn render(ui: &MainWindow, state: &DesktopState) {
    ui.global::<Theme>().set_zoom(state.zoom);
    let data = ui.global::<AppData>();
    data.set_zoom_percent((state.zoom * 100.0).round() as i32);
    data.set_activity(state.activity);
    data.set_workspace_path(state.workspace.display().to_string().trim_start_matches("\\\\?\\").into());
    data.set_workspace_ready(state.workspace_ready);
    data.set_startup_screen(state.startup_screen);
    data.set_project_opening(state.project_opening);
    data.set_workspace_busy(state.workspace_busy);
    data.set_folder_picker_busy(state.folder_picker_busy);
    data.set_pending_workspace(state.pending_workspace.as_ref().map(|path| path.display().to_string()).unwrap_or_default().into());
    data.set_wallet_busy(state.wallet_busy);
    data.set_keystore_error(state.keystore_error.clone().into());
    data.set_keystore_folder(state.keystore_root.as_ref().map(|path| path.display().to_string()).unwrap_or_default().into());
    data.set_pending_keystore_delete(state.pending_keystore_delete.as_ref().map(|pending| pending.name().to_string()).unwrap_or_default().into());
    data.set_pending_keystore_path(state.pending_keystore_delete.as_ref().map(|pending| pending.path().display().to_string()).unwrap_or_default().into());
    data.set_sidebar_width(state.sidebar_width);
    data.set_sidebar_collapsed(state.sidebar_collapsed);
    data.set_call_status(state.operation_status(Operation::Call).into());
    data.set_storage_status(state.operation_status(Operation::Storage).into());
    data.set_explorer_status(state.operation_status(Operation::Explorer).into());
    data.set_job_output(state.selected_job_output().into());
    if data.get_converter_input().as_str() != state.converter_input { data.set_converter_input(state.converter_input.clone().into()); }
    if data.get_explorer_input().as_str() != state.explorer_input { data.set_explorer_input(state.explorer_input.clone().into()); }
    data.set_converter_tool(state.converter_tool);
    data.set_converter_mode(state.converter_mode);
    data.set_converter_modes(model(converter::modes(state.converter_tool).into_iter().map(slint::SharedString::from).collect()));
    data.set_converter_hint(converter::hint(state.converter_tool, state.converter_mode).into());
    data.set_converter_error(state.converter_error.clone().into());
    data.set_menu_open(state.menu_open);
    data.set_deployment_menu(state.deployment_menu);
    data.set_mode_label(if state.network.is_local() {
        "Local".into()
    } else {
        let chain = state.fork_chain_id.map(toolchain::chain_name).unwrap_or_else(|| fork_host(&state.network.fork_url));
        let block = state.network.fork_block.map(|block| format!(" @ {block}")).unwrap_or_default();
        format!("Fork of {chain}{block}").into()
    });
    // Signing always uses the local id, so nothing is valid on the real chain.
    data.set_chain_label(if state.network.is_local() { "31337" } else { "signs as 31337" }.into());
    data.set_block_label(format!("block {}", state.block_number).into());
    data.set_compile_label(state.compile_label.clone().into());
    data.set_debugger_label(state.debugger_label.clone().into());
    data.set_debugger_tint(if state.debugger_label.contains("ready") {
        brush(0x89, 0xb4, 0x82)
    } else if state.debugger_label.contains("unavailable") || state.debugger_label.contains("failed") {
        brush(0xf0, 0x80, 0x80)
    } else {
        brush(0xa6, 0xa6, 0xa6)
    });
    data.set_status_left(state.status_left.clone().into());
    data.set_heading(state.heading().into());
    data.set_notice(state.notice.clone().into());
    let (label, action) = notice_action::suggest(&state.notice).unwrap_or(("", ""));
    data.set_notice_action_label(label.into());
    data.set_notice_action(action.into());
    data.set_show_logs(state.show_logs);
    data.set_dock_tab(state.dock_tab);
    data.set_doc_kind(state.current_doc_kind());
    data.set_selected_contract(state.selected_contract.clone().unwrap_or_default().into());
    data.set_selected_function(state.selected_function);
    data.set_selected_keystore(state.selected_keystore.clone().into());
    data.set_wallet_address(state.wallet_address.clone().into());
    data.set_deploy_output(state.deploy_output.clone().into());
    data.set_call_output(state.call_output.clone().into());
    data.set_storage_output(state.storage_output.clone().into());
    data.set_converter_output(state.converter_output.clone().into());
    let labels = address_book::labels(state);
    let selectors = address_book::selectors(state);
    let mut explorer = address_book::annotate(&state.explorer_result, &labels);
    // Name the selector line of a transaction lookup.
    if let Some(line) = explorer.lines().find(|line| line.starts_with("Selector  0x")).map(str::to_string) {
        if let Some(name) = selectors.get(line.trim_start_matches("Selector  ").trim()) {
            explorer = explorer.replacen(&line, &format!("{line}  {name}"), 1);
        }
    }
    data.set_explorer_result(explorer.into());
    data.set_recent_blocks(model(state.recent_blocks.iter().map(|item| ChainItem {
        title: item.title.clone().into(),
        detail: item.detail.clone().into(),
        query: item.query.clone().into(),
    }).collect()));
    data.set_recent_transactions(model(state.recent_transactions.iter().map(|item| ChainItem {
        title: item.title.clone().into(),
        detail: address_book::transaction_detail(item, &labels, &selectors).into(),
        query: item.query.clone().into(),
    }).collect()));
    data.set_document_body(state.document_body().into());
    data.set_debugger_live(state.debugger_live);
    native_macro::render(ui, state);
    native_macro::project_call(ui, state);
    native_debugger::render(ui, state);
    native_trace::render(ui, state);
    native_history::render(ui, state);
    native_chain::render(ui, state);
    native_toolchain::render(ui, state);
    native_palette::render(ui, state);
    native_contract::render(ui, state);
    data.set_active_document(state.active_document);
    render_deployment_form(ui, state);

    // Inputs that host may override (never the text the user is typing, except
    // address after a deploy, which is intentionally host-authored).
    data.set_address_input(state.address_input.clone().into());

    let (name, params) = state.selected_function_detail();
    data.set_selected_name(name.into());
    data.set_function_inputs(model(params.iter().map(|(param_name, kind)| InputRow { name: param_name.clone().into(), kind: kind.clone().into() }).collect()));

    data.set_contracts(model(state.visible_contracts().into_iter().map(|item| ContractRow { name: item.name.into(), kind: item.kind.into() }).collect()));
    data.set_functions(model(state.selected_contract_functions().iter().map(|item| FunctionRow {
        name: item.name.clone().into(),
        inputs: item.inputs.clone().into(),
        mutability: item.mutability.clone().into(),
    }).collect()));
    data.set_deployments(model(state.deployments.iter().map(|item| DeploymentRow { name: item.name.clone().into(), address: item.address.clone().into() }).collect()));
    data.set_instance_rows(model(instance_rows(state)));
    data.set_keystores(model(state.keystores.iter().map(|item| TextRow { text: item.clone().into() }).collect()));
    data.set_documents(model(state.documents.iter().map(|doc| DocTab { title: doc.title.clone().into(), kind: doc.kind }).collect()));
    let filter = state.log_filter.to_lowercase();
    let filtered_logs: Vec<_> = state.logs.iter().filter(|line| line.to_lowercase().contains(&filter)).collect();
    let log_text = filtered_logs.iter().map(|line| line.as_str()).collect::<Vec<_>>().join("\n");
    if data.get_log_text().as_str() != log_text { data.set_log_text(log_text.into()); }
    data.set_logs(model(filtered_logs.into_iter().map(|line| LogLine { text: line.clone().into() }).collect()));
    data.set_jobs(model(state.jobs.iter().map(|job| JobRow { id: job.id.0.to_string().into(), name: format!("#{} {}", job.id.0, job.name).into(), status: job.status.clone().into() }).collect()));
}

/// Deployments with their functions inlined, so a call is one click from the sidebar.
fn instance_rows(state: &DesktopState) -> Vec<InstanceRow> {
    let current = state.current_contract_view().and_then(|view| view.target.as_ref().map(|target| (target.address.to_lowercase(), view.selected_function)));
    let mut rows = Vec::new();
    // With many deployments only the instance in use stays open by default.
    let crowded = state.deployments.len() > 3;
    for (index, deployment) in state.deployments.iter().enumerate() {
        let address = deployment.address.to_lowercase();
        let active = current.as_ref().filter(|(target, _)| *target == address).map(|(_, function)| *function);
        let mut contracts = state.contracts.iter().filter(|item| item.name == deployment.name);
        let contract = contracts.next().filter(|_| contracts.next().is_none());
        let expanded = !state.collapsed_instances.contains(&address)
            && (!crowded || active.is_some() || state.expanded_instances.contains(&address));
        rows.push(InstanceRow {
            deployment: index as i32,
            function: -1,
            label: deployment.name.clone().into(),
            tag: "".into(),
            address: short_address(&deployment.address).into(),
            selected: active == Some(-1),
            expanded,
            count: contract.map(|contract| contract.functions.len() as i32).unwrap_or(0),
        });
        let Some(contract) = contract.filter(|_| expanded) else { continue };
        for (position, function) in contract.functions.iter().enumerate() {
            rows.push(InstanceRow {
                deployment: index as i32,
                function: position as i32,
                label: function.name.clone().into(),
                tag: match function.mutability.as_str() {
                    "view" | "pure" => "VIEW",
                    "payable" => "PAY",
                    _ => "TX",
                }
                .into(),
                address: "".into(),
                selected: active == Some(position as i32),
                expanded: true,
                count: 0,
            });
        }
    }
    rows
}

fn short_address(address: &str) -> String {
    if address.len() > 12 { format!("{}…{}", &address[..6], &address[address.len() - 4..]) } else { address.to_string() }
}

fn brush(r: u8, g: u8, b: u8) -> slint::Brush {
    slint::Brush::from(slint::Color::from_rgb_u8(r, g, b))
}

fn model<T: Clone + 'static>(items: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(items))
}

/// A fresh model makes Slint rebuild every row element, which drops keyboard
/// focus from a LineEdit after each keystroke. Keep the shown model when every
/// row has the same identity (`same`) and update only the rows that differ.
fn keep_rows<T: Clone + PartialEq + 'static>(shown: ModelRc<T>, next: ModelRc<T>, same: impl Fn(&T, &T) -> bool) -> ModelRc<T> {
    let count = next.row_count();
    let stable = count > 0
        && shown.row_count() == count
        && (0..count).all(|index| matches!((shown.row_data(index), next.row_data(index)), (Some(old), Some(new)) if same(&old, &new)));
    if !stable {
        return next;
    }
    for index in 0..count {
        let row = next.row_data(index).unwrap();
        if shown.row_data(index).as_ref() != Some(&row) {
            shown.set_row_data(index, row);
        }
    }
    shown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_owners_share_controllers_without_retargeting_or_starting_unowned_jobs() {
        i_slint_backend_testing::init_no_event_loop();
        let root = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let raw = serde_json::json!({"type":"compile_success","contracts":[
            {"name":"BankA","artifact":{"abi":[{"type":"function","name":"writeA","inputs":[{"name":"amount","type":"uint256"}],"outputs":[],"stateMutability":"nonpayable"}]}},
            {"name":"BankB","artifact":{"abi":[{"type":"function","name":"readB","inputs":[],"outputs":[{"name":"value","type":"bool"}],"stateMutability":"view"}]}}
        ]});
        let mut initial = DesktopState::new(root.path().into(), 1.0);
        initial.contracts = session::contracts_from_compile(&raw.to_string()).unwrap();
        initial.select_contract(0);
        let first = initial.current_document_id().unwrap();
        initial.set_contract_address("0x1111111111111111111111111111111111111111".into());
        initial.select_document_function(&first, 0);
        initial.select_contract(1);
        let second = initial.current_document_id().unwrap();
        initial.set_contract_address("0x2222222222222222222222222222222222222222".into());
        initial.select_document_function(&second, 0);
        initial.wallet_key = Some("synthetic-not-used".into());
        let state = Arc::new(Mutex::new(initial));
        let main = MainWindow::new().unwrap();
        let floating = MainWindow::new().unwrap();
        floating.set_panel_host(true);
        floating.set_host_pane(1);
        floating.set_host_owner(first.clone().into());
        wire(&main, &state, runtime.handle().clone());
        wire(&floating, &state, runtime.handle().clone());
        floating.global::<ContractLogic>().invoke_arguments_edited(first.clone().into(), "17".into());
        floating.global::<ContractLogic>().invoke_section(first.clone().into(), 1);
        {
            let state = state.lock().unwrap();
            assert_eq!(state.current_document_id().as_deref(), Some(second.as_str()));
            let first_doc = state.documents.iter().find(|doc| doc.id == first).unwrap();
            let second_doc = state.documents.iter().find(|doc| doc.id == second).unwrap();
            assert_eq!(first_doc.contract_view.arguments, "17");
            assert_eq!(first_doc.contract_view.section, 1);
            assert!(second_doc.contract_view.arguments.is_empty());
            assert_eq!(native_contract::presentation(&state, first_doc).selected_name, "writeA(uint256 amount)");
            assert_eq!(native_contract::presentation(&state, second_doc).selected_name, "readB()");
        }
        floating.global::<ContractLogic>().invoke_send(first.clone().into());
        {
            let mut state = state.lock().unwrap();
            assert!(!state.send_busy);
            assert!(state.jobs.is_empty());
            assert!(state.notice.contains("owned local Anvil"));
            state.security_confirmation = true;
        }
        floating.global::<ContractLogic>().invoke_arguments_edited(first.clone().into(), "unauthorized".into());
        let state = state.lock().unwrap();
        assert_eq!(state.documents.iter().find(|doc| doc.id == first).unwrap().contract_view.arguments, "17");
        assert_eq!(state.current_document_id().as_deref(), Some(second.as_str()));
    }

    #[tokio::test]
    async fn startup_panics_and_errors_return_to_recovery_instead_of_stranding_opening() {
        let panic = recover_startup(async { panic!("fixture startup failure"); #[allow(unreachable_code)] Ok::<(), anyhow::Error>(()) }).await;
        assert!(panic.unwrap_err().to_string().contains("startup failed unexpectedly"));
        let error = recover_startup(async { Err::<(), _>(anyhow::anyhow!("fixture boot error")) }).await;
        assert_eq!(error.unwrap_err().to_string(), "fixture boot error");
    }

    #[test]
    fn welcome_does_not_write_settings_or_start_jobs_from_hidden_actions() {
        i_slint_backend_testing::init_no_event_loop();
        let fixture = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let mut welcome = DesktopState::welcome();
        welcome.workspace = fixture.path().to_path_buf();
        let state = Arc::new(Mutex::new(welcome));
        let ui = MainWindow::new().unwrap();
        render(&ui, &state.lock().unwrap());
        wire(&ui, &state, runtime.handle().clone());
        ui.global::<Logic>().invoke_select_zoom(2);
        ui.global::<Logic>().invoke_adjust_sidebar(1);
        ui.global::<Logic>().invoke_toggle_sidebar();
        ui.global::<Logic>().invoke_reset_sidebar();
        ui.global::<Logic>().invoke_sidebar_drag_finished();
        ui.global::<Logic>().invoke_recompile();
        assert!(!fixture.path().join(".chasm").exists());
        assert!(state.lock().unwrap().jobs.is_empty());
        assert!(state.lock().unwrap().contracts.is_empty());
        assert!(!ui.global::<AppData>().get_workspace_ready());
        assert!(ui.global::<AppData>().get_startup_screen());
    }

    #[test]
    fn project_confirmation_cancel_and_stale_paint_keep_live_session() {
        i_slint_backend_testing::init_no_event_loop();
        let workspace = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let mut original = DesktopState::new(workspace.path().canonicalize().unwrap(), 1.0);
        original.keystore_root = Some(workspace.path().to_path_buf());
        original.session_id = Some("live-test-session".into());
        original.address_input = "address-sentinel".into();
        original.open_wallet_doc();
        original.pending_workspace = Some(target.path().canonicalize().unwrap());
        let state = Arc::new(Mutex::new(original));
        let ui = MainWindow::new().unwrap();
        render(&ui, &state.lock().unwrap());
        wire(&ui, &state, runtime.handle().clone());
        ui.global::<Logic>().invoke_cancel_open_folder();
        let data = ui.global::<AppData>();
        assert!(data.get_pending_workspace().is_empty());
        assert_eq!(data.get_address_input(), "address-sentinel");
        assert_eq!(state.lock().unwrap().session_id.as_deref(), Some("live-test-session"));
        state.lock().unwrap().active = false;
        data.set_status_left("new-project-sentinel".into());
        state.lock().unwrap().status_left = "obsolete callback result".into();
        paint(&ui.as_weak(), &state);
        assert_eq!(data.get_status_left(), "new-project-sentinel");
    }

    #[test]
    fn wallet_page_navigation_preserves_signer_and_debugger_context() {
        i_slint_backend_testing::init_no_event_loop();
        let workspace = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let mut original = DesktopState::new(workspace.path().to_path_buf(), 1.0);
        original.use_anvil_key(1);
        original.session_id = Some("fixture debugger".into());
        let address = original.wallet_address.clone();
        let state = Arc::new(Mutex::new(original));
        let ui = MainWindow::new().unwrap();
        render(&ui, &state.lock().unwrap());
        wire(&ui, &state, runtime.handle().clone());
        ui.global::<Logic>().invoke_open_wallet();
        assert_eq!(ui.global::<AppData>().get_doc_kind(), 2);
        assert_eq!(ui.global::<AppData>().get_wallet_address(), address.as_str());
        ui.global::<Logic>().invoke_open_activity(1);
        ui.global::<Logic>().invoke_open_wallet();
        assert_eq!(state.lock().unwrap().wallet_address, address);
        assert_eq!(state.lock().unwrap().session_id.as_deref(), Some("fixture debugger"));
        assert_eq!(state.lock().unwrap().documents.iter().filter(|doc| doc.kind == 2).count(), 1);
    }

    #[test]
    fn deployment_preparation_requires_valid_fields_account_and_explicit_idle_state() {
        let raw = r#"{"type":"compile_success","contracts":[{"name":"Vault","source_type":"src","artifact":{"bytecode":{"object":"6000"},"abi":[{"type":"constructor","stateMutability":"nonpayable","inputs":[{"name":"amount","type":"uint8"}]}]}}]}"#;
        let mut state = DesktopState::new(std::path::PathBuf::from("."), 1.0);
        state.contracts = session::contracts_from_compile(raw).unwrap();
        state.select_contract(0);
        state.open_deploy_doc();
        assert!(prepare_deployment(&state).is_err());
        assert!(state.jobs.is_empty());
        state.wallet_key = Some("test-only-account-not-sent".into());
        state.edit_constructor_argument(0, "256".into());
        assert!(prepare_deployment(&state).is_err());
        assert!(state.jobs.is_empty());
        state.edit_constructor_argument(0, "42".into());
        let prepared = prepare_deployment(&state).unwrap();
        assert_eq!(&prepared.creation[..2], &[0x60, 0x00]);
        assert_eq!(prepared.creation.len(), 34);
        assert_eq!(prepared.creation[33], 42);
        assert!(state.jobs.is_empty());
        state.edit_deployment_value("1".into());
        assert!(prepare_deployment(&state).is_err());
        state.edit_deployment_value("0".into());
        state.deploy_busy = true;
        assert!(prepare_deployment(&state).is_err());
    }

    #[test]
    fn constructor_callbacks_keep_fields_stable_and_validate_before_submission() {
        i_slint_backend_testing::init_no_event_loop();
        let workspace = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let raw = r#"{"type":"compile_success","contracts":[{"name":"Bank","source_type":"src","artifact":{"bytecode":{"object":"6000"},"abi":[{"type":"constructor","stateMutability":"nonpayable","inputs":[{"name":"_owner","type":"address"},{"name":"_luckyNumbers","type":"uint256[]"}]}]}}]}"#;
        let mut state = DesktopState::new(workspace.path().to_path_buf(), 1.0);
        state.contracts = session::contracts_from_compile(raw).unwrap();
        state.select_contract(0);
        state.open_deploy_doc();
        let state = Arc::new(Mutex::new(state));
        let ui = MainWindow::new().unwrap();
        render(&ui, &state.lock().unwrap());
        wire(&ui, &state, runtime.handle().clone());
        let data = ui.global::<AppData>();
        let fields = data.get_constructor_fields();
        assert_eq!(fields.row_count(), 2);
        assert_eq!(fields.row_data(0).unwrap().name, "_owner");
        assert_eq!(fields.row_data(1).unwrap().kind, "uint256[]");
        assert!(!data.get_deploy_ready());
        ui.global::<Logic>().invoke_constructor_edited(0, "0x0000000000000000000000000000000000000042".into());
        ui.global::<Logic>().invoke_constructor_edited(1, "[1,2,9007199254740993]".into());
        assert!(data.get_constructor_fields() == fields);
        assert!(fields.row_data(0).unwrap().error.is_empty());
        assert!(fields.row_data(1).unwrap().error.is_empty());
        assert!(!data.get_deploy_ready());
        ui.global::<Logic>().invoke_deploy_contract();
        assert!(state.lock().unwrap().jobs.is_empty());
        assert!(!state.lock().unwrap().deploy_busy);
        state.lock().unwrap().wallet_key = Some("test-only-account-not-sent".into());
        render(&ui, &state.lock().unwrap());
        assert!(data.get_deploy_ready());
        ui.global::<Logic>().invoke_deployment_value_edited("1".into());
        assert!(!data.get_deploy_ready());
        assert!(data.get_deployment_value_error().contains("nonpayable"));
        ui.global::<Logic>().invoke_deployment_value_edited("0".into());
        assert!(data.get_deploy_ready());
        ui.global::<Logic>().invoke_select_zoom(2);
        ui.global::<Logic>().invoke_open_activity(1);
        ui.global::<Logic>().invoke_open_activity(0);
        assert!(data.get_constructor_fields() == fields);
        assert_eq!(fields.row_data(1).unwrap().value, "[1,2,9007199254740993]");
        assert_eq!(data.get_deploy_value(), "0");
        ui.global::<Logic>().invoke_constructor_edited(1, "[1.5]".into());
        assert!(!data.get_deploy_ready());
        assert!(!fields.row_data(1).unwrap().error.is_empty());
    }

    #[test]
    fn live_zoom_callback_preserves_ui_and_session() {
        i_slint_backend_testing::init_no_event_loop();
        let workspace = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let mut state = DesktopState::new(workspace.path().to_path_buf(), 1.0);
        state.session_id = Some("offline-session-sentinel".into());
        state.generation = 7;
        state.revision = 11;
        state.address_input = "0x0000000000000000000000000000000000000042".into();
        state.open_wallet_doc();
        let state = Arc::new(Mutex::new(state));
        let ui = MainWindow::new().unwrap();
        render(&ui, &state.lock().unwrap());
        wire(&ui, &state, runtime.handle().clone());
        ui.show().unwrap();
        let data = ui.global::<AppData>();
        data.set_call_args("[1,2,3] sentinel".into());
        data.set_call_value("1000000000000000000".into());
        data.set_deploy_value("0".into());
        data.set_storage_slot("42".into());
        state.lock().unwrap().converter_input = "0x19".into();
        state.lock().unwrap().explorer_input = "explorer-sentinel".into();
        data.set_converter_input("0x19".into());
        data.set_explorer_input("explorer-sentinel".into());
        let documents = data.get_documents();
        let dpi = ui.window().scale_factor();

        for (index, expected) in [(0, 1.0), (2, 1.25), (3, 1.5), (0, 1.0)] {
            ui.global::<Logic>().invoke_select_zoom(index);
            assert_eq!(ui.global::<Theme>().get_zoom(), expected);
            assert_eq!(data.get_zoom_percent(), (expected * 100.0) as i32);
            assert_eq!(ui.window().scale_factor(), dpi);
            assert!(ui.window().is_visible());
            assert_eq!(data.get_call_args(), "[1,2,3] sentinel");
            assert_eq!(data.get_call_value(), "1000000000000000000");
            assert_eq!(data.get_deploy_value(), "0");
            assert_eq!(data.get_storage_slot(), "42");
            assert_eq!(data.get_converter_input(), "0x19");
            assert_eq!(data.get_explorer_input(), "explorer-sentinel");
            assert_eq!(data.get_address_input(), "0x0000000000000000000000000000000000000042");
            assert!(data.get_documents() == documents);
            assert_eq!(load_zoom(workspace.path()), expected);
            let borrowed = state.lock().unwrap();
            assert_eq!(borrowed.session_id.as_deref(), Some("offline-session-sentinel"));
            assert_eq!((borrowed.generation, borrowed.revision), (7, 11));
            assert_eq!(borrowed.documents.len(), 1);
        }

        data.set_menu_open(3);
        ui.global::<Logic>().invoke_select_zoom(-1);
        assert_eq!(ui.global::<Theme>().get_zoom(), 1.0);
        assert_eq!(data.get_menu_open(), 3);
        let edited_address = "0x0000000000000000000000000000000000000099";
        data.set_address_input(edited_address.into());
        ui.global::<Logic>().invoke_address_edited(edited_address.into());
        render(&ui, &state.lock().unwrap());
        assert_eq!(data.get_address_input(), "0x0000000000000000000000000000000000000042");
        {
            let mut state = state.lock().unwrap();
            state.documents.push(session::Document::new("Bank".into(), String::new(), 0));
            let index = state.documents.len() as i32 - 1;
            state.select_document(index);
        }
        ui.global::<Logic>().invoke_address_edited(edited_address.into());
        render(&ui, &state.lock().unwrap());
        assert_eq!(data.get_address_input(), edited_address);
        assert_eq!(data.get_call_args(), "[1,2,3] sentinel");
        assert!(ui.window().is_visible());

        state.lock().unwrap().status_left = "Ready".into();
        render(&ui, &state.lock().unwrap());
        assert_eq!(data.get_status_left(), "Ready");
        ui.global::<Logic>().invoke_sidebar_drag_started(400.0);
        ui.global::<Logic>().invoke_sidebar_resized(200.0);
        assert_eq!(state.lock().unwrap().sidebar_width, 180.0);
        ui.global::<Logic>().invoke_sidebar_resized(470.0);
        assert_eq!(data.get_sidebar_width(), 320.0);
        ui.global::<Logic>().invoke_sidebar_drag_finished();
        render(&ui, &state.lock().unwrap());
        assert!((ui.window().size().width as f32) > 0.0);
        state.lock().unwrap().deployments.push(session::Deployment { name: "Bank".into(), address: "0x1111111111111111111111111111111111111111".into() });
        render(&ui, &state.lock().unwrap());
        ui.global::<Logic>().invoke_remove_deployment(0);
        assert!(state.lock().unwrap().deployments.is_empty());
        ui.global::<Logic>().invoke_use_anvil_key(2);
        assert_eq!(state.lock().unwrap().wallet_address, "0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC");
    }
}
