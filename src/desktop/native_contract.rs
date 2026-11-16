use super::call_result::TraceState;
use super::contract_state::{ContractView, Target};
use super::session::{DesktopState, Document};
use super::{ContractData, ContractFunction, ContractLogic, ContractViewData, MainWindow};
use serde_json::Value;
use slint::{ComponentHandle, Weak};
use std::sync::{Arc, Mutex};

type State = Arc<Mutex<DesktopState>>;

fn document_index(state: &DesktopState, owner: &str) -> Option<usize> {
    if owner.is_empty() {
        usize::try_from(state.active_document)
            .ok()
            .filter(|index| state.documents.get(*index).is_some_and(|doc| doc.kind == 0))
    } else {
        state
            .documents
            .iter()
            .position(|doc| doc.id == owner && doc.kind == 0)
    }
}

fn edit_view(state: &mut DesktopState, owner: &str, edit: impl FnOnce(&mut ContractView)) -> bool {
    if state.transition_pending() || !state.workspace_ready {
        return false;
    }
    let Some(index) = document_index(state, owner) else {
        return false;
    };
    edit(&mut state.documents[index].contract_view);
    state.view_revision = state.view_revision.wrapping_add(1);
    state.restore_contract_view();
    true
}

pub fn wire(ui: &MainWindow, state: &State, runtime: tokio::runtime::Handle) {
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<ContractLogic>()
        .on_section(move |owner, section| {
            edit_view(&mut held.lock().unwrap(), owner.as_str(), |view| {
                view.section = section.clamp(0, 2)
            });
            super::paint(&weak, &held);
        });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<ContractLogic>()
        .on_select_function(move |owner, selected| {
            let mut state = held.lock().unwrap();
            if state.transition_pending() || state.send_busy {
                return;
            }
            let Some(index) = document_index(&state, owner.as_str()) else {
                return;
            };
            let owner = state.documents[index].id.clone();
            state.select_document_function(&owner, selected);
            drop(state);
            super::paint(&weak, &held);
        });
    for field in 0..4 {
        let held = state.clone();
        let weak = ui.as_weak();
        let callback = move |owner: slint::SharedString, value: slint::SharedString| {
            if value.len() > 64 * 1024 {
                return;
            }
            let mut state = held.lock().unwrap();
            edit_view(&mut state, owner.as_str(), |view| match field {
                0 => {
                    view.arguments = value.to_string();
                    view.revision = view.revision.wrapping_add(1);
                }
                1 => {
                    view.value = value.to_string();
                    let (amount, unit) = super::request_form::from_wei(&view.value);
                    view.value_amount = amount;
                    view.value_unit = unit as i32;
                    view.value_error.clear();
                    view.revision = view.revision.wrapping_add(1);
                }
                2 => {
                    view.slot = value.to_string();
                    view.storage_generation = view.storage_generation.wrapping_add(1);
                    view.storage_busy = false;
                }
                _ => view.attach_address = value.to_string(),
            });
            drop(state);
            super::paint(&weak, &held);
        };
        match field {
            0 => ui.global::<ContractLogic>().on_arguments_edited(callback),
            1 => ui.global::<ContractLogic>().on_value_edited(callback),
            2 => ui.global::<ContractLogic>().on_slot_edited(callback),
            _ => ui
                .global::<ContractLogic>()
                .on_attach_address_edited(callback),
        }
    }
    let held = state.clone();
    let weak = ui.as_weak();
    ui.global::<ContractLogic>().on_field_edited(move |owner, index, text| {
        if text.len() > 64 * 1024 {
            return;
        }
        let mut state = held.lock().unwrap();
        let Some(doc_index) = document_index(&state, owner.as_str()) else { return };
        let Some(function) = abi_function(&state, &state.documents[doc_index]) else { return };
        let mut values = super::request_form::split(&function, &state.documents[doc_index].contract_view.arguments);
        let Some(slot) = values.get_mut(index as usize) else { return };
        *slot = text.to_string();
        let joined = super::request_form::join(&function, &values);
        edit_view(&mut state, owner.as_str(), |view| {
            view.arguments = joined;
            view.revision = view.revision.wrapping_add(1);
        });
        drop(state);
        super::paint(&weak, &held);
    });
    let held = state.clone();
    let weak = ui.as_weak();
    ui.global::<ContractLogic>().on_amount_edited(move |owner, amount, unit| {
        if amount.len() > 100 {
            return;
        }
        let mut state = held.lock().unwrap();
        edit_view(&mut state, owner.as_str(), |view| {
            view.value_amount = amount.to_string();
            view.value_unit = unit.clamp(0, 2);
            match super::request_form::to_wei(&view.value_amount, view.value_unit as usize) {
                Ok(wei) => {
                    view.value = wei;
                    view.value_error.clear();
                }
                Err(error) => view.value_error = error,
            }
            view.revision = view.revision.wrapping_add(1);
        });
        drop(state);
        super::paint(&weak, &held);
    });
    let held = state.clone();
    ui.global::<ContractLogic>()
        .on_scroll_changed(move |owner, part, position| {
            if !position.is_finite()
                || !(-1_000_000.0..=0.0).contains(&position)
                || ![
                    "functions",
                    "request",
                    "details",
                    "result-simple",
                    "result-raw",
                    "result-trace",
                    "storage",
                ]
                .contains(&part.as_str())
            {
                return;
            }
            let Ok(mut state) = held.try_lock() else {
                return;
            };
            if state.transition_pending() {
                return;
            }
            if let Some(index) = document_index(&state, owner.as_str()) {
                state.documents[index]
                    .contract_view
                    .scroll
                    .insert(part.to_string(), position);
            }
        });
    let held = state.clone();
    let weak = ui.as_weak();
    ui.global::<ContractLogic>()
        .on_size_changed(move |owner, part, value| {
            if !value.is_finite() {
                return;
            }
            let mut state = held.lock().unwrap();
            if state.transition_pending() {
                return;
            }
            let Some(index) = document_index(&state, owner.as_str()) else {
                return;
            };
            let view = &mut state.documents[index].contract_view;
            match part.as_str() {
                "function-width" => view.function_width = value.clamp(160.0, 640.0),
                "result-height" => view.result_height = value.clamp(100.0, 1600.0),
                _ => return,
            }
            drop(state);
            super::paint(&weak, &held);
        });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<ContractLogic>().on_interact_view(move |owner, mode| {
        edit_view(&mut held.lock().unwrap(), owner.as_str(), |view| view.interact_view = mode.clamp(0, 1));
        super::paint(&weak, &held);
    });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<ContractLogic>()
        .on_result_view(move |owner, mode| {
            edit_view(&mut held.lock().unwrap(), owner.as_str(), |view| {
                view.result_view = mode.clamp(0, 2)
            });
            super::paint(&weak, &held);
        });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<ContractLogic>()
        .on_attach(move |owner, address| {
            let mut state = held.lock().unwrap();
            if state.transition_pending() || state.send_busy || state.deploy_busy {
                return;
            }
            let Some(index) = document_index(&state, owner.as_str()) else {
                return;
            };
            match address.as_str().parse::<ethers::types::Address>() {
                Err(_) => {
                    state.notice = "Attach requires an exact 20-byte contract address.".into()
                }
                Ok(address) => {
                    let contract = state.documents[index]
                        .contract_name()
                        .unwrap_or_default()
                        .to_string();
                    let network_epoch = state.network_epoch;
                    let view = &mut state.documents[index].contract_view;
                    view.target = Some(Target {
                        contract,
                        address: format!("{address:#x}"),
                        deployment_id: None,
                        network_epoch,
                    });
                    view.revision = view.revision.wrapping_add(1);
                    view.storage_generation = view.storage_generation.wrapping_add(1);
                    view.storage_busy = false;
                    view.storage.clear();
                    view.storage_raw = None;
                    view.storage_status.clear();
                    view.call_generation = view.call_generation.wrapping_add(1);
                    view.result = None;
                    view.call_status.clear();
                    view.attach_address.clear();
                    state.view_revision = state.view_revision.wrapping_add(1);
                    state.restore_contract_view();
                }
            }
            drop(state);
            super::paint(&weak, &held);
        });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<ContractLogic>().on_deploy(move |owner| {
        let mut state = held.lock().unwrap();
        if state.transition_pending() {
            return;
        }
        let Some(index) = document_index(&state, owner.as_str()) else {
            return;
        };
        let id = state.documents[index].id.clone();
        state.open_deploy_for_document(&id);
        drop(state);
        super::panels::focus_current();
        super::paint(&weak, &held);
    });
    let weak = ui.as_weak();
    let held = state.clone();
    let rt = runtime.clone();
    ui.global::<ContractLogic>().on_read(move |owner| {
        execute(
            weak.clone(),
            held.clone(),
            rt.clone(),
            owner.to_string(),
            false,
        )
    });
    let weak = ui.as_weak();
    let held = state.clone();
    let rt = runtime.clone();
    ui.global::<ContractLogic>().on_send(move |owner| {
        execute(
            weak.clone(),
            held.clone(),
            rt.clone(),
            owner.to_string(),
            true,
        )
    });
    let weak = ui.as_weak();
    let held = state.clone();
    let rt = runtime.clone();
    ui.global::<ContractLogic>().on_read_slot(move |owner| {
        read_storage(weak.clone(), held.clone(), rt.clone(), owner.to_string())
    });
    let weak = ui.as_weak();
    let held = state.clone();
    let rt = runtime.clone();
    ui.global::<ContractLogic>()
        .on_trace(move |owner| trace(weak.clone(), held.clone(), rt.clone(), owner.to_string()));
    let weak = ui.as_weak();
    ui.global::<ContractLogic>().on_save_output(move |_, text| {
        if let Some(ui) = weak.upgrade() {
            ui.global::<super::Logic>().invoke_save_output(text);
        }
    });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<super::Logic>().on_open_storage(move || {
        edit_view(&mut held.lock().unwrap(), "", |view| view.section = 1);
        super::paint(&weak, &held);
    });
    // The old toolbar callbacks share the typed service, not a second cast/signing path.
    let weak = ui.as_weak();
    let held = state.clone();
    let rt = runtime.clone();
    ui.global::<super::Logic>().on_simulate_call(move || {
        execute(weak.clone(), held.clone(), rt.clone(), String::new(), false)
    });
    let weak = ui.as_weak();
    let held = state.clone();
    let rt = runtime.clone();
    ui.global::<super::Logic>()
        .on_send_call(move || execute(weak.clone(), held.clone(), rt.clone(), String::new(), true));
    let weak = ui.as_weak();
    let held = state.clone();
    let rt = runtime.clone();
    ui.global::<super::Logic>().on_read_storage(move || {
        read_storage(weak.clone(), held.clone(), rt.clone(), String::new())
    });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<super::Logic>()
        .on_trace(move || trace(weak.clone(), held.clone(), runtime.clone(), String::new()));
}

#[derive(Clone)]
struct CallStamp {
    document: String,
    target: Target,
    generation: u64,
    revision: u64,
}

fn call_is_current(view: &ContractView, stamp: &CallStamp) -> bool {
    view.call_generation == stamp.generation
        && view.revision == stamp.revision
        && view.target.as_ref() == Some(&stamp.target)
}

fn functions_for<'a>(
    state: &'a DesktopState,
    doc: &Document,
) -> &'a [super::session::FunctionInfo] {
    let Some(name) = doc.contract_name() else {
        return &[];
    };
    let mut matches = state
        .contracts
        .iter()
        .filter(|contract| contract.name == name);
    match (matches.next(), matches.next()) {
        (Some(contract), None) => &contract.functions,
        _ => &[],
    }
}

/// Full ABI entry of the tab's selected function, for typed request fields.
fn abi_function(state: &DesktopState, doc: &Document) -> Option<ethers::abi::Function> {
    let selected = functions_for(state, doc).get(doc.contract_view.selected_function as usize)?;
    let signature = format!("{}({})", selected.name, selected.params.iter().map(|(_, kind)| kind.as_str()).collect::<Vec<_>>().join(","));
    let name = doc.contract_name()?;
    let mut contracts = state.contracts.iter().filter(|contract| contract.name == name);
    let contract = contracts.next().filter(|_| contracts.next().is_none())?;
    use ethers::abi::FunctionExt;
    contract.abi.functions().find(|function| function.abi_signature() == signature).cloned()
}

fn execute(
    weak: Weak<MainWindow>,
    held: State,
    runtime: tokio::runtime::Handle,
    owner: String,
    send: bool,
) {
    let prepared = (|| -> Result<_, String> {
        let mut state = held.lock().unwrap();
        if state.transition_pending()
            || !state.workspace_ready
            || state.send_busy
            || state.deploy_busy
            || state.wallet_busy
        {
            return Err(
                "Wait for the current wallet, confirmation or transaction operation.".into(),
            );
        }
        let index = document_index(&state, &owner).ok_or("Open a contract document first.")?;
        let doc = &state.documents[index];
        let view = &doc.contract_view;
        let target = view
            .target
            .clone()
            .ok_or("Choose a deployment or Attach an address in Details.")?;
        if target.network_epoch != state.network_epoch {
            return Err("This deployment belongs to an earlier network context.".into());
        }
        let selected = functions_for(&state, doc)
            .get(view.selected_function as usize)
            .ok_or("Select a function first.")?;
        if send && ["view", "pure"].contains(&selected.mutability.as_str()) {
            return Err("Read-only functions use Read / Simulate, not Send.".into());
        }
        let signature = format!(
            "{}({})",
            selected.name,
            selected
                .params
                .iter()
                .map(|(_, kind)| kind.as_str())
                .collect::<Vec<_>>()
                .join(",")
        );
        let mut contracts = state
            .contracts
            .iter()
            .filter(|contract| contract.name == target.contract);
        let abi = contracts
            .next()
            .filter(|_| contracts.next().is_none())
            .ok_or("Compile output is missing or ambiguous.")?
            .abi
            .clone();
        let request = super::contract_service::prepare(
            &target.contract,
            &target.address,
            &signature,
            &abi,
            &view.arguments,
            &view.value,
            &state.wallet_address,
            state.workspace_epoch,
            state.network_epoch,
            state.rpc.clone(),
        )?;
        let (key, owned, lease) = if send {
            let owned = state
                .owned_node
                .clone()
                .filter(|node| node.valid())
                .ok_or("The owned local Anvil node is unavailable; signing is disabled.")?;
            let key = state.wallet_key.clone().ok_or(
                "Unlock a local keystore or choose an Anvil development account before Send.",
            )?;
            let lease = state.mutation_gate.try_acquire().ok_or(
                "Another local chain mutation is in progress. Wait for it to finish before sending.",
            )?;
            (Some(key), Some(owned), Some(lease))
        } else {
            (None, None, None)
        };
        let doc = &mut state.documents[index];
        doc.contract_view.call_generation = doc.contract_view.call_generation.wrapping_add(1);
        doc.contract_view.call_status = if send {
            "Submitting transaction…"
        } else {
            "Reading pinned contract state…"
        }
        .into();
        let stamp = CallStamp {
            document: doc.id.clone(),
            target,
            generation: doc.contract_view.call_generation,
            revision: doc.contract_view.revision,
        };
        let token = state.begin_request(
            super::jobs::Operation::Call,
            if send { "send call" } else { "simulate call" },
            false,
        );
        state.send_busy = true;
        Ok((stamp, request, key, token, owned, lease))
    })();
    let (stamp, request, key, token, owned, lease) = match prepared {
        Ok(value) => value,
        Err(error) => {
            held.lock().unwrap().notice = error;
            super::paint(&weak, &held);
            return;
        }
    };
    super::paint(&weak, &held);
    runtime.spawn(async move {
        let _lease = lease;
        let result = match key {
            Some(key) => super::contract_service::send(request, key, owned.unwrap()).await,
            None => super::contract_service::simulate(request).await,
        };
        let _ = slint::invoke_from_event_loop(move || {
            let mut state = held.lock().unwrap();
            let summary = result.as_ref().map(|result| result.simple.clone()).map_err(Clone::clone);
            let tx = result.as_ref().ok().and_then(|result| result.context.request.get("transactionHash").and_then(Value::as_str).map(str::to_string));
            state.finish_request(&token, &summary, tx.clone());
            if let Ok(result) = &result {
                let status = if result.context.request["submissionState"] == "unknown" { Some("unknown") }
                    else if result.raw["result"]["status"] == "0x0" || result.raw.get("error").is_some_and(|error| !error.is_null()) { Some("failed") }
                    else { None };
                if let Some(status) = status {
                    if let Some(job) = state.jobs.iter_mut().find(|job| job.id == token.id) { job.status = status.into(); }
                }
            }
            if state.workspace_epoch == token.workspace_epoch && state.network_epoch == token.network_epoch {
                state.send_busy = false;
                if let Some(tx) = tx { state.last_tx = tx; }
                if state.active {
                    if let Some(doc) = state.documents.iter_mut().find(|doc| doc.id == stamp.document) {
                        if call_is_current(&doc.contract_view, &stamp) {
                            match result {
                                Ok(result) => {
                                    doc.contract_view.result = Some(result);
                                    doc.contract_view.result_generation = stamp.generation;
                                    doc.contract_view.result_view = 0;
                                    doc.contract_view.interact_view = 1;
                                    doc.contract_view.call_status = "Call finished. Result context is captured below.".into();
                                }
                                Err(error) => doc.contract_view.call_status = format!("Last attempt failed: {error}\nPrevious completed result, if any, is unchanged."),
                            }
                        }
                    }
                    state.restore_contract_view();
                }
            }
            drop(state);
            super::paint(&weak, &held);
        });
    });
}

fn storage_is_current(view: &ContractView, generation: u64, target: &Target, slot: &str) -> bool {
    view.storage_generation == generation
        && view.slot == slot
        && view.target.as_ref() == Some(target)
}

fn read_storage(
    weak: Weak<MainWindow>,
    held: State,
    runtime: tokio::runtime::Handle,
    owner: String,
) {
    let prepared = {
        let mut state = held.lock().unwrap();
        if state.transition_pending() || !state.workspace_ready {
            return;
        }
        let Some(index) = document_index(&state, &owner) else {
            return;
        };
        let network = state.network_epoch;
        let epoch = state.workspace_epoch;
        let rpc = state.rpc.clone();
        let doc = &mut state.documents[index];
        let Some(target) = doc
            .contract_view
            .target
            .clone()
            .filter(|target| target.network_epoch == network)
        else {
            return;
        };
        let view = &mut doc.contract_view;
        if view.storage_busy { return; }
        view.storage_generation = view.storage_generation.wrapping_add(1);
        view.storage_busy = true;
        view.storage_status = "Reading the selected slot at a pinned block…".into();
        (
            doc.id.clone(),
            epoch,
            network,
            rpc,
            target,
            view.slot.clone(),
            view.storage_generation,
        )
    };
    let (id, epoch, network, rpc, target, slot, generation) = prepared;
    super::paint(&weak, &held);
    runtime.spawn(async move {
        let result = super::contract_service::storage(&rpc, &target.address, &slot).await;
        let _ = slint::invoke_from_event_loop(move || {
            let mut state = held.lock().unwrap();
            if !state.active || state.workspace_epoch != epoch || state.network_epoch != network {
                return;
            }
            if let Some(doc) = state.documents.iter_mut().find(|doc| doc.id == id) {
                let view = &mut doc.contract_view;
                if storage_is_current(view, generation, &target, &slot) {
                    view.storage_busy = false;
                    match result {
                        Ok((text, raw)) => {
                            view.storage = text;
                            view.storage_raw = Some(raw);
                            view.storage_status = "Pinned slot read complete.".into();
                        }
                        Err(error) => view.storage_status = format!("Slot read failed: {error}"),
                    }
                }
            }
            state.restore_contract_view();
            drop(state);
            super::paint(&weak, &held);
        });
    });
}

fn trace(weak: Weak<MainWindow>, held: State, runtime: tokio::runtime::Handle, owner: String) {
    let prepared = {
        let mut state = held.lock().unwrap();
        if state.transition_pending() || !state.workspace_ready {
            return;
        }
        let Some(index) = document_index(&state, &owner) else {
            return;
        };
        let rpc = state.rpc.clone();
        let epoch = state.workspace_epoch;
        let network = state.network_epoch;
        let doc = &mut state.documents[index];
        let generation = doc.contract_view.result_generation;
        let Some(result) = doc.contract_view.result.as_mut() else {
            return;
        };
        if matches!(result.trace, TraceState::Loading) {
            return;
        }
        result.trace = TraceState::Loading;
        (
            doc.id.clone(),
            epoch,
            network,
            rpc,
            result.clone(),
            generation,
        )
    };
    let (id, epoch, network, rpc, result, generation) = prepared;
    super::paint(&weak, &held);
    runtime.spawn(async move {
        let trace = super::contract_service::trace(&rpc, &result).await;
        let _ = slint::invoke_from_event_loop(move || {
            let mut state = held.lock().unwrap();
            if !state.active || state.workspace_epoch != epoch || state.network_epoch != network {
                return;
            }
            if let Some(view) = state
                .documents
                .iter_mut()
                .find(|doc| doc.id == id)
                .map(|doc| &mut doc.contract_view)
            {
                if view.result_generation == generation {
                    if let Some(current) = view.result.as_mut() {
                        current.trace = match trace {
                            Ok(text) => TraceState::Ready(text),
                            Err(reason) => TraceState::Unavailable(reason),
                        };
                    }
                }
            }
            drop(state);
            super::paint(&weak, &held);
        });
    });
}

pub fn presentation(state: &DesktopState, doc: &Document) -> ContractViewData {
    let view = &doc.contract_view;
    let functions = functions_for(state, doc);
    let function = functions.get(view.selected_function as usize);
    let ready = state.workspace_ready
        && !state.transition_pending()
        && !state.send_busy
        && !state.deploy_busy;
    let mut data = ContractViewData {
        contract_name: doc.contract_name().unwrap_or(&doc.title).into(),
        functions: super::model(
            functions
                .iter()
                .map(|function| ContractFunction {
                    name: function.name.clone().into(),
                    inputs: function.inputs.clone().into(),
                    mutability: function.mutability.clone().into(),
                })
                .collect(),
        ),
        selected_function: view.selected_function,
        selected_name: function
            .map(|function| format!("{}({})", function.name, function.inputs))
            .unwrap_or_default()
            .into(),
        mutability: function
            .map(|function| function.mutability.as_str())
            .unwrap_or_default()
            .into(),
        call_busy: state.send_busy,
        has_result: view.result.is_some(),
        storage_busy: view.storage_busy,
        can_read_storage: ready
            && view
                .target
                .as_ref()
                .is_some_and(|target| target.network_epoch == state.network_epoch),
        call_status: view.call_status.clone().into(),
        storage_status: view.storage_status.clone().into(),
        can_read: ready
            && function.is_some()
            && view
                .target
                .as_ref()
                .is_some_and(|target| target.network_epoch == state.network_epoch),
        send_enabled: ready
            && state.wallet_key.is_some()
            && state.owned_node.as_ref().is_some_and(|node| node.valid())
            && function
                .is_some_and(|function| !["view", "pure"].contains(&function.mutability.as_str())),
        section: view.section,
        result_view: view.result_view,
        interact_view: view.interact_view,
        target_bound: view.target.is_some(),
        address: view
            .target
            .as_ref()
            .map(|target| target.address.as_str())
            .unwrap_or_default()
            .into(),
        target_note: if view.target.is_some() {
            "Target is bound to this tab. Details holds the full address."
        } else {
            "Choose a deployment, or attach an address in Details."
        }
        .into(),
        arguments: view.arguments.clone().into(),
        value: view.value.clone().into(),
        value_amount: view.value_amount.clone().into(),
        value_unit: view.value_unit,
        value_error: view.value_error.clone().into(),
        fields: super::model(
            abi_function(state, doc)
                .map(|function| super::request_form::fields(&function, &view.arguments))
                .unwrap_or_default()
                .into_iter()
                .map(|field| super::RequestField {
                    name: field.name.into(),
                    kind: field.kind.into(),
                    value: field.value.into(),
                    hint: field.hint.into(),
                    placeholder: field.placeholder.into(),
                    error: field.error.into(),
                })
                .collect(),
        ),
        slot: view.slot.clone().into(),
        attach_address: view.attach_address.clone().into(),
        storage_output: view.storage.clone().into(),
        functions_scroll_y: view.scroll.get("functions").copied().unwrap_or(0.0),
        request_scroll_y: view.scroll.get("request").copied().unwrap_or(0.0),
        details_scroll_y: view.scroll.get("details").copied().unwrap_or(0.0),
        result_scroll_y: view
            .scroll
            .get(match view.result_view {
                1 => "result-raw",
                2 => "result-trace",
                _ => "result-simple",
            })
            .copied()
            .unwrap_or(0.0),
        storage_scroll_y: view.scroll.get("storage").copied().unwrap_or(0.0),
        function_width: view.function_width,
        result_height: view.result_height,
        details: format!(
            "Contract: {}\nAddress: {}\nNetwork epoch: {}\nDocument: {}",
            doc.contract_name().unwrap_or(&doc.title),
            view.target
                .as_ref()
                .map(|target| target.address.as_str())
                .unwrap_or("not attached"),
            view.target
                .as_ref()
                .map(|target| target.network_epoch)
                .unwrap_or(state.network_epoch),
            doc.id
        )
        .into(),
        ..Default::default()
    };
    if let Some(result) = &view.result {
        data.simple_output = format!(
            "{}\n{}\nFunction: {}\nPinned block: {}\n\n{}",
            result.context.contract,
            result.context.address,
            result.context.signature,
            result.context.source["block"]["number"],
            result.simple
        )
        .into();
        data.raw_output = serde_json::to_string_pretty(&serde_json::json!({"context":{"contract":result.context.contract,"address":result.context.address,"function":result.context.signature,"workspaceEpoch":result.context.workspace_epoch,"networkEpoch":result.context.network_epoch,"source":result.context.source},"request":result.context.request,"response":result.raw})).unwrap_or_default().into();
        data.trace_busy = matches!(result.trace, TraceState::Loading);
        data.trace_output = match &result.trace {
            TraceState::Empty => {
                "Trace is loaded on demand for this captured result. No transaction is sent.".into()
            }
            TraceState::Loading => "Tracing the captured context…".into(),
            TraceState::Ready(text) | TraceState::Unavailable(text) | TraceState::Failed(text) => {
                text.clone().into()
            }
        };
    } else {
        data.simple_output = "No call result yet. Read or simulate a function.".into();
        data.raw_output = "No RPC result captured.".into();
        data.trace_output = "No captured call or transaction to trace.".into();
    }
    data
}

pub fn render_document(ui: &MainWindow, state: &DesktopState, doc: &Document) {
    let view = presentation(state, doc);
    project_legacy(ui, &view);
}

pub fn render(ui: &MainWindow, state: &DesktopState) {
    if let Some(index) = document_index(state, "") {
        render_document(ui, state, &state.documents[index]);
    } else {
        project_legacy(
            ui,
            &ContractViewData {
                selected_function: -1,
                ..Default::default()
            },
        );
    }
}

fn project_legacy(ui: &MainWindow, view: &ContractViewData) {
    let data = ui.global::<ContractData>();
    data.set_contract_name(view.contract_name.clone());
    data.set_functions(view.functions.clone());
    data.set_selected_function(view.selected_function);
    data.set_selected_name(view.selected_name.clone());
    data.set_mutability(view.mutability.clone());
    data.set_call_busy(view.call_busy);
    data.set_call_status(view.call_status.clone());
    data.set_storage_status(view.storage_status.clone());
    data.set_can_read(view.can_read);
    data.set_send_enabled(view.send_enabled);
    data.set_section(view.section);
    data.set_result_view(view.result_view);
    data.set_address(view.address.clone());
    data.set_target_bound(view.target_bound);
    data.set_target_note(view.target_note.clone());
    data.set_arguments(view.arguments.clone());
    data.set_value(view.value.clone());
    data.set_slot(view.slot.clone());
    data.set_attach_address(view.attach_address.clone());
    data.set_storage_output(view.storage_output.clone());
    data.set_details(view.details.clone());
    data.set_simple_output(view.simple_output.clone());
    data.set_raw_output(view.raw_output.clone());
    data.set_trace_output(view.trace_output.clone());
    data.set_trace_busy(view.trace_busy);
}

#[cfg(test)]
mod tests {
    use super::*;
    fn target(address: &str) -> Target {
        Target {
            contract: "Bank".into(),
            address: address.into(),
            deployment_id: Some(address.into()),
            network_epoch: 1,
        }
    }

    #[test]
    fn call_and_storage_completion_require_exact_owner_request_and_target() {
        let mut view = ContractView::new(Some(target("A")));
        view.call_generation = 2;
        view.revision = 4;
        let stamp = CallStamp {
            document: "bank-A".into(),
            target: target("A"),
            generation: 2,
            revision: 4,
        };
        assert!(call_is_current(&view, &stamp));
        view.target = Some(target("B"));
        assert!(!call_is_current(&view, &stamp));
        view.target = Some(target("A"));
        view.call_generation += 1;
        assert!(!call_is_current(&view, &stamp));
        view.storage_generation = 3;
        assert!(storage_is_current(&view, 3, &target("A"), "0"));
        view.slot = "1".into();
        assert!(!storage_is_current(&view, 3, &target("A"), "0"));
        assert!(!storage_is_current(&view, 2, &target("A"), "1"));
        assert!(!storage_is_current(&view, 3, &target("B"), "1"));
    }

    #[test]
    fn qualified_owner_never_falls_back_to_active_document() {
        let mut state = DesktopState::welcome();
        state
            .documents
            .push(Document::new("Bank".into(), "".into(), 0));
        state
            .documents
            .push(Document::new("Bank 2".into(), "".into(), 0));
        state.active_document = 1;
        assert_eq!(document_index(&state, &state.documents[0].id), Some(0));
        assert_eq!(document_index(&state, "missing"), None);
        assert_eq!(state.active_document, 1);
    }
}
