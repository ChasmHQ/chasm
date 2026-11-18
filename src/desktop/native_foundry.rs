use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock, Weak},
};

#[cfg(test)]
use serde_json::Value;
use tokio::{runtime::Handle, sync::watch};
use tokio_util::sync::CancellationToken;

use super::foundry_build::{
    self, FoundryArtifactSummary, FoundryBuildReport, FoundryPreflight, FoundrySettings,
    SourceManifestEntry,
};
use super::session::DesktopState;

type Session = Arc<Mutex<DesktopState>>;
type Registry = HashMap<usize, Weak<FoundryController>>;

static CONTROLLERS: OnceLock<Mutex<Registry>> = OnceLock::new();

#[cfg(test)]
#[path = "native_foundry_tests.rs"]
mod tests;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FoundryPhase {
    #[default]
    Idle,
    Reviewing,
    AwaitingTrust,
    Ready,
    Building,
    Cancelling,
    Complete,
    Cancelled,
    Failed,
    Stale,
    Closed,
}

#[derive(Clone, Debug)]
/// Read by tests and kept for the review UI; not every field is shown yet.
#[allow(dead_code)]
pub struct FoundryReviewView {
    pub generation: u64,
    pub workspace: PathBuf,
    pub config_path: PathBuf,
    pub digest: String,
    pub settings: FoundrySettings,
    pub profile_summary: String,
    pub sources: Vec<SourceManifestEntry>,
    pub trust_warning: String,
    pub trusted: bool,
}

#[derive(Clone, Debug)]
/// Read by tests and kept for the review UI; not every field is shown yet.
#[allow(dead_code)]
pub struct FoundryArtifactView {
    pub id: String,
    pub source: String,
    pub contract: String,
    pub abi_keccak256: String,
    pub creation_keccak256: Option<String>,
    pub runtime_keccak256: Option<String>,
    pub linked: bool,
    pub abi_eligible: bool,
    pub deploy_eligible: bool,
    pub debugger_data_available: bool,
    pub ineligibility: Vec<String>,
}

#[derive(Clone, Debug)]
/// Read by tests and kept for the review UI; not every field is shown yet.
#[allow(dead_code)]
pub struct FoundryReportView {
    pub generation: u64,
    pub digest: String,
    pub build_info_id: String,
    pub settings_verified: bool,
    pub sources_verified: bool,
    /// This verifies the reviewed build inputs, not an on-chain deployment.
    pub exact_build_eligible: bool,
    pub artifacts: Vec<FoundryArtifactView>,
    pub warnings: Vec<String>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Clone, Debug)]
pub struct FoundryView {
    pub phase: FoundryPhase,
    pub generation: u64,
    pub message: String,
    pub review: Option<FoundryReviewView>,
    pub report: Option<FoundryReportView>,
    pub can_review: bool,
    pub can_confirm_trust: bool,
    pub can_build: bool,
    pub can_cancel: bool,
    #[allow(dead_code)]
    pub can_close_or_replace_workspace: bool,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArtifactPurpose {
    Abi,
    Deploy,
    Debugger,
}

#[cfg(test)]
#[derive(Clone, Debug)]
/// Returned to tests that check the resolver; callers keep the record whole.
#[allow(dead_code)]
pub struct ResolvedArtifact {
    pub workspace_epoch: u64,
    pub generation: u64,
    pub digest: String,
    pub build_info_id: String,
    pub artifact: FoundryArtifactSummary,
}

#[cfg(test)]
#[derive(Clone, Debug)]
/// Returned to tests that check the resolver; callers keep the record whole.
#[allow(dead_code)]
pub struct ResolvedBuildInfo {
    pub workspace_epoch: u64,
    pub generation: u64,
    pub digest: String,
    pub id: String,
    pub content: Value,
    /// This path is valid only while the same controller retains this report.
    pub path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildProvenance {
    pub workspace_epoch: u64,
    pub generation: u64,
    pub digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkspaceStamp {
    epoch: u64,
    path: PathBuf,
}

#[derive(Clone, Debug)]
struct Ticket {
    generation: u64,
    workspace: WorkspaceStamp,
}

struct StoredReport {
    generation: u64,
    build_info_id: String,
    report: FoundryBuildReport,
}

#[derive(Default)]
struct State {
    phase: FoundryPhase,
    generation: u64,
    workspace: Option<WorkspaceStamp>,
    review_generation: Option<u64>,
    review: Option<FoundryPreflight>,
    trusted_digest: Option<String>,
    cancel: Option<CancellationToken>,
    report: Option<StoredReport>,
    message: String,
}

/// One controller is shared by every host of one desktop session. It does not
/// own the session, and workers hold only a weak controller reference.
pub struct FoundryController {
    session: Weak<Mutex<DesktopState>>,
    runtime: Handle,
    state: Mutex<State>,
    changes: watch::Sender<u64>,
}

impl FoundryController {
    /// The first call fixes the runtime for this session. Later hosts receive
    /// the same controller rather than starting independent builds.
    pub fn for_session(session: &Session, runtime: Handle) -> Arc<Self> {
        let registry = CONTROLLERS.get_or_init(|| Mutex::new(HashMap::new()));
        let mut registry = registry.lock().unwrap_or_else(|error| error.into_inner());
        registry.retain(|_, controller| controller.strong_count() != 0);
        let key = Arc::as_ptr(session) as usize;
        if let Some(controller) = registry.get(&key).and_then(Weak::upgrade) {
            if controller
                .session
                .upgrade()
                .is_some_and(|current| Arc::ptr_eq(&current, session))
            {
                return controller;
            }
        }
        let (changes, _) = watch::channel(0);
        let controller = Arc::new(Self {
            session: Arc::downgrade(session),
            runtime,
            state: Mutex::new(State::default()),
            changes,
        });
        registry.insert(key, Arc::downgrade(&controller));
        controller
    }

    /// Subscribe once per host and repaint on the Slint event loop when this changes.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    fn changed(&self) {
        self.changes.send_modify(|revision| {
            *revision = revision.wrapping_add(1);
        });
    }

    /// An explicit UI action. No caller-provided workspace path is accepted.
    pub fn request_review(self: &Arc<Self>) -> Result<u64, String> {
        let ticket = self.begin_review()?;
        self.changed();
        let generation = ticket.generation;
        let weak = Arc::downgrade(self);
        self.runtime.spawn(async move {
            let worker_controller = weak.clone();
            let worker_ticket = ticket.clone();
            let result = tokio::task::spawn_blocking(move || {
                if !worker_controller.upgrade().is_some_and(|controller| {
                    controller.operation_is_current(&worker_ticket, FoundryPhase::Reviewing)
                }) {
                    return Err("Foundry review cancelled before starting.".into());
                }
                foundry_build::preflight(&worker_ticket.workspace.path)
            })
            .await
            .unwrap_or_else(|error| Err(format!("Foundry review worker failed: {error}")));
            if let Some(controller) = weak.upgrade() {
                controller.finish_review(&ticket, result);
            }
        });
        Ok(generation)
    }

    fn begin_review(&self) -> Result<Ticket, String> {
        let session = self
            .session
            .upgrade()
            .ok_or("The desktop session is closed.")?;
        let session = session
            .lock()
            .map_err(|_| "The desktop session is unavailable.")?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| "The Foundry controller is unavailable.")?;
        reconcile(&session, &mut state);
        require_action(&session, &state)?;
        if matches!(
            state.phase,
            FoundryPhase::Reviewing | FoundryPhase::Building | FoundryPhase::Cancelling
        ) {
            return Err("Wait for the current Foundry operation or cancel it first.".into());
        }
        let workspace = current_workspace(&session).ok_or("Open a project first.")?;
        let generation = next_generation(&mut state)?;
        state.workspace = Some(workspace.clone());
        state.review_generation = None;
        state.review = None;
        state.trusted_digest = None;
        state.cancel = None;
        state.report = None;
        state.phase = FoundryPhase::Reviewing;
        state.message = "Reviewing the selected workspace. Forge has not been started.".into();
        Ok(Ticket {
            generation,
            workspace,
        })
    }

    fn finish_review(&self, ticket: &Ticket, result: Result<FoundryPreflight, String>) {
        let Some(session) = self.session.upgrade() else {
            return;
        };
        let Ok(session) = session.lock() else { return };
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if !ticket_is_current(&session, &state, ticket, FoundryPhase::Reviewing) {
            let changed = reconcile(&session, &mut state);
            drop(state);
            drop(session);
            if changed {
                self.changed();
            }
            return;
        }
        match result {
            Ok(review) if review.workspace == ticket.workspace.path => {
                state.review_generation = Some(ticket.generation);
                state.review = Some(review);
                state.phase = FoundryPhase::AwaitingTrust;
                state.message = "Review the effective profile, sources, digest and trust warning before confirming.".into();
            }
            Ok(_) => {
                state.phase = FoundryPhase::Failed;
                state.message = "The reviewed root does not match the selected workspace.".into();
            }
            Err(error) => {
                state.phase = FoundryPhase::Failed;
                state.message = error;
            }
        }
        drop(state);
        drop(session);
        self.changed();
    }

    /// Call only from the local confirmation action, with the values shown in
    /// that confirmation. This approval is consumed by one build attempt.
    pub fn confirm_trust_from_local_ui(&self, generation: u64, digest: &str) -> Result<(), String> {
        let session = self
            .session
            .upgrade()
            .ok_or("The desktop session is closed.")?;
        let session = session
            .lock()
            .map_err(|_| "The desktop session is unavailable.")?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| "The Foundry controller is unavailable.")?;
        reconcile(&session, &mut state);
        require_action(&session, &state)?;
        if state.phase != FoundryPhase::AwaitingTrust
            || state.review_generation != Some(generation)
            || state.review.as_ref().map(|review| review.digest.as_str()) != Some(digest)
        {
            return Err(
                "The displayed Foundry review is no longer current. Review it again.".into(),
            );
        }
        state.trusted_digest = Some(digest.to_owned());
        state.phase = FoundryPhase::Ready;
        state.message = "Trust confirmed for this digest and one local build attempt.".into();
        drop(state);
        drop(session);
        self.changed();
        Ok(())
    }

    /// Starts Forge only after a local confirmation for this exact review.
    pub fn request_build(self: &Arc<Self>, generation: u64, digest: &str) -> Result<u64, String> {
        let (ticket, review, cancel) = self.begin_build(generation, digest)?;
        self.changed();
        let build_generation = ticket.generation;
        let weak = Arc::downgrade(self);
        self.runtime.spawn(async move {
            let worker_controller = weak.clone();
            let worker_ticket = ticket.clone();
            let result = tokio::task::spawn_blocking(move || {
                let may_start = worker_controller
                    .upgrade()
                    .is_some_and(|controller| controller.may_start_build(&worker_ticket));
                if !may_start {
                    return Err("Foundry build cancelled before starting.".into());
                }
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| format!("Cannot start the local Foundry worker: {error}"))?;
                runtime.block_on(foundry_build::build(&review, true, cancel))
            })
            .await
            .unwrap_or_else(|error| Err(format!("The Foundry build worker failed: {error}")));
            if let Some(controller) = weak.upgrade() {
                controller.finish_build(&ticket, result);
            }
        });
        Ok(build_generation)
    }

    fn begin_build(
        &self,
        generation: u64,
        digest: &str,
    ) -> Result<(Ticket, FoundryPreflight, CancellationToken), String> {
        let session = self
            .session
            .upgrade()
            .ok_or("The desktop session is closed.")?;
        let session = session
            .lock()
            .map_err(|_| "The desktop session is unavailable.")?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| "The Foundry controller is unavailable.")?;
        reconcile(&session, &mut state);
        require_action(&session, &state)?;
        if state.phase != FoundryPhase::Ready
            || state.review_generation != Some(generation)
            || state.trusted_digest.as_deref() != Some(digest)
            || state.review.as_ref().map(|review| review.digest.as_str()) != Some(digest)
        {
            return Err("Confirm local trust for the exact current review before building.".into());
        }
        let review = state.review.clone().ok_or("Review the workspace first.")?;
        let workspace = state.workspace.clone().ok_or("Open a project first.")?;
        let generation = next_generation(&mut state)?;
        let cancel = CancellationToken::new();
        state.trusted_digest = None;
        state.cancel = Some(cancel.clone());
        state.report = None;
        state.phase = FoundryPhase::Building;
        state.message = "Running the reviewed local Foundry build offline.".into();
        Ok((
            Ticket {
                generation,
                workspace,
            },
            review,
            cancel,
        ))
    }

    fn operation_is_current(&self, ticket: &Ticket, phase: FoundryPhase) -> bool {
        let Some(session) = self.session.upgrade() else {
            return false;
        };
        let Ok(session) = session.lock() else {
            return false;
        };
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        let changed = reconcile(&session, &mut state);
        let current = ticket_is_current(&session, &state, ticket, phase);
        drop(state);
        drop(session);
        if changed {
            self.changed();
        }
        current
    }

    fn may_start_build(&self, ticket: &Ticket) -> bool {
        let Some(session) = self.session.upgrade() else {
            return false;
        };
        let Ok(session) = session.lock() else {
            return false;
        };
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        let current = ticket_is_current(&session, &state, ticket, FoundryPhase::Building)
            && state
                .cancel
                .as_ref()
                .is_some_and(|cancel| !cancel.is_cancelled());
        let changed = reconcile(&session, &mut state);
        drop(state);
        drop(session);
        if changed {
            self.changed();
        }
        current
    }

    fn finish_build(&self, ticket: &Ticket, result: Result<FoundryBuildReport, String>) {
        let Some(session) = self.session.upgrade() else {
            return;
        };
        let Ok(session) = session.lock() else { return };
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let current = ticket_is_current(&session, &state, ticket, FoundryPhase::Building)
            || ticket_is_current(&session, &state, ticket, FoundryPhase::Cancelling);
        if !current {
            let changed = reconcile(&session, &mut state);
            drop(state);
            drop(session);
            if changed {
                self.changed();
            }
            return;
        }
        let cancelling = state.phase == FoundryPhase::Cancelling;
        state.cancel = None;
        if cancelling {
            match result {
                Ok(_) => {
                    state.phase = FoundryPhase::Cancelled;
                    state.message =
                        "Cancellation was requested; the returned build output was discarded."
                            .into();
                }
                Err(error) if error.to_ascii_lowercase().contains("cancelled") => {
                    state.phase = FoundryPhase::Cancelled;
                    state.message = error;
                }
                Err(error) => {
                    state.phase = FoundryPhase::Failed;
                    state.message = format!(
                        "Cancellation was requested, but the build reported an error: {}",
                        actionable_error(error)
                    );
                }
            }
        } else {
            match result {
                Ok(report)
                    if state.review.as_ref().is_some_and(|review| {
                        report.workspace == ticket.workspace.path
                            && report.preflight_digest == review.digest
                            && report.settings == review.settings
                    }) =>
                {
                    let build_info_id = format!("build-{}", report.preflight_digest);
                    state.report = Some(StoredReport {
                        generation: ticket.generation,
                        build_info_id,
                        report,
                    });
                    state.phase = FoundryPhase::Complete;
                    state.message = "Foundry build finished. Inspect its verification and warnings before using an artifact.".into();
                }
                Ok(_) => {
                    state.phase = FoundryPhase::Failed;
                    state.message = "Foundry returned a report for a different workspace or review; it was discarded.".into();
                }
                Err(error) => {
                    state.phase = FoundryPhase::Failed;
                    state.message = actionable_error(error);
                }
            }
        }
        drop(state);
        drop(session);
        self.changed();
    }

    /// A stale host cannot cancel a newer operation.
    pub fn cancel(&self, generation: u64) -> Result<(), String> {
        let session = self
            .session
            .upgrade()
            .ok_or("The desktop session is closed.")?;
        let session = session
            .lock()
            .map_err(|_| "The desktop session is unavailable.")?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| "The Foundry controller is unavailable.")?;
        reconcile(&session, &mut state);
        if state.generation != generation {
            return Err("This Foundry operation is no longer current.".into());
        }
        match state.phase {
            FoundryPhase::Reviewing => {
                state.phase = FoundryPhase::Cancelled;
                state.message = "Review cancelled. Its result will be discarded.".into();
            }
            FoundryPhase::Building => {
                if let Some(cancel) = &state.cancel {
                    cancel.cancel();
                }
                state.phase = FoundryPhase::Cancelling;
                state.message =
                    "Cancelling the Foundry build. Waiting for the local process to stop.".into();
            }
            _ => return Err("There is no cancellable Foundry operation.".into()),
        }
        state.trusted_digest = None;
        drop(state);
        drop(session);
        self.changed();
        Ok(())
    }

    /// Cancel an active build and wait for the worker to return before tearing
    /// down the runtime or admitting work in a replacement workspace.
    pub async fn cancel_and_wait(&self) -> Result<(), String> {
        let mut changes = self.subscribe();
        loop {
            let view = self.view()?;
            match view.phase {
                FoundryPhase::Building => {
                    if self.cancel(view.generation).is_err() {
                        continue;
                    }
                }
                FoundryPhase::Cancelling => {}
                FoundryPhase::Reviewing => {
                    let _ = self.cancel(view.generation);
                    return Ok(());
                }
                _ => return Ok(()),
            }
            changes
                .changed()
                .await
                .map_err(|_| "The Foundry worker stopped without a completion notification.")?;
        }
    }

    /// After cancel_and_wait, call before the desktop marks a workspace inactive
    /// or replaces it. Drop also requests cancellation, but cannot await cleanup.
    pub fn shutdown(&self) {
        if let Ok(mut state) = self.state.lock() {
            if let Some(cancel) = state.cancel.take() {
                cancel.cancel();
            }
            state.review = None;
            state.review_generation = None;
            state.trusted_digest = None;
            state.report = None;
            state.workspace = None;
            state.phase = FoundryPhase::Closed;
            state.message = "The desktop session is closed.".into();
        }
        self.changed();
    }

    pub fn view(&self) -> Result<FoundryView, String> {
        let session = self
            .session
            .upgrade()
            .ok_or("The desktop session is closed.")?;
        let session = session
            .lock()
            .map_err(|_| "The desktop session is unavailable.")?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| "The Foundry controller is unavailable.")?;
        let changed = reconcile(&session, &mut state);
        let available = require_action(&session, &state).is_ok();
        let review = state.review.as_ref().and_then(|review| {
            state.review_generation.map(|generation| FoundryReviewView {
                generation,
                workspace: review.workspace.clone(),
                config_path: review.config_path.clone(),
                digest: review.digest.clone(),
                settings: review.settings.clone(),
                profile_summary: review.profile_summary(),
                sources: review.source_manifest.clone(),
                trust_warning: review.trust_warning.clone(),
                trusted: state.trusted_digest.as_deref() == Some(review.digest.as_str()),
            })
        });
        let report = state.report.as_ref().map(report_view);
        let view = FoundryView {
            phase: state.phase,
            generation: state.generation,
            message: state.message.clone(),
            review,
            report,
            can_review: available
                && !matches!(
                    state.phase,
                    FoundryPhase::Reviewing | FoundryPhase::Building | FoundryPhase::Cancelling
                ),
            can_confirm_trust: available && state.phase == FoundryPhase::AwaitingTrust,
            can_build: available && state.phase == FoundryPhase::Ready,
            can_cancel: matches!(
                state.phase,
                FoundryPhase::Reviewing | FoundryPhase::Building
            ),
            can_close_or_replace_workspace: !matches!(
                state.phase,
                FoundryPhase::Building | FoundryPhase::Cancelling
            ),
        };
        drop(state);
        drop(session);
        if changed {
            self.changed();
        }
        Ok(view)
    }

    #[cfg(test)]
    /// Resolves by opaque artifact ID, never by a caller-supplied path. Each
    /// lookup rechecks the reviewed workspace and the retained build generation.
    pub async fn resolve_artifact(
        &self,
        generation: u64,
        artifact_id: &str,
        purpose: ArtifactPurpose,
    ) -> Result<ResolvedArtifact, String> {
        let (ticket, review) = self.prepare_lookup(generation)?;
        self.recheck_lookup(&ticket, &review).await?;
        let session = self
            .session
            .upgrade()
            .ok_or("The desktop session is closed.")?;
        let session = session
            .lock()
            .map_err(|_| "The desktop session is unavailable.")?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| "The Foundry controller is unavailable.")?;
        reconcile(&session, &mut state);
        require_action(&session, &state)?;
        let stored = current_report(&session, &state, &ticket)?;
        let artifact = checked_artifact(
            stored.report.settings_verified,
            stored.report.sources_verified,
            &stored.report.artifacts,
            artifact_id,
            purpose,
        )?;
        Ok(ResolvedArtifact {
            workspace_epoch: ticket.workspace.epoch,
            generation,
            digest: stored.report.preflight_digest.clone(),
            build_info_id: stored.build_info_id.clone(),
            artifact: artifact.clone(),
        })
    }

    #[cfg(test)]
    /// The returned content is a copy. The path does not keep temporary output alive.
    pub async fn resolve_build_info(
        &self,
        generation: u64,
        build_info_id: &str,
    ) -> Result<ResolvedBuildInfo, String> {
        let (ticket, review) = self.prepare_lookup(generation)?;
        self.recheck_lookup(&ticket, &review).await?;
        let session = self
            .session
            .upgrade()
            .ok_or("The desktop session is closed.")?;
        let session = session
            .lock()
            .map_err(|_| "The desktop session is unavailable.")?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| "The Foundry controller is unavailable.")?;
        reconcile(&session, &mut state);
        require_action(&session, &state)?;
        let stored = current_report(&session, &state, &ticket)?;
        if stored.build_info_id != build_info_id {
            return Err("The exact build-info ID is not in the current build.".into());
        }
        if !stored.report.exact_target_eligible() {
            return Err("Build settings or sources did not match the review; exact build-info use is unavailable.".into());
        }
        Ok(ResolvedBuildInfo {
            workspace_epoch: ticket.workspace.epoch,
            generation,
            digest: stored.report.preflight_digest.clone(),
            id: stored.build_info_id.clone(),
            content: stored.report.build_info.clone(),
            path: stored.report.build_info_path.clone(),
        })
    }

    /// Run an in-process adapter against the retained, verified report without
    /// transferring its temporary-directory ownership. The callback must not
    /// re-enter this controller or wait on another thread that needs the session.
    pub async fn with_current_report<R>(
        &self,
        generation: u64,
        adapt: impl FnOnce(&FoundryBuildReport) -> R,
    ) -> Result<(BuildProvenance, R), String> {
        let (ticket, review) = self.prepare_lookup(generation)?;
        self.recheck_lookup(&ticket, &review).await?;
        let session = self
            .session
            .upgrade()
            .ok_or("The desktop session is closed.")?;
        let session = session
            .lock()
            .map_err(|_| "The desktop session is unavailable.")?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| "The Foundry controller is unavailable.")?;
        reconcile(&session, &mut state);
        require_action(&session, &state)?;
        let stored = current_report(&session, &state, &ticket)?;
        if !stored.report.exact_target_eligible() {
            return Err("Build settings or sources did not match the review; exact artifact use is unavailable.".into());
        }
        let provenance = BuildProvenance {
            workspace_epoch: ticket.workspace.epoch,
            generation,
            digest: stored.report.preflight_digest.clone(),
        };
        Ok((provenance, adapt(&stored.report)))
    }

    /// Revalidate captured provenance immediately before an adapter uses copied
    /// ABI, bytecode or build-info. This cannot lock an independently writable workspace.
    pub async fn validate_provenance(&self, provenance: &BuildProvenance) -> Result<(), String> {
        let (ticket, review) = self.prepare_lookup(provenance.generation)?;
        if ticket.workspace.epoch != provenance.workspace_epoch
            || review.digest != provenance.digest
        {
            return Err("The captured artifact belongs to a different workspace or build.".into());
        }
        self.recheck_lookup(&ticket, &review).await?;
        let session = self
            .session
            .upgrade()
            .ok_or("The desktop session is closed.")?;
        let session = session
            .lock()
            .map_err(|_| "The desktop session is unavailable.")?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| "The Foundry controller is unavailable.")?;
        reconcile(&session, &mut state);
        require_action(&session, &state)?;
        let stored = current_report(&session, &state, &ticket)?;
        if !stored.report.exact_target_eligible()
            || stored.report.preflight_digest != provenance.digest
        {
            return Err("The captured build is no longer eligible for exact artifact use.".into());
        }
        Ok(())
    }

    fn prepare_lookup(&self, generation: u64) -> Result<(Ticket, FoundryPreflight), String> {
        let session = self
            .session
            .upgrade()
            .ok_or("The desktop session is closed.")?;
        let session = session
            .lock()
            .map_err(|_| "The desktop session is unavailable.")?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| "The Foundry controller is unavailable.")?;
        reconcile(&session, &mut state);
        require_action(&session, &state)?;
        let workspace = state
            .workspace
            .clone()
            .ok_or("Review the workspace first.")?;
        let ticket = Ticket {
            generation,
            workspace,
        };
        current_report(&session, &state, &ticket)?;
        let review = state
            .review
            .clone()
            .ok_or("The Foundry review is unavailable.")?;
        Ok((ticket, review))
    }

    async fn recheck_lookup(
        &self,
        ticket: &Ticket,
        review: &FoundryPreflight,
    ) -> Result<(), String> {
        let path = ticket.workspace.path.clone();
        let result = tokio::task::spawn_blocking(move || foundry_build::preflight(&path))
            .await
            .unwrap_or_else(|error| Err(format!("Foundry freshness check failed: {error}")));
        let unchanged = result.as_ref().is_ok_and(|current| {
            current.workspace == review.workspace
                && current.config_path == review.config_path
                && current.digest == review.digest
                && current.settings == review.settings
                && current.source_manifest == review.source_manifest
        });
        if !unchanged {
            let detail = result
                .err()
                .map(|error| format!(" {error}"))
                .unwrap_or_default();
            self.invalidate_report(ticket);
            return Err(format!("Workspace changed or could not be reviewed. Review and build again before using this output.{detail}"));
        }
        Ok(())
    }

    fn invalidate_report(&self, ticket: &Ticket) {
        let Some(session) = self.session.upgrade() else {
            return;
        };
        let Ok(session) = session.lock() else { return };
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if current_report(&session, &state, ticket).is_ok() {
            state.report = None;
            state.review = None;
            state.review_generation = None;
            state.trusted_digest = None;
            state.phase = FoundryPhase::Stale;
            state.message =
                "The workspace changed or could not be reviewed. Review and build again.".into();
            drop(state);
            drop(session);
            self.changed();
        }
    }
}

impl Drop for FoundryController {
    fn drop(&mut self) {
        if let Ok(state) = self.state.get_mut() {
            if let Some(cancel) = state.cancel.take() {
                cancel.cancel();
            }
        }
    }
}

fn current_workspace(session: &DesktopState) -> Option<WorkspaceStamp> {
    (session.active
        && session.workspace_ready
        && !session.workspace_busy
        && !session.project_opening)
        .then(|| WorkspaceStamp {
            epoch: session.workspace_epoch,
            path: session.workspace.clone(),
        })
}

fn require_action(session: &DesktopState, state: &State) -> Result<(), String> {
    if state.phase == FoundryPhase::Closed {
        return Err("The Foundry controller is closed.".into());
    }
    if current_workspace(session).is_none() || session.transition_pending() {
        return Err("Open a project and finish the current workspace confirmation first.".into());
    }
    Ok(())
}

fn reconcile(session: &DesktopState, state: &mut State) -> bool {
    if state.phase == FoundryPhase::Closed {
        return false;
    }
    let stale = state
        .workspace
        .as_ref()
        .is_some_and(|workspace| current_workspace(session).as_ref() != Some(workspace));
    if !stale {
        return false;
    }
    if let Some(cancel) = state.cancel.take() {
        cancel.cancel();
    }
    state.workspace = None;
    state.review_generation = None;
    state.review = None;
    state.trusted_digest = None;
    state.report = None;
    state.phase = FoundryPhase::Stale;
    state.message =
        "The workspace changed or closed; the Foundry review and output were discarded.".into();
    true
}

fn ticket_is_current(
    session: &DesktopState,
    state: &State,
    ticket: &Ticket,
    phase: FoundryPhase,
) -> bool {
    state.phase == phase
        && state.generation == ticket.generation
        && state.workspace.as_ref() == Some(&ticket.workspace)
        && current_workspace(session).as_ref() == Some(&ticket.workspace)
}

fn current_report<'a>(
    session: &DesktopState,
    state: &'a State,
    ticket: &Ticket,
) -> Result<&'a StoredReport, String> {
    if !ticket_is_current(session, state, ticket, FoundryPhase::Complete) {
        return Err("The Foundry build or workspace is no longer current.".into());
    }
    state
        .report
        .as_ref()
        .filter(|report| report.generation == ticket.generation)
        .ok_or_else(|| "The Foundry build is no longer available.".into())
}

fn next_generation(state: &mut State) -> Result<u64, String> {
    state.generation = state
        .generation
        .checked_add(1)
        .ok_or("Foundry operation generation is exhausted; reopen the project.")?;
    Ok(state.generation)
}

fn actionable_error(error: String) -> String {
    if error.contains("solc ") && error.contains("is not installed locally") {
        format!("{error} Install the exact compiler separately using your trusted local Foundry setup, then review and retry. Chasm will not download it.")
    } else {
        error
    }
}

#[cfg(test)]
fn checked_artifact<'a>(
    settings_verified: bool,
    sources_verified: bool,
    artifacts: &'a [FoundryArtifactSummary],
    artifact_id: &str,
    purpose: ArtifactPurpose,
) -> Result<&'a FoundryArtifactSummary, String> {
    if !settings_verified || !sources_verified {
        return Err("Build settings or sources did not match the review; exact artifact use is unavailable.".into());
    }
    let mut matches = artifacts
        .iter()
        .filter(|artifact| artifact.id == artifact_id);
    let artifact = matches
        .next()
        .ok_or("The exact artifact ID is not in the current build.")?;
    if matches.next().is_some() {
        return Err("The artifact ID is ambiguous in the current build.".into());
    }
    match purpose {
        ArtifactPurpose::Abi => {}
        ArtifactPurpose::Deploy if !artifact.deployable() => {
            return Err("This artifact is not deployable: creation bytecode is missing or linking is unresolved.".into());
        }
        ArtifactPurpose::Debugger if !debugger_data_available(artifact) => {
            return Err("This artifact lacks the linked bytecode or compiler data required by the debugger adapter.".into());
        }
        _ => {}
    }
    Ok(artifact)
}

fn debugger_data_available(artifact: &FoundryArtifactSummary) -> bool {
    artifact.linked
        && artifact.creation_bytecode.is_some()
        && artifact.runtime_bytecode.is_some()
        && artifact
            .creation_source_map
            .as_ref()
            .is_some_and(|value| !value.is_empty())
        && artifact
            .runtime_source_map
            .as_ref()
            .is_some_and(|value| !value.is_empty())
        && artifact.storage_layout.is_some()
        && artifact.method_identifiers.is_some()
        && artifact.creation_link_references.is_some()
        && artifact.runtime_link_references.is_some()
        && artifact.immutable_references.is_some()
}

fn report_view(stored: &StoredReport) -> FoundryReportView {
    let report = &stored.report;
    let exact = report.exact_target_eligible();
    let artifacts = report
        .artifacts
        .iter()
        .map(|artifact| {
            let deploy_eligible = report.artifact_deployable(&artifact.id);
            let debug = exact && debugger_data_available(artifact);
            let mut ineligibility = Vec::new();
            if !report.settings_verified {
                ineligibility.push("Compiler settings do not match the reviewed profile.".into());
            }
            if !report.sources_verified {
                ineligibility.push("Compiler sources do not match the reviewed manifest.".into());
            }
            if !artifact.linked {
                ineligibility.push("Library linking is unresolved or requires review.".into());
            }
            if artifact.creation_bytecode.is_none() {
                ineligibility.push("Creation bytecode is unavailable.".into());
            }
            if !debugger_data_available(artifact) {
                ineligibility
                    .push("Linked bytecode or requested debugger data is incomplete.".into());
            }
            FoundryArtifactView {
                id: artifact.id.clone(),
                source: artifact.source.clone(),
                contract: artifact.contract.clone(),
                abi_keccak256: artifact.abi_keccak256.clone(),
                creation_keccak256: artifact.creation_keccak256.clone(),
                runtime_keccak256: artifact.runtime_keccak256.clone(),
                linked: artifact.linked,
                abi_eligible: exact,
                deploy_eligible,
                debugger_data_available: debug,
                ineligibility,
            }
        })
        .collect();
    let mut warnings = report.warnings.clone();
    warnings.push("Build verification does not establish a match with any deployed contract; verify the target separately.".into());
    FoundryReportView {
        generation: stored.generation,
        digest: report.preflight_digest.clone(),
        build_info_id: stored.build_info_id.clone(),
        settings_verified: report.settings_verified,
        sources_verified: report.sources_verified,
        exact_build_eligible: exact,
        artifacts,
        warnings,
        stdout: report.stdout.clone(),
        stderr: report.stderr.clone(),
    }
}
