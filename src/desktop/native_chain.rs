//! Timeline and cheatcodes view: snapshot points and their reverts, and the
//! cheat form. Every action holds the chain mutation gate while it runs.
use super::chain_tools::{self, Cheat};
use super::session::DesktopState;
use super::{ChainData, ChainLogic, MainWindow, TimelineRow};
use slint::{ComponentHandle, Weak};
use std::sync::{Arc, Mutex};

type State = Arc<Mutex<DesktopState>>;

pub fn render(ui: &MainWindow, state: &DesktopState) {
    let data = ui.global::<ChainData>();
    data.set_busy(state.rpc_busy);
    let labels = super::address_book::labels(state);
    data.set_points(super::model(
        state
            .timeline
            .iter()
            .rev()
            .map(|point| TimelineRow {
                label: label_with_names(&point.label, &labels).into(),
                detail: format!("block {}  ·  {}  ·  snapshot {}", point.block, super::history::when(point.time), point.id).into(),
            })
            .collect(),
    ));
}

/// Start a chain change: holds the gate and marks the chain busy.
fn begin(held: &State) -> Result<(String, super::mutation_gate::MutationLease), String> {
    let mut state = held.lock().unwrap();
    if state.transition_pending() || !state.workspace_ready || state.rpc_busy {
        return Err("Wait for the current chain operation to finish.".into());
    }
    let lease = state
        .mutation_gate
        .try_acquire()
        .ok_or("Another local chain mutation is in progress. Wait for it to finish.")?;
    state.rpc_busy = true;
    Ok((state.rpc.clone(), lease))
}

fn finish(weak: &Weak<MainWindow>, held: &State, message: String) {
    held.lock().unwrap().rpc_busy = false;
    if let Some(ui) = weak.upgrade() {
        ui.global::<ChainData>().set_status(message.into());
        ui.global::<super::Logic>().invoke_fetch_block();
    }
    super::paint(weak, held);
}

pub fn wire(ui: &MainWindow, state: &State, runtime: tokio::runtime::Handle) {
    let weak = ui.as_weak();
    let held = state.clone();
    let rt = runtime.clone();
    ui.global::<ChainLogic>().on_snapshot(move || {
        let (rpc, lease) = match begin(&held) {
            Ok(value) => value,
            Err(error) => return report(&weak, error),
        };
        let keys = held.lock().unwrap().current_deployment_keys();
        let weak = weak.clone();
        let held = held.clone();
        rt.spawn(async move {
            let _lease = lease;
            let result = chain_tools::snapshot(&rpc).await;
            let _ = slint::invoke_from_event_loop(move || {
                let message = match result {
                    Ok((id, block)) => {
                        held.lock().unwrap().push_timeline(id.clone(), block, "manual snapshot".into(), keys);
                        format!("Snapshot {id} at block {block}.")
                    }
                    Err(error) => error,
                };
                finish(&weak, &held, message);
            });
        });
    });
    let weak = ui.as_weak();
    let held = state.clone();
    let rt = runtime.clone();
    ui.global::<ChainLogic>().on_revert_to(move |row| {
        // Rows are shown newest first.
        let point = {
            let state = held.lock().unwrap();
            usize::try_from(row).ok().and_then(|row| state.timeline.len().checked_sub(row + 1)).and_then(|index| state.timeline.get(index).cloned().map(|point| (index, point)))
        };
        let Some((index, point)) = point else { return report(&weak, "That snapshot is gone. The list was refreshed.".into()) };
        let (rpc, lease) = match begin(&held) {
            Ok(value) => value,
            Err(error) => return report(&weak, error),
        };
        let weak = weak.clone();
        let held = held.clone();
        rt.spawn(async move {
            let _lease = lease;
            let result = chain_tools::revert(&rpc, &point.id).await;
            let _ = slint::invoke_from_event_loop(move || {
                let message = match result {
                    Ok(true) => {
                        held.lock().unwrap().revert_to_point(index);
                        format!("Reverted to before \"{}\" (block {}).", point.label.trim_start_matches("before "), point.block)
                    }
                    Ok(false) => {
                        held.lock().unwrap().forget_points_from(index);
                        "The node no longer has that snapshot (the node restarted or an earlier revert removed it). It was removed from the list.".into()
                    }
                    Err(error) => error,
                };
                finish(&weak, &held, message);
            });
        });
    });
    let weak = ui.as_weak();
    let held = state.clone();
    let rt = runtime.clone();
    ui.global::<ChainLogic>().on_apply_cheat(move || {
        let Some(ui) = weak.upgrade() else { return };
        let data = ui.global::<ChainData>();
        let cheat = Cheat { action: data.get_action().to_string(), address: data.get_address().to_string(), value: data.get_value().to_string(), slot: data.get_slot().to_string() };
        apply(&weak, &held, &rt, cheat);
    });
}

pub fn apply(weak: &Weak<MainWindow>, held: &State, runtime: &tokio::runtime::Handle, cheat: Cheat) {
    let (calls, label) = match chain_tools::plan(&cheat) {
        Ok(value) => value,
        Err(error) => return report(weak, error),
    };
    let (rpc, lease) = match begin(held) {
        Ok(value) => value,
        Err(error) => return report(weak, error),
    };
    let keys = held.lock().unwrap().current_deployment_keys();
    let weak = weak.clone();
    let held = held.clone();
    runtime.spawn(async move {
        let _lease = lease;
        let result = async {
            let point = chain_tools::snapshot(&rpc).await?;
            for (method, params) in calls {
                chain_tools::call(&rpc, method, params).await?;
            }
            Ok::<_, String>(point)
        }
        .await;
        let _ = slint::invoke_from_event_loop(move || {
            let message = match result {
                Ok((id, block)) => {
                    let mut state = held.lock().unwrap();
                    state.push_timeline(id, block, format!("before {label}"), keys);
                    state.push_log(format!("Cheat: {label}"));
                    format!("Done: {label}.")
                }
                Err(error) => error,
            };
            finish(&weak, &held, message);
        });
    });
}

fn report(weak: &Weak<MainWindow>, message: String) {
    if let Some(ui) = weak.upgrade() {
        ui.global::<ChainData>().set_status(message.into());
    }
}

/// Cheat labels carry shortened addresses; name the known ones.
fn label_with_names(label: &str, labels: &std::collections::HashMap<String, String>) -> String {
    for (address, name) in labels {
        let short = format!("{}…{}", &address[..6], &address[address.len() - 4..]);
        if label.contains(&short) {
            return label.replace(&short, &format!("{short} ({name})"));
        }
    }
    label.to_string()
}
