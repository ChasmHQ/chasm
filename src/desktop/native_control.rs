use super::application::{self, Activity, Command, PanelAction, Query};
use super::control::{Hub, Tier, UiRequest};
use super::control_http::ControlHost;
use super::{
    AppData, AutomationClient, AutomationData, AutomationLogic, DesktopState, Logic, MainWindow,
};
use serde_json::{json, Value};
use slint::{ComponentHandle, Model, Weak};
use std::sync::{Arc, Mutex};

pub fn wire(
    ui: &MainWindow,
    state: &Arc<Mutex<DesktopState>>,
    hub: &Arc<Hub>,
    host: &Arc<ControlHost>,
    runtime: tokio::runtime::Handle,
) -> slint::Timer {
    let weak = ui.as_weak();
    let held = state.clone();
    let hub_bridge = hub.clone();
    hub.bind(Arc::new(move |request| {
        let weak = weak.clone();
        let held = held.clone();
        let hub = hub_bridge.clone();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = weak.upgrade() else { return };
            match request {
                UiRequest::Source {
                    principal,
                    context,
                    reply,
                } => {
                    let state = held.lock().unwrap();
                    let result = hub
                        .authorize(&principal, "analyze")
                        .and_then(|_| application::check_context(&state, &context))
                        .and_then(|_| {
                            if state.workspace_ready && !state.transition_pending() {
                                Ok(state.rpc.clone())
                            } else {
                                Err(application::AppError::new(
                                    "workspace_unavailable",
                                    "The selected workspace is not ready.",
                                ))
                            }
                        });
                    let _ = reply.send(result);
                }
                UiRequest::Query {
                    principal,
                    query,
                    reply,
                } => {
                    let result = hub.authorize(&principal, query.scope()).and_then(|_| {
                        let state = held.lock().unwrap();
                        if !state.active {
                            return Err(application::AppError::new(
                                "stale_context",
                                "The workspace changed before the query ran.",
                            ));
                        }
                        let mut value = application::query(&state, &query);
                        drop(state);
                        match query {
                            Query::Macros => value["macros"] = macro_state(&ui),
                            Query::Debugger => value["debugger"] = debugger_state(&ui),
                            Query::Screenshot => value["screenshot"] = screenshot(&ui)?,
                            Query::History => value["status"] = ui.global::<super::HistoryData>().get_status().as_str().into(),
                            Query::Toolchain => value["status"] = ui.global::<super::ToolchainData>().get_status().as_str().into(),
                            _ => {}
                        }
                        Ok(value)
                    });
                    let _ = reply.send(result);
                }
                UiRequest::Command {
                    principal,
                    request,
                    reply,
                } => {
                    let mut result = hub.execute_ui(&principal, &request, &mut held.lock().unwrap());
                    if let Ok(value) = &result {
                        if value["effect"] == "native" {
                            let outcome = perform(&ui, &held, &request.command);
                            hub.amend_result(&principal, &request.request_id, outcome.clone());
                            result = Ok(outcome);
                        }
                    }
                    let switch = result
                        .as_ref()
                        .ok()
                        .is_some_and(|value| value["effect"] == "workspace_switch");
                    if result.is_ok() { super::panels::focus_current(); }
                    super::paint(&weak, &held);
                    if result.is_ok() && matches!(request.command, Command::Navigate { activity: Activity::Explorer }) {
                        ui.global::<super::Logic>().invoke_fetch_block();
                    }
                    if let Command::Convert { input, .. } = &request.command {
                        if result
                            .as_ref()
                            .ok()
                            .is_some_and(|value| value["status"] == "completed")
                        {
                            ui.global::<AppData>()
                                .set_converter_input(input.clone().into());
                        }
                    }
                    sync(&ui, &hub);
                    let _ = reply.send(result);
                    if switch {
                        let _ = slint::quit_event_loop();
                    }
                }
            }
        });
    }));
    wire_shared(ui, state);

    let weak = ui.as_weak();
    let hub_open = hub.clone();
    ui.global::<AutomationLogic>().on_open(move || {
        if let Some(ui) = weak.upgrade() {
            ui.global::<super::InvestigationData>().set_visible(false);
            ui.global::<AutomationData>().set_visible(true);
            sync(&ui, &hub_open);
        }
    });
    let weak = ui.as_weak();
    let hub_close = hub.clone();
    ui.global::<AutomationLogic>().on_close(move || {
        if let Some(ui) = weak.upgrade() {
            let data = ui.global::<AutomationData>();
            data.set_pairing_token("".into());
            data.set_visible(false);
            sync(&ui, &hub_close);
        }
    });
    let weak = ui.as_weak();
    let host_enable = host.clone();
    let runtime_enable = runtime.clone();
    let hub_enable = hub.clone();
    ui.global::<AutomationLogic>().on_enable(move || {
        let Some(ui) = weak.upgrade() else { return };
        if ui.global::<AutomationData>().get_busy() {
            return;
        }
        ui.global::<AutomationData>().set_busy(true);
        let weak = weak.clone();
        let host = host_enable.clone();
        let hub = hub_enable.clone();
        runtime_enable.spawn(async move {
            let result = host.enable().await;
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = weak.upgrade() {
                    ui.global::<AutomationData>().set_busy(false);
                    ui.global::<AutomationData>().set_message(match result {
                        Ok(_) => "Local access enabled. Create a client token to connect.".into(),
                        Err(error) => error.message.into(),
                    });
                    sync(&ui, &hub);
                }
            });
        });
    });
    let weak = ui.as_weak();
    let host_disable = host.clone();
    let hub_disable = hub.clone();
    ui.global::<AutomationLogic>().on_disable(move || {
        host_disable.disable();
        if let Some(ui) = weak.upgrade() {
            ui.global::<AutomationData>().set_pairing_token("".into());
            ui.global::<AutomationData>().set_message(
                "Access disabled. All client tokens and pending approvals were revoked.".into(),
            );
            sync(&ui, &hub_disable);
        }
    });
    let weak = ui.as_weak();
    let hub_pair = hub.clone();
    ui.global::<AutomationLogic>().on_pair(move |mode| {
        if let Some(ui) = weak.upgrade() {
            let tier = match mode {
                0 => Tier::Limited,
                1 => Tier::Partial,
                _ => Tier::Full,
            };
            match hub_pair.pair_local(tier) {
                Ok((id, token)) => {
                    ui.global::<AutomationData>()
                        .set_pairing_token(token.into());
                    ui.global::<AutomationData>()
                        .set_message(format!("Created {id}").into());
                }
                Err(error) => ui
                    .global::<AutomationData>()
                    .set_message(error.message.into()),
            }
            sync(&ui, &hub_pair);
        }
    });
    let weak = ui.as_weak();
    let hub_revoke = hub.clone();
    ui.global::<AutomationLogic>().on_revoke(move |id| {
        hub_revoke.revoke_local(id.as_str());
        if let Some(ui) = weak.upgrade() {
            ui.global::<AutomationData>().set_pairing_token("".into());
            sync(&ui, &hub_revoke);
        }
    });
    for accepted in [false, true] {
        let weak = ui.as_weak();
        let held = state.clone();
        let hub = hub.clone();
        let callback = move |id: slint::SharedString| {
            let Some(ui) = weak.upgrade() else { return };
            let result = hub.resolve_local(id.as_str(), accepted, &mut held.lock().unwrap());
            let switch = result
                .as_ref()
                .ok()
                .is_some_and(|(value, _)| value["effect"] == "workspace_switch");
            // The approval left the queue; lift its confirmation gate before the
            // callbacks run, or they would see a pending confirmation and stop.
            sync(&ui, &hub);
            match result {
                Ok((value, request)) if value["effect"] == "native" => {
                    let outcome = perform(&ui, &held, &request.command);
                    if let Some(client) = hub.client_of(&request.request_id) {
                        hub.amend_result(&client, &request.request_id, outcome);
                    }
                }
                Ok(_) => {}
                Err(error) => ui.global::<AutomationData>().set_message(error.message.into()),
            }
            super::paint(&weak, &held);
            sync(&ui, &hub);
            if switch {
                let _ = slint::quit_event_loop();
            }
        };
        if accepted {
            ui.global::<AutomationLogic>().on_accept(callback)
        } else {
            ui.global::<AutomationLogic>().on_reject(callback)
        }
    }
    sync(ui, hub);
    let weak = ui.as_weak();
    let hub = hub.clone();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(250),
        move || {
            if let Some(ui) = weak.upgrade() {
                sync(&ui, &hub);
            }
        },
    );
    timer
}

fn sync(ui: &MainWindow, hub: &Hub) {
    let status = hub.local_status();
    let data = ui.global::<AutomationData>();
    super::panels::set_security_confirmation(
        status["pendingApprovals"].as_u64().unwrap_or(0) > 0 || data.get_visible(),
    );
    data.set_enabled(status["enabled"].as_bool().unwrap_or(false));
    data.set_endpoint(status["endpoint"].as_str().unwrap_or_default().into());
    let clients = status["clients"]
        .as_array()
        .unwrap()
        .iter()
        .map(|client| AutomationClient {
            id: client["id"].as_str().unwrap_or_default().into(),
            scopes: match client["tier"].as_str() {
                Some("limited") => "Limited: read and analysis",
                Some("partial") => "Partial: read, UI control, chain actions after approval",
                _ => "Full: all actions except secrets",
            }
            .into(),
        })
        .collect();
    data.set_clients(super::model(clients));
    if let Some(approval) = hub.pending_local() {
        if data.get_approval_id().as_str() != approval.id {
            if !data.get_approval_id().is_empty() {
                data.set_approval_id("".into());
                data.set_approval_detail("".into());
                return;
            }
            data.set_approval_id(approval.id.clone().into());
            data.set_approval_detail(format!("Client: {}\nAction: {}\nWorkspace: {}\nNetwork epoch: {}\nTarget: {}\nParameters:\n{}",approval.client_id,approval.request.command.action_id(),approval.request.context.workspace_id,approval.request.context.network_epoch,
                approval.target, serde_json::to_string_pretty(&approval.request.command).unwrap_or_default()).into());
            data.set_approval_action("Approve once".into());
        }
    } else {
        data.set_approval_id("".into());
        data.set_approval_detail("".into());
    }
}

fn local(weak: &Weak<MainWindow>, held: &Arc<Mutex<DesktopState>>, command: Command) {
    let mut state = held.lock().unwrap();
    if let Err(error) = application::apply(&mut state, &command) {
        state.notice = error.message;
    }
    let input = state.converter_input.clone();
    drop(state);
    super::panels::focus_current();
    super::paint(weak, held);
    if let Some(ui) = weak.upgrade() {
        if matches!(command, Command::Convert { .. }) {
            ui.global::<AppData>().set_converter_input(input.into());
        }
    }
}

pub(super) fn wire_shared(ui: &MainWindow, state: &Arc<Mutex<DesktopState>>) {
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<Logic>().on_open_activity(move |index| {
        let activity = match index {
            0 => Activity::Contracts,
            1 => Activity::Explorer,
            2 => Activity::Converter,
            3 => Activity::Macros,
            _ => Activity::Debugger,
        };
        let explorer = matches!(activity, Activity::Explorer);
        local(&weak, &held, Command::Navigate { activity });
        if explorer {
            if let Some(ui) = weak.upgrade() {
                ui.global::<Logic>().invoke_fetch_block();
            }
        }
    });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<Logic>().on_open_wallet(move || {
        local(
            &weak,
            &held,
            Command::Navigate {
                activity: Activity::Wallet,
            },
        )
    });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<Logic>().on_select_document(move |index| {
        let id = application::document_id(&held.lock().unwrap(), index as usize);
        if let Some(id) = id {
            local(&weak, &held, Command::SelectDocument { id });
        }
    });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<Logic>().on_close_document(move |index| {
        let id = application::document_id(&held.lock().unwrap(), index as usize);
        if let Some(id) = id {
            local(&weak, &held, Command::CloseDocument { id });
        }
    });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<Logic>().on_select_contract(move |index| {
        let id = application::contract_id(&held.lock().unwrap(), index as usize);
        if let Some(id) = id {
            local(&weak, &held, Command::SelectContract { id });
        }
    });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<Logic>().on_select_function(move |index| {
        let id = application::function_id(&held.lock().unwrap(), index as usize);
        if let Some(id) = id {
            local(&weak, &held, Command::SelectFunction { id });
        }
    });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<Logic>().on_select_deployment(move |index| {
        let id = application::deployment_id(&held.lock().unwrap(), index as usize);
        if let Some(id) = id {
            local(&weak, &held, Command::SelectDeployment { id });
        }
    });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<Logic>().on_remove_deployment(move |index| {
        let id = application::deployment_id(&held.lock().unwrap(), index as usize);
        if let Some(id) = id {
            local(&weak, &held, Command::RemoveDeployment { id });
        }
    });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<Logic>().on_converter_edited(move |input| {
        let command = {
            let state = held.lock().unwrap();
            Command::Convert {
                tool: state.converter_tool,
                mode: state.converter_mode,
                input: input.to_string(),
            }
        };
        local(&weak, &held, command);
    });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<Logic>().on_set_converter_tool(move |tool| {
        let input = held.lock().unwrap().converter_input.clone();
        local(
            &weak,
            &held,
            Command::Convert {
                tool,
                mode: 0,
                input,
            },
        );
    });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<Logic>().on_set_converter_mode(move |mode| {
        let command = {
            let state = held.lock().unwrap();
            Command::Convert {
                tool: state.converter_tool,
                mode,
                input: state.converter_input.clone(),
            }
        };
        local(&weak, &held, command);
    });
}

/// Run an accepted command through the same callbacks as the native buttons,
/// so remote control and local clicks share validation, gates and jobs.
fn perform(ui: &MainWindow, held: &Arc<Mutex<DesktopState>>, command: &Command) -> Value {
    use super::{ContractLogic, DebugData, DebugLogic, Logic, MacroData, MacroLogic, PanelLogic};
    let owner = || held.lock().unwrap().current_document_id().unwrap_or_default();
    let contract = ui.global::<ContractLogic>();
    let logic = ui.global::<Logic>();
    let macros = ui.global::<MacroLogic>();
    let debug = ui.global::<DebugLogic>();
    match command {
        Command::EditRequest { arguments, value } => {
            if let Some(arguments) = arguments {
                contract.invoke_arguments_edited(owner().into(), arguments.as_str().into());
            }
            if let Some(value) = value {
                contract.invoke_value_edited(owner().into(), value.as_str().into());
            }
        }
        Command::Attach { address } => contract.invoke_attach(owner().into(), address.as_str().into()),
        Command::Simulate {} => contract.invoke_read(owner().into()),
        Command::Send {} => contract.invoke_send(owner().into()),
        Command::Trace {} => {
            contract.invoke_result_view(owner().into(), 2);
            contract.invoke_trace(owner().into());
        }
        Command::ReadStorage { slot } => {
            contract.invoke_slot_edited(owner().into(), slot.as_str().into());
            contract.invoke_read_slot(owner().into());
        }
        Command::Lookup { query } => logic.invoke_explorer_open(query.as_str().into()),
        Command::TraceLoad { hash, call } => {
            ui.global::<AppData>().set_debugger_mode(0);
            match call {
                Some(call) => {
                    let from = held.lock().unwrap().wallet_address.clone();
                    match call.request(&from) {
                        Ok((request, block)) => super::native_trace::load_call(ui.as_weak(), held.clone(), request, block),
                        // Validated before the command ran; nothing to do.
                        Err(_) => {}
                    }
                }
                None => ui.global::<super::TraceLogic>().invoke_load(hash.clone().unwrap_or_default().into()),
            }
        }
        Command::TraceStep { action } => ui.global::<super::TraceLogic>().invoke_navigate(action.as_str().into()),
        Command::TraceSeek { step } => ui.global::<super::TraceLogic>().invoke_seek(*step as i32),
        Command::TraceBreakpoint { line } => ui.global::<super::TraceLogic>().invoke_toggle_breakpoint(*line as i32),
        Command::TraceOpenSaved { name } => ui.global::<super::TraceLogic>().invoke_open_saved(name.as_str().into()),
        Command::TraceConsole { line } => ui.global::<super::TraceLogic>().invoke_console_run(line.as_str().into()),
        Command::TimelineRevert { index } => ui.global::<super::ChainLogic>().invoke_revert_to(*index as i32),
        Command::Cheat { action, address, value, slot } => {
            let data = ui.global::<super::ChainData>();
            data.set_action(action.as_str().into());
            data.set_address(address.clone().unwrap_or_default().into());
            data.set_value(value.as_str().into());
            data.set_slot(slot.clone().unwrap_or_default().into());
            ui.global::<super::ChainLogic>().invoke_apply_cheat();
        }
        Command::Toolchain { action, library, version, fork_url, fork_block, optimizer, optimizer_runs, via_ir, evm_version, bytecode_hash } => {
            let logic = ui.global::<super::ToolchainLogic>();
            let library = library.clone().unwrap_or_default();
            let version = version.clone().unwrap_or_default();
            match action.as_str() {
                "open" => ui.global::<Logic>().invoke_open_toolchain(),
                "lock_solc" => logic.invoke_lock_solc(version.into()),
                "install" => logic.invoke_install(library.into(), version.into()),
                "use" => logic.invoke_use_version(library.into(), version.into()),
                "remove" => logic.invoke_remove(library.into()),
                "network" => logic.invoke_save_network(fork_url.clone().unwrap_or_default().into(), fork_block.clone().unwrap_or_default().into()),
                "settings" => logic.invoke_save_settings(
                    match optimizer { Some(true) => 1, Some(false) => 2, None => 0 },
                    optimizer_runs.map(|runs| runs.to_string()).unwrap_or_default().into(),
                    via_ir.unwrap_or(false),
                    evm_version.clone().unwrap_or_default().into(),
                    bytecode_hash.clone().unwrap_or_default().into(),
                ),
                _ => {}
            }
        }
        Command::Palette { query, index } => {
            let data = ui.global::<super::PaletteData>();
            data.set_open(true);
            data.set_query(query.as_str().into());
            match index {
                Some(index) => ui.global::<super::PaletteLogic>().invoke_run(*index as i32),
                None => ui.global::<super::PaletteLogic>().invoke_query_edited(query.as_str().into()),
            }
        }
        Command::Template { kind, name } => logic.invoke_new_from_template(kind.as_str().into(), name.as_str().into()),
        Command::NoticeAction {} => {
            let action = ui.global::<AppData>().get_notice_action();
            ui.global::<Logic>().invoke_notice_action(action);
        }
        Command::HistoryAction { action, index } => {
            let logic = ui.global::<super::HistoryLogic>();
            ui.global::<super::HistoryData>().set_filter(0);
            match action.as_str() {
                "repeat" => logic.invoke_repeat(*index as i32),
                "debug" => logic.invoke_debug(*index as i32),
                "to_macro" => logic.invoke_to_macro(*index as i32),
                "timeline" => {
                    ui.global::<AppData>().set_explorer_tab(3);
                    ui.global::<Logic>().invoke_open_activity(1);
                }
                "show" => {
                    ui.global::<AppData>().set_explorer_tab(2);
                    ui.global::<Logic>().invoke_open_activity(1);
                }
                _ => {}
            }
        }
        Command::TraceView { opcodes, state, console, compact, tab, opcodes_width, state_width, console_height } => {
            let data = ui.global::<super::TraceData>();
            if let Some(value) = opcodes { data.set_show_opcodes(*value); }
            if let Some(value) = state { data.set_show_state(*value); }
            if let Some(value) = console { data.set_show_console(*value); }
            if let Some(value) = compact { data.set_compact(*value); }
            if let Some(value) = tab { data.set_state_tab(i32::from((*value).min(5))); }
            if let Some(value) = opcodes_width { data.set_opcodes_width(value.clamp(220.0, 700.0)); }
            if let Some(value) = state_width { data.set_state_width(value.clamp(200.0, 900.0)); }
            if let Some(value) = console_height { data.set_console_height(value.clamp(70.0, 600.0)); }
        }
        Command::ReadVariables {} => {
            contract.invoke_section(owner().into(), 1);
            contract.invoke_refresh_variables(owner().into());
        }
        Command::ReadMapping { variable, key } => {
            contract.invoke_mapping_edited(owner().into(), variable.as_str().into(), key.as_str().into());
            contract.invoke_read_mapping(owner().into());
        }
        Command::OpenDeploy {} => logic.invoke_open_deploy(),
        Command::Deploy {} => logic.invoke_deploy_contract(),
        Command::Compile {} => logic.invoke_recompile(),
        Command::UseDevAccount { index } => logic.invoke_use_anvil_key(*index as i32),
        Command::Snapshot {} => logic.invoke_snapshot(),
        Command::Revert {} => logic.invoke_revert(),
        Command::Mine {} => logic.invoke_roll(),
        Command::Warp {} => logic.invoke_warp(),
        Command::NewMacro {} => macros.invoke_new_macro(),
        Command::SelectMacro { id } => {
            let rows = ui.global::<MacroData>().get_macros();
            match (0..rows.row_count()).find(|index| rows.row_data(*index).is_some_and(|row| row.id.as_str() == id)) {
                Some(index) => macros.invoke_select(index as i32),
                None => return json!({"status":"failed","error":{"code":"entity_not_found","message":"No macro with that ID."}}),
            }
        }
        Command::EditMacro { json } => {
            ui.global::<MacroData>().set_definition_json(json.as_str().into());
            macros.invoke_edited(json.as_str().into());
        }
        Command::ValidateMacro {} => macros.invoke_validate(),
        Command::RunMacro { dry_run } => {
            if !ui.global::<MacroData>().get_validation_ready() {
                return json!({"status":"failed","error":{"code":"not_ready","message":"Validate the current draft first (macros.validate)."}});
            }
            if *dry_run { macros.invoke_dry_run() } else { macros.invoke_run() }
        }
        Command::CancelMacro {} => macros.invoke_cancel(),
        Command::AddStep { kind } => macros.invoke_add_step(kind.as_str().into()),
        Command::SelectStep { index } => macros.invoke_select_step(*index as i32),
        Command::StepField { index, field, value } => macros.invoke_set_field(*index as i32, field.as_str().into(), value.as_str().into()),
        Command::StepEncode { index, contract, function, arguments, send } => {
            let data = ui.global::<MacroData>();
            let pick = |rows: slint::ModelRc<super::MacroChoice>, wanted: &str| rows_index(&rows, |row| row.label.as_str() == wanted || row.label.split('(').next() == Some(wanted));
            let Some(contract_index) = pick(data.get_contracts(), contract) else {
                return json!({"status":"failed","error":{"code":"entity_not_found","message":"No compiled workspace contract with that name."}});
            };
            macros.invoke_choose_call(contract_index as i32, -1);
            let Some(function_index) = pick(data.get_functions(), function) else {
                return json!({"status":"failed","error":{"code":"entity_not_found","message":"No function with that name on the contract."}});
            };
            macros.invoke_choose_call(contract_index as i32, function_index as i32);
            macros.invoke_encode_call(*index as i32, arguments.as_str().into(), *send);
            let error = data.get_call_error();
            if !error.is_empty() {
                return json!({"status":"failed","error":{"code":"invalid_params","message":error.as_str()}});
            }
        }
        Command::SelectMacroResult { index } => macros.invoke_select_result(*index as i32),
        Command::DebugLaunch { mode, foundry_target, foundry_match, trusted, capture_id, build_info_id } => {
            let data = ui.global::<DebugData>();
            data.set_launch_mode(*mode as i32);
            if let Some(value) = foundry_target { data.set_foundry_target(value.as_str().into()); }
            if let Some(value) = foundry_match { data.set_foundry_match(value.as_str().into()); }
            if let Some(value) = trusted { data.set_foundry_trusted(*value); }
            if let Some(value) = capture_id { data.set_capture_id(value.as_str().into()); }
            if let Some(value) = build_info_id { data.set_build_info_id(value.as_str().into()); }
            debug.invoke_launch_edited();
        }
        Command::DebugStart {} => debug.invoke_start(),
        Command::DebugNavigate { action } => debug.invoke_navigate(action.as_str().into()),
        Command::DebugEvaluate { expression } => {
            ui.global::<DebugData>().set_evaluate_input(expression.as_str().into());
            debug.invoke_evaluate();
        }
        Command::DebugConsole { command } => ui.global::<super::PanelLogic>().invoke_console_submit(command.as_str().into()),
        Command::DebugCancel {} => debug.invoke_cancel(),
        Command::CaptureSave {} => debug.invoke_save_capture(),
        Command::CaptureOpen { name } => debug.invoke_open_capture(name.as_str().into()),
        Command::Panel { pane, action, zone } => {
            let panels = ui.global::<PanelLogic>();
            match action {
                PanelAction::Dock => super::panels::dock(pane, zone.map(i32::from).unwrap_or(-1)),
                PanelAction::Float => panels.invoke_float(pane.as_str().into()),
                PanelAction::Hide => super::panels::hide(pane),
                PanelAction::Maximize => panels.invoke_maximize(pane.as_str().into()),
                PanelAction::Reset => panels.invoke_reset(),
                PanelAction::Preset => panels.invoke_preset(match pane.as_str() {
                    "debugging" => 1,
                    "investigation" => 2,
                    "tracing" => 3,
                    _ => 0,
                }),
            }
        }
        _ => {}
    }
    let state = held.lock().unwrap();
    json!({"status":"completed","effect":"native","context":application::context(&state),"notice":state.notice,"statusLine":state.status_left})
}

fn rows_index<T: Clone>(model: &slint::ModelRc<T>, matches: impl Fn(&T) -> bool) -> Option<usize> {
    (0..model.row_count()).find(|index| model.row_data(*index).is_some_and(|row| matches(&row)))
}

fn rows<T: Clone>(model: slint::ModelRc<T>) -> Vec<T> {
    (0..model.row_count()).filter_map(|index| model.row_data(index)).collect()
}

fn macro_state(ui: &MainWindow) -> Value {
    let data = ui.global::<super::MacroData>();
    json!({
        "library": rows(data.get_macros()).into_iter().map(|row| json!({"id":row.id.as_str(),"name":row.name.as_str(),"detail":row.detail.as_str()})).collect::<Vec<_>>(),
        "selectedId": data.get_selected_id().as_str(),
        "selectedName": data.get_selected_name().as_str(),
        "definitionJson": data.get_definition_json().as_str(),
        "validationCurrent": data.get_validation_ready(),
        "validation": data.get_validation().as_str(),
        "validationFailed": data.get_validation_failed(),
        "preflight": {"status": data.get_preflight_status().as_str(), "broadcast": data.get_preflight_broadcast().as_str(), "read": data.get_preflight_read().as_str(), "sweep": data.get_preflight_sweep().as_str(), "signers": data.get_preflight_signers().as_str(), "snapshot": data.get_preflight_snapshot().as_str()},
        "status": data.get_status().as_str(),
        "error": data.get_error().as_str(),
        "runBusy": data.get_run_busy(),
        "canRun": data.get_can_run(),
        "canDryRun": data.get_can_dry_run(),
        "steps": rows(data.get_steps()).into_iter().map(|step| json!({"name":step.name.as_str(),"kind":step.kind.as_str(),"enabled":step.enabled,"status":step.status.as_str(),"duration":step.duration.as_str(),"result":step.public_result.as_str(),"hint":step.action_hint.as_str()})).collect::<Vec<_>>(),
        "results": rows(data.get_results()).into_iter().map(|run| json!({"id":run.id.as_str(),"title":run.title.as_str(),"status":run.status.as_str(),"duration":run.duration.as_str()})).collect::<Vec<_>>(),
        "selectedResult": data.get_selected_result(),
        "resultSimple": data.get_result_simple().as_str(),
        "resultRaw": data.get_result_raw().as_str(),
        "runLog": data.get_run_log().as_str(),
        "selectedStep": data.get_selected_step(),
        "stepKind": data.get_step_kind().as_str(),
        "stepSummaries": rows(data.get_step_summaries()).into_iter().map(|item| item.to_string()).collect::<Vec<_>>(),
        "form": rows(data.get_form_fields()).into_iter().map(|field| json!({"key":field.key.as_str(),"label":field.label.as_str(),"value":field.value.as_str()})).collect::<Vec<_>>(),
        "references": rows(data.get_references()).into_iter().map(|item| item.to_string()).collect::<Vec<_>>(),
        "stepResult": data.get_step_result().as_str(),
    })
}

fn debugger_state(ui: &MainWindow) -> Value {
    let data = ui.global::<super::DebugData>();
    json!({
        "launchMode": data.get_launch_mode(), "launchSummary": data.get_launch_summary().as_str(),
        "captureId": data.get_capture_id().as_str(), "captureStatus": data.get_capture_status().as_str(),
        "captures": rows(data.get_saved_captures()).into_iter().map(|name| name.to_string()).collect::<Vec<_>>(),
        "phase": data.get_phase().as_str(), "status": data.get_status().as_str(),
        "location": data.get_location().as_str(), "notice": data.get_notice().as_str(),
        "canStart": data.get_can_start(), "canCommand": data.get_can_command(), "canCancel": data.get_can_cancel(),
        "outcome": data.get_outcome().as_str(), "completion": data.get_completion().as_str(),
        "code": rows(data.get_code_rows()).into_iter().map(|row| json!({"position":row.position.as_str(),"text":row.text.as_str(),"current":row.current})).collect::<Vec<_>>(),
        "execution": rows(data.get_execution()).into_iter().map(|stat| json!({"label":stat.label.as_str(),"value":stat.value.as_str()})).collect::<Vec<_>>(),
        "frames": data.get_frames_text().as_str(), "locals": data.get_locals_text().as_str(),
        "stack": data.get_stack_text().as_str(), "storage": data.get_storage_text().as_str(),
        "result": data.get_result_text().as_str(), "logs": data.get_logs_text().as_str(),
        "console": ui.global::<super::PanelData>().get_console_text().as_str(),
        "trace": trace_state(ui),
    })
}

fn trace_state(ui: &MainWindow) -> Value {
    let trace = ui.global::<super::TraceData>();
    json!({
                "loaded": trace.get_loaded(), "busy": trace.get_busy(), "status": trace.get_status().as_str(),
                "title": trace.get_title().as_str(), "summary": trace.get_summary().as_str(),
                "location": trace.get_location().as_str(), "step": trace.get_cursor(), "total": trace.get_total(),
                "file": trace.get_file().as_str(),
                "sourceLine": rows(trace.get_source()).into_iter().find(|line| line.current).map(|line| json!({"number":line.number,"text":line.text.as_str()})),
                "stack": rows(trace.get_stack()).into_iter().take(8).map(|item| item.value.to_string()).collect::<Vec<_>>(),
                "storage": rows(trace.get_storage()).into_iter().map(|item| item.value.to_string()).collect::<Vec<_>>(),
                "frames": rows(trace.get_frames()).into_iter().map(|item| item.value.to_string()).collect::<Vec<_>>(),
                "gas": {"calls": rows(trace.get_gas_calls()).into_iter().map(|item| json!({"name":item.label.as_str(),"value":item.value.as_str()})).collect::<Vec<_>>(), "lines": rows(trace.get_gas_lines()).into_iter().take(10).map(|item| json!({"name":item.label.as_str(),"value":item.value.as_str()})).collect::<Vec<_>>()},
                "variables": rows(trace.get_variables()).into_iter().map(|item| json!({"name":item.label.as_str(),"value":item.value.as_str()})).collect::<Vec<_>>(),
                "panes": {"opcodes": trace.get_show_opcodes(), "state": trace.get_show_state(), "console": trace.get_show_console(), "compact": trace.get_compact(), "opcodesWidth": trace.get_opcodes_width(), "stateWidth": trace.get_state_width(), "consoleHeight": trace.get_console_height()},
                "savedFolder": trace.get_saved_folder().as_str(),
                "console": trace.get_console_text().as_str().chars().rev().take(4000).collect::<String>().chars().rev().collect::<String>(),
                "saved": rows(trace.get_saved()).into_iter().map(|item| json!({"name":item.query.as_str(),"label":item.label.as_str(),"size":item.detail.as_str()})).collect::<Vec<_>>(),
    })
}

/// The pairing token and approval details are drawn only in those overlays,
/// so a capture is refused while either is visible.
fn screenshot(ui: &MainWindow) -> Result<Value, application::AppError> {
    let automation = ui.global::<AutomationData>();
    if automation.get_visible() || !automation.get_approval_id().is_empty() || !automation.get_pairing_token().is_empty() {
        return Err(application::AppError::new("screenshot_blocked", "Close the AI access panel and resolve approvals before a screenshot."));
    }
    let pixels = ui.window().take_snapshot().map_err(|_| application::AppError::new("screenshot_failed", "The window could not be captured."))?;
    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png, pixels.width(), pixels.height());
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|_| application::AppError::new("screenshot_failed", "PNG encoding failed."))?;
        writer.write_image_data(pixels.as_bytes()).map_err(|_| application::AppError::new("screenshot_failed", "PNG encoding failed."))?;
    }
    use base64::Engine;
    Ok(json!({"width":pixels.width(),"height":pixels.height(),"pngBase64":base64::engine::general_purpose::STANDARD.encode(png)}))
}
