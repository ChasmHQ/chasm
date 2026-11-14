use super::application::{self, Activity, Command};
use super::control::{Hub, Scopes, UiRequest};
use super::control_http::ControlHost;
use super::{
    AppData, AutomationClient, AutomationData, AutomationLogic, DesktopState, Logic, MainWindow,
};
use slint::{ComponentHandle, Weak};
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
                    let result = hub.authorize(&principal, "read").and_then(|_| {
                        let state = held.lock().unwrap();
                        if !state.active {
                            Err(application::AppError::new(
                                "stale_context",
                                "The workspace changed before the query ran.",
                            ))
                        } else {
                            Ok(application::query(&state, &query))
                        }
                    });
                    let _ = reply.send(result);
                }
                UiRequest::Command {
                    principal,
                    request,
                    reply,
                } => {
                    let result = hub.execute_ui(&principal, &request, &mut held.lock().unwrap());
                    let switch = result
                        .as_ref()
                        .ok()
                        .is_some_and(|value| value["effect"] == "workspace_switch");
                    if result.is_ok() { super::panels::focus_current(); }
                    super::paint(&weak, &held);
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
            let scopes = Scopes {
                read: true,
                ui: mode == 1 || mode == 3,
                analyze: mode == 2 || mode == 3,
            };
            match hub_pair.pair_local(scopes) {
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
                .is_some_and(|value| value["effect"] == "workspace_switch");
            if let Err(error) = result {
                ui.global::<AutomationData>()
                    .set_message(error.message.into());
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
        .map(|client| {
            let mut scopes = vec!["read"];
            if client["scopes"]["ui"] == true {
                scopes.push("UI control");
            }
            if client["scopes"]["analyze"] == true {
                scopes.push("analysis");
            }
            AutomationClient {
                id: client["id"].as_str().unwrap_or_default().into(),
                scopes: scopes.join(", ").into(),
            }
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
        local(&weak, &held, Command::Navigate { activity });
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
