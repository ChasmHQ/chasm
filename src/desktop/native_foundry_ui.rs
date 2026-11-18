use super::{FoundryArtifactRow, FoundryData, FoundryLogic, MainWindow};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::native_foundry::{FoundryController, FoundryPhase};
use super::session::DesktopState;

type State = Arc<Mutex<DesktopState>>;

fn phase_label(phase: FoundryPhase) -> &'static str {
    match phase {
        FoundryPhase::Idle => "Idle",
        FoundryPhase::Reviewing => "Reviewing",
        FoundryPhase::AwaitingTrust => "Awaiting local trust",
        FoundryPhase::Ready => "Ready to build",
        FoundryPhase::Building => "Building",
        FoundryPhase::Cancelling => "Cancelling",
        FoundryPhase::Complete => "Complete",
        FoundryPhase::Cancelled => "Cancelled",
        FoundryPhase::Failed => "Failed",
        FoundryPhase::Stale => "Stale",
        FoundryPhase::Closed => "Closed",
    }
}

pub fn wire(
    ui: &MainWindow,
    state: &State,
    runtime: tokio::runtime::Handle,
) -> Arc<FoundryController> {
    let controller = FoundryController::for_session(state, runtime.clone());
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<FoundryLogic>().on_review({
        let controller = controller.clone();
        move || {
            if let Err(error) = controller.request_review() {
                held.lock().unwrap().notice = error;
            }
            super::paint(&weak, &held);
        }
    });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<FoundryLogic>().on_confirm_trust({
        let controller = controller.clone();
        move || {
            let result = weak.upgrade().ok_or("The Foundry window is closed.".to_string()).and_then(|ui| {
                let data = ui.global::<FoundryData>();
                let generation = data.get_review_generation().parse::<u64>()
                    .map_err(|_| "The displayed Foundry review is unavailable. Review again.".to_string())?;
                controller.confirm_trust_from_local_ui(generation, data.get_digest().as_str())
            });
            if let Err(error) = result {
                held.lock().unwrap().notice = error;
            }
            super::paint(&weak, &held);
        }
    });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<FoundryLogic>().on_build({
        let controller = controller.clone();
        move || {
            let result = controller.view().and_then(|view| {
                let review = view.review.ok_or("Review and confirm this workspace first.".to_string())?;
                controller.request_build(review.generation, &review.digest).map(|_| ())
            });
            if let Err(error) = result {
                held.lock().unwrap().notice = error;
            }
            super::paint(&weak, &held);
        }
    });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<FoundryLogic>().on_cancel({
        let controller = controller.clone();
        move || {
            let result = controller.view().and_then(|view| controller.cancel(view.generation));
            if let Err(error) = result {
                held.lock().unwrap().notice = error;
            }
            super::paint(&weak, &held);
        }
    });
    let mut changes = controller.subscribe();
    let weak_controller = Arc::downgrade(&controller);
    let weak = ui.as_weak();
    let held = state.clone();
    // Slint handles can only be upgraded on the event loop, so a closed host
    // reports back through this flag and its watcher stops on the next change.
    let host_closed = Arc::new(AtomicBool::new(false));
    runtime.spawn(async move {
        while changes.changed().await.is_ok() {
            if host_closed.load(Ordering::Acquire) {
                return;
            }
            let Some(controller) = weak_controller.upgrade() else { return };
            let weak = weak.clone();
            let held = held.clone();
            let host_closed = host_closed.clone();
            let _ = slint::invoke_from_event_loop(move || {
                let Some(ui) = weak.upgrade() else {
                    host_closed.store(true, Ordering::Release);
                    return;
                };
                render_controller(&ui, &controller);
                super::paint(&weak, &held);
            });
        }
    });
    render_controller(ui, &controller);
    controller
}

fn render_controller(ui: &MainWindow, controller: &FoundryController) {
    let data = ui.global::<FoundryData>();
    let Ok(view) = controller.view() else {
        data.set_phase("Unavailable".into());
        data.set_message("Trusted Foundry controller is unavailable.".into());
        data.set_can_review(false);
        data.set_can_confirm(false);
        data.set_can_build(false);
        data.set_can_cancel(false);
        return;
    };
    data.set_phase(phase_label(view.phase).into());
    data.set_message(view.message.into());
    data.set_can_review(view.can_review);
    data.set_can_confirm(view.can_confirm_trust);
    data.set_can_build(view.can_build);
    data.set_can_cancel(view.can_cancel);
    if let Some(review) = view.review {
        data.set_review_generation(review.generation.to_string().into());
        data.set_digest(review.digest.into());
        data.set_profile(review.profile_summary.into());
        data.set_trust_warning(review.trust_warning.into());
        data.set_trusted(review.trusted);
        data.set_source_summary(format!("{} reviewed Solidity source file(s).", review.sources.len()).into());
    } else {
        data.set_review_generation("".into());
        data.set_digest("".into());
        data.set_profile("".into());
        data.set_trust_warning("".into());
        data.set_trusted(false);
        data.set_source_summary("".into());
    }
    if let Some(report) = view.report {
        data.set_report_summary(format!(
            "Settings verified: {} · Sources verified: {} · Exact build eligible: {}",
            report.settings_verified, report.sources_verified, report.exact_build_eligible
        ).into());
        data.set_warnings(report.warnings.join("\n").into());
        data.set_artifact_json(serde_json::to_string_pretty(&report.artifacts.iter().map(|artifact| {
            serde_json::json!({"id":artifact.id,"source":artifact.source,"contract":artifact.contract,"deployable":artifact.deploy_eligible})
        }).collect::<Vec<_>>()).unwrap_or_default().into());
        data.set_artifacts(ModelRc::new(VecModel::from(
            report.artifacts.into_iter().map(|artifact| FoundryArtifactRow {
                id: artifact.id.into(),
                name: artifact.contract.into(),
                source: artifact.source.into(),
                status: if artifact.deploy_eligible { "Deployable" } else if artifact.abi_eligible { "ABI only" } else { "Ineligible" }.into(),
                detail: artifact.ineligibility.join(" ").into(),
            }).collect::<Vec<_>>(),
        )));
    } else {
        data.set_report_summary("".into());
        data.set_warnings("".into());
        data.set_artifact_json("".into());
        data.set_artifacts(ModelRc::new(VecModel::from(Vec::<FoundryArtifactRow>::new())));
    }
}
