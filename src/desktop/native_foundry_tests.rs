use super::super::session::DesktopState;
use super::*;
use serde_json::json;
use std::{
    fs,
    sync::{Arc, Mutex},
    time::Duration,
};

const SOURCE: &str = "pragma solidity 0.8.28; contract Tithe {}\n";
const CONFIG: &str = "[profile.default]\nsolc='0.8.28'\nevm_version='cancun'\n";

fn fixture() -> (tempfile::TempDir, Arc<Mutex<DesktopState>>) {
    let project = tempfile::tempdir().unwrap();
    fs::create_dir(project.path().join("src")).unwrap();
    fs::write(project.path().join("src/Tithe.sol"), SOURCE).unwrap();
    fs::write(project.path().join("foundry.toml"), CONFIG).unwrap();
    let root = fs::canonicalize(project.path()).unwrap();
    let state = Arc::new(Mutex::new(DesktopState::new(root, 1.0)));
    (project, state)
}

fn reviewed(controller: &FoundryController) -> FoundryPreflight {
    let ticket = controller.begin_review().unwrap();
    let review = foundry_build::preflight(&ticket.workspace.path).unwrap();
    controller.finish_review(&ticket, Ok(review.clone()));
    controller
        .confirm_trust_from_local_ui(ticket.generation, &review.digest)
        .unwrap();
    review
}

fn begin(controller: &FoundryController, review: &FoundryPreflight) -> (Ticket, CancellationToken) {
    let generation = controller.view().unwrap().review.unwrap().generation;
    let (ticket, _, cancel) = controller.begin_build(generation, &review.digest).unwrap();
    (ticket, cancel)
}

async fn wait_for(controller: &FoundryController, phase: FoundryPhase) -> FoundryView {
    let mut changes = controller.subscribe();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let view = controller.view().unwrap();
            if view.phase == phase {
                return view;
            }
            assert!(
                !matches!(
                    view.phase,
                    FoundryPhase::Failed | FoundryPhase::Stale | FoundryPhase::Closed
                ),
                "{}",
                view.message
            );
            changes.changed().await.unwrap();
        }
    })
    .await
    .expect("Foundry controller did not reach the expected phase")
}

fn artifact(linked: bool) -> FoundryArtifactSummary {
    FoundryArtifactSummary {
        id: "0xabc".into(),
        source: "src/Tithe.sol".into(),
        contract: "Tithe".into(),
        abi: json!([]),
        abi_keccak256: "0xabi".into(),
        creation_bytecode: Some("0x6001".into()),
        runtime_bytecode: Some("0x6002".into()),
        creation_keccak256: Some("0xcreation".into()),
        runtime_keccak256: Some("0xruntime".into()),
        creation_source_map: Some("0:1:0".into()),
        runtime_source_map: Some("0:1:0".into()),
        storage_layout: Some(json!({})),
        method_identifiers: Some(json!({})),
        creation_link_references: Some(json!({})),
        runtime_link_references: Some(json!({})),
        immutable_references: Some(json!({})),
        linked,
    }
}

#[tokio::test]
async fn confirmation_requires_current_review_digest_and_generation() {
    let (project, state) = fixture();
    let controller = FoundryController::for_session(&state, Handle::current());
    let other_host = FoundryController::for_session(&state, Handle::current());
    assert!(Arc::ptr_eq(&controller, &other_host));
    assert_eq!(controller.view().unwrap().phase, FoundryPhase::Idle);
    controller.request_review().unwrap();
    let first = wait_for(&controller, FoundryPhase::AwaitingTrust)
        .await
        .review
        .unwrap();
    assert!(first.profile_summary.contains("solc: 0.8.28"));
    assert!(first.trust_warning.contains("not a sandbox"));
    assert_eq!(first.sources.len(), 1);
    assert!(controller
        .request_build(first.generation, &first.digest)
        .is_err());
    assert!(controller
        .confirm_trust_from_local_ui(first.generation, "wrong")
        .is_err());
    controller
        .confirm_trust_from_local_ui(first.generation, &first.digest)
        .unwrap();
    fs::write(
        project.path().join("src/Tithe.sol"),
        "pragma solidity 0.8.28; contract Changed {}\n",
    )
    .unwrap();
    controller.request_review().unwrap();
    let second = wait_for(&controller, FoundryPhase::AwaitingTrust)
        .await
        .review
        .unwrap();
    assert_ne!(first.digest, second.digest);
    assert_ne!(first.generation, second.generation);
    assert!(!second.trusted);
    assert!(controller
        .confirm_trust_from_local_ui(first.generation, &first.digest)
        .is_err());
    assert!(controller
        .request_build(second.generation, &second.digest)
        .is_err());
    controller
        .confirm_trust_from_local_ui(second.generation, &second.digest)
        .unwrap();
    assert!(controller.view().unwrap().can_build);
}

#[tokio::test]
async fn changed_source_after_confirmation_fails_before_compiler_lookup() {
    let (project, state) = fixture();
    let controller = FoundryController::for_session(&state, Handle::current());
    let review = reviewed(&controller);
    let generation = controller.view().unwrap().review.unwrap().generation;
    fs::write(
        project.path().join("src/Tithe.sol"),
        "pragma solidity 0.8.28; contract Changed {}\n",
    )
    .unwrap();
    controller
        .request_build(generation, &review.digest)
        .unwrap();
    let view = wait_for(&controller, FoundryPhase::Failed).await;
    assert!(
        view.message.contains("changed after review"),
        "{}",
        view.message
    );
    assert!(view.report.is_none());
    assert!(!view.can_build);
}

#[tokio::test]
async fn cancel_discards_late_result_and_blocks_stale_generation() {
    let (_project, state) = fixture();
    let controller = FoundryController::for_session(&state, Handle::current());
    let review = reviewed(&controller);
    let (ticket, cancel) = begin(&controller, &review);
    assert!(!controller.view().unwrap().can_close_or_replace_workspace);
    assert!(controller.cancel(ticket.generation - 1).is_err());
    controller.cancel(ticket.generation).unwrap();
    assert!(cancel.is_cancelled());
    assert_eq!(controller.view().unwrap().phase, FoundryPhase::Cancelling);
    controller.finish_build(&ticket, Err("Foundry build cancelled.".into()));
    assert_eq!(controller.view().unwrap().phase, FoundryPhase::Cancelled);
    let next_review = reviewed(&controller);
    let (next, _) = begin(&controller, &next_review);
    controller.finish_build(&ticket, Err("late failure".into()));
    assert_eq!(controller.view().unwrap().generation, next.generation);
    assert_eq!(controller.view().unwrap().phase, FoundryPhase::Building);
    controller.cancel(next.generation).unwrap();
    controller.finish_build(&next, Err("Foundry build cancelled.".into()));
}

#[tokio::test]
async fn workspace_replacement_cancels_and_discards_late_result() {
    let (_first, state) = fixture();
    let (second, _) = fixture();
    let controller = FoundryController::for_session(&state, Handle::current());
    let review = reviewed(&controller);
    let (ticket, cancel) = begin(&controller, &review);
    {
        let mut session = state.lock().unwrap();
        session.workspace = fs::canonicalize(second.path()).unwrap();
        session.workspace_epoch += 1;
    }
    controller.finish_build(&ticket, Err("late failure".into()));
    assert!(cancel.is_cancelled());
    let view = controller.view().unwrap();
    assert_eq!(view.phase, FoundryPhase::Stale);
    assert!(view.review.is_none() && view.report.is_none());
    assert!(controller
        .confirm_trust_from_local_ui(ticket.generation, &review.digest)
        .is_err());
}

#[tokio::test]
async fn unavailable_build_and_unlinked_artifacts_are_ineligible() {
    let (_project, state) = fixture();
    let controller = FoundryController::for_session(&state, Handle::current());
    let review = reviewed(&controller);
    assert!(controller
        .resolve_artifact(0, "0xabc", ArtifactPurpose::Abi)
        .await
        .is_err());
    assert!(controller.resolve_build_info(0, "unknown").await.is_err());
    let linked = vec![artifact(true)];
    for purpose in [
        ArtifactPurpose::Abi,
        ArtifactPurpose::Deploy,
        ArtifactPurpose::Debugger,
    ] {
        assert!(checked_artifact(true, true, &linked, "0xabc", purpose).is_ok());
        assert!(checked_artifact(false, true, &linked, "0xabc", purpose).is_err());
        assert!(checked_artifact(true, false, &linked, "0xabc", purpose).is_err());
        assert!(checked_artifact(true, true, &linked, "Tithe", purpose).is_err());
    }
    let ambiguous = vec![artifact(true), artifact(true)];
    assert!(checked_artifact(true, true, &ambiguous, "0xabc", ArtifactPurpose::Abi).is_err());
    let unlinked = vec![artifact(false)];
    assert!(checked_artifact(true, true, &unlinked, "0xabc", ArtifactPurpose::Abi).is_ok());
    assert!(checked_artifact(true, true, &unlinked, "0xabc", ArtifactPurpose::Deploy).is_err());
    assert!(checked_artifact(true, true, &unlinked, "0xabc", ArtifactPurpose::Debugger).is_err());
    assert_eq!(review.settings.solc, "0.8.28");
}

#[test]
fn missing_compiler_error_is_actionable_without_fallback() {
    let error = actionable_error(
        "solc 0.8.28 is not installed locally. Offline mode will not install it.".into(),
    );
    assert!(error.contains("0.8.28"));
    assert!(error.contains("Install the exact compiler separately"));
    assert!(error.contains("will not download"));
}
