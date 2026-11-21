//! History view in Explorer: rows from `.chasm/history.jsonl` and the
//! Repeat / Debug / To macro actions.
use super::history::{self, Entry};
use super::session::DesktopState;
use super::{HistoryData, HistoryLogic, HistoryRow, MainWindow};
use slint::{ComponentHandle, Weak};
use std::sync::{Arc, Mutex};

type State = Arc<Mutex<DesktopState>>;

/// Entries shown under the current filter, newest first. Filter 0 is this
/// network (untagged entries count as unknown, not foreign); 3 is everything.
fn visible(state: &DesktopState, filter: i32) -> Vec<Entry> {
    let current = state.network_identity.as_ref().map(|identity| identity.key());
    history::load(&state.workspace)
        .into_iter()
        .filter(|entry| match filter {
            0 => entry.network.as_deref().map(|network| Some(network) == current.as_deref()).unwrap_or(true),
            1 => entry.kind == "send",
            2 => entry.status != "ok",
            _ => true,
        })
        .collect()
}

pub fn render(ui: &MainWindow, state: &DesktopState) {
    let data = ui.global::<HistoryData>();
    let labels = super::address_book::labels(state);
    let rows: Vec<HistoryRow> = visible(state, data.get_filter())
        .iter()
        .map(|entry| {
            let target = labels.get(&entry.address.to_ascii_lowercase()).cloned().unwrap_or_else(|| short(&entry.address));
            let value = if entry.value.is_empty() || entry.value == "0" { String::new() } else { format!("  value {} wei", entry.value) };
            let (usable, note) = entry_network_note(state, entry);
            HistoryRow {
                title: format!("{} {}.{}", if entry.kind == "send" { "SEND" } else { "CALL" }, entry.contract, entry.function).into(),
                detail: format!("{}  {target}  args {}{value}  from {}", history::when(entry.time), if entry.arguments.is_empty() { "[]" } else { entry.arguments.as_str() }, labels.get(&entry.from.to_ascii_lowercase()).cloned().unwrap_or_else(|| short(&entry.from))).into(),
                status: entry.status.clone().into(),
                summary: entry.summary.clone().into(),
                can_debug: !entry.tx.is_empty() || !entry.data.is_empty(),
                usable,
                note: note.into(),
            }
        })
        .collect();
    if data.get_selected() >= rows.len() as i32 {
        data.set_selected(-1);
    }
    data.set_rows(super::model(rows));
    data.set_file(history::path(&state.workspace).display().to_string().trim_start_matches(r"\\?\").into());
}

/// Whether the entry may repeat, debug or become a macro step on the network
/// that runs now, with the reason when it may not. Untagged entries predate
/// profiles and are treated as unknown, not foreign.
fn entry_network_note(state: &DesktopState, entry: &Entry) -> (bool, String) {
    let Some(network) = &entry.network else {
        return (true, String::new());
    };
    let Some(identity) = state.network_identity.as_ref() else {
        return (true, String::new());
    };
    if network == &identity.key() {
        return (true, String::new());
    }
    let recorded = super::network::NetworkTag::from_key(network).map(|tag| tag.describe()).unwrap_or_else(|| network.clone());
    let current = identity.tag().describe();
    (false, format!("recorded on {recorded}; the current network is {current}. Repeat and Debug would hit another chain."))
}

fn short(address: &str) -> String {
    if address.len() > 12 { format!("{}…{}", &address[..6], &address[address.len() - 4..]) } else { address.to_string() }
}

pub fn wire(ui: &MainWindow, state: &State, runtime: tokio::runtime::Handle) {
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<HistoryLogic>().on_set_filter(move |filter| {
        if let Some(ui) = weak.upgrade() {
            ui.global::<HistoryData>().set_filter(filter.clamp(0, 3));
            ui.global::<HistoryData>().set_selected(-1);
        }
        super::paint(&weak, &held);
    });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<HistoryLogic>().on_clear(move || {
        let result = history::clear(&held.lock().unwrap().workspace);
        if let Some(ui) = weak.upgrade() {
            ui.global::<HistoryData>().set_status(match result {
                Ok(()) => "History cleared.".into(),
                Err(error) => format!("Could not clear history: {error}").into(),
            });
        }
        super::paint(&weak, &held);
    });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<HistoryLogic>().on_repeat(move |index| {
        let message = with_entry(&weak, &held, index, |state, entry| repeat(state, entry));
        report(&weak, &held, message);
        super::panels::focus_current();
    });
    let weak = ui.as_weak();
    let held = state.clone();
    let rt = runtime.clone();
    ui.global::<HistoryLogic>().on_debug(move |index| {
        let entry = with_entry(&weak, &held, index, |_, entry| Ok(entry.clone()));
        match entry {
            Ok(entry) if !entry.tx.is_empty() => {
                if let Some(ui) = weak.upgrade() {
                    ui.global::<super::AppData>().set_debugger_mode(0);
                }
                super::native_trace::load(weak.clone(), held.clone(), rt.clone(), entry.tx);
            }
            Ok(entry) => {
                // A simulated call: replay it through the active tab, which
                // Repeat has just pointed at the same target and request.
                let repeated = repeat(&mut held.lock().unwrap(), &entry);
                match repeated {
                    Ok(_) => {
                        {
                            let mut state = held.lock().unwrap();
                            let call = serde_json::json!({"from": entry.from, "to": entry.address, "data": entry.data, "value": format!("{:#x}", entry.value.parse::<u128>().unwrap_or(0)), "gas": "0x989680"});
                            if let Some(view) = state.current_contract_view_mut() {
                                view.reverted_call = Some(call);
                            }
                        }
                        if let Some(ui) = weak.upgrade() {
                            ui.global::<super::AppData>().set_debugger_mode(0);
                        }
                        super::native_trace::load(weak.clone(), held.clone(), rt.clone(), String::new());
                    }
                    Err(error) => report(&weak, &held, Err(error)),
                }
            }
            Err(error) => report(&weak, &held, Err(error)),
        }
    });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<HistoryLogic>().on_to_macro(move |index| {
        let entry = with_entry(&weak, &held, index, |_, entry| Ok(entry.clone()));
        let message = entry.and_then(|entry| {
            let ui = weak.upgrade().ok_or("The window closed.")?;
            super::native_macro::add_history_step(&ui, &held, &entry)
        });
        report(&weak, &held, message);
    });
}

fn with_entry<T>(weak: &Weak<MainWindow>, held: &State, index: i32, action: impl FnOnce(&mut DesktopState, &Entry) -> Result<T, String>) -> Result<T, String> {
    let filter = weak.upgrade().map(|ui| ui.global::<HistoryData>().get_filter()).unwrap_or(0);
    let mut state = held.lock().unwrap();
    if state.transition_pending() || !state.workspace_ready {
        return Err("Wait for the current operation to finish.".into());
    }
    let entries = visible(&state, filter);
    let entry = usize::try_from(index).ok().and_then(|index| entries.get(index)).ok_or("That history entry is gone. Refresh the list.")?.clone();
    // The row buttons are disabled for another network's entries; this guard
    // backs them up for callers that arrive through the control API.
    let (usable, note) = entry_network_note(&state, &entry);
    if !usable {
        return Err(format!("That entry was {note}"));
    }
    action(&mut state, &entry)
}

fn report(weak: &Weak<MainWindow>, held: &State, message: Result<String, String>) {
    if let Some(ui) = weak.upgrade() {
        ui.global::<HistoryData>().set_status(match message { Ok(text) | Err(text) => text }.into());
    }
    super::paint(weak, held);
}

/// Open the deployment's tab and refill the request from the entry.
pub fn repeat(state: &mut DesktopState, entry: &Entry) -> Result<String, String> {
    let index = state
        .deployments
        .iter()
        .position(|deployment| deployment.address.eq_ignore_ascii_case(&entry.address) && deployment.name == entry.contract)
        .ok_or_else(|| format!("{} at {} is not a deployment on this chain any more (the node may have restarted). Deploy it again, then repeat.", entry.contract, short(&entry.address)))?;
    let id = state.deployment_id(index).ok_or("The deployment is missing.")?;
    if !state.select_deployment_by_id(&id) {
        return Err(std::mem::take(&mut state.notice));
    }
    let owner = state.current_document_id().ok_or("The contract tab did not open.")?;
    let functions = state
        .contracts
        .iter()
        .find(|contract| contract.name == entry.contract)
        .map(|contract| contract.functions.clone())
        .unwrap_or_default();
    let position = functions
        .iter()
        .position(|function| format!("{}({})", function.name, function.params.iter().map(|(_, kind)| kind.as_str()).collect::<Vec<_>>().join(",")) == entry.function)
        .ok_or_else(|| format!("{} no longer has {}. Recompile or pick another entry.", entry.contract, entry.function))?;
    state.select_document_function(&owner, position as i32);
    let doc = state.documents.iter_mut().find(|doc| doc.id == owner).ok_or("The contract tab closed.")?;
    let view = &mut doc.contract_view;
    view.arguments = entry.arguments.clone();
    view.value = if entry.value.is_empty() { "0".into() } else { entry.value.clone() };
    view.value_amount = if view.value == "0" { String::new() } else { view.value.clone() };
    view.value_unit = 0;
    view.value_error.clear();
    view.call_status = format!("Request refilled from history ({}). Press {} to run it again.", history::when(entry.time), if entry.kind == "send" { "Send" } else { "Read / Simulate" });
    view.revision = view.revision.wrapping_add(1);
    state.view_revision = state.view_revision.wrapping_add(1);
    state.restore_contract_view();
    state.open_activity(0);
    Ok(format!("Refilled {}.{} in its contract tab.", entry.contract, entry.function))
}
