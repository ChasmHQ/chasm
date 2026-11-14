use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Idle,
    Preparing,
    Running,
    Paused,
    Finished,
    Failed,
    Cancelling,
    Cancelled,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Preparing => "preparing",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Finished => "finished",
            Self::Failed => "failed",
            Self::Cancelling => "cancelling",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Capabilities {
    #[serde(default)]
    pub source: bool,
    #[serde(default)]
    pub solidity: bool,
    #[serde(default)]
    pub locals: bool,
    #[serde(default)]
    pub restart: bool,
    #[serde(default)]
    pub reason: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Panes {
    #[serde(default)]
    pub source: Value,
    #[serde(default)]
    pub disassembly: Value,
    #[serde(default)]
    pub frames: Value,
    #[serde(default)]
    pub locals: Value,
    #[serde(default)]
    pub arguments: Value,
    #[serde(default)]
    pub stack: Value,
    #[serde(default)]
    pub memory: Value,
    #[serde(default)]
    pub storage: Value,
    #[serde(default)]
    pub calldata: Value,
    #[serde(default)]
    pub logs: Value,
    #[serde(default)]
    pub gas: Value,
    #[serde(default)]
    pub diff: Value,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Snapshot {
    pub status: Value,
    pub capabilities: Capabilities,
    pub panes: Panes,
    #[serde(default)]
    pub fork: Value,
    #[serde(default)]
    pub breakpoints: Value,
}

#[derive(Clone, Debug, Deserialize)]
pub struct GatewayError {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Envelope {
    pub id: String,
    pub generation: u64,
    pub revision: u64,
    pub sequence: u64,
    pub state: Phase,
    pub busy: bool,
    pub snapshot: Option<Snapshot>,
    pub error: Option<GatewayError>,
    pub result: Option<Value>,
}

#[derive(Clone, Debug)]
pub struct Ticket {
    epoch: u64,
    operation: u64,
    id: Option<String>,
}
impl Ticket {
    pub fn is_launch(&self) -> bool {
        self.id.is_none()
    }
}

#[derive(Default)]
pub struct DebugSession {
    pub phase: Phase,
    pub envelope: Option<Envelope>,
    pub snapshot: Option<Snapshot>,
    pub result: Option<Value>,
    pub notice: String,
    pub recovery_request: Option<String>,
    pub orphan_session: Option<String>,
    current_snapshot: bool,
    pub code_mode: i32,
    pub inspector_tab: i32,
    pub inspect_tab: i32,
    epoch: u64,
    operation: u64,
    pending: bool,
    disposed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Acceptance {
    Accepted,
    Ignored,
    Cancel(String),
}

impl DebugSession {
    pub fn can_start(&self) -> bool {
        !self.pending
            && matches!(self.phase, Phase::Idle | Phase::Cancelled)
            && (self.envelope.is_none() || self.disposed)
    }
    pub fn can_cancel(&self) -> bool {
        (!self.disposed
            && (self.pending || self.envelope.is_some())
            && self.phase != Phase::Cancelling)
            || (self.phase == Phase::Cancelling
                && !self.pending
                && (self.envelope.is_some()
                    || self.recovery_request.is_some()
                    || self.orphan_session.is_some()))
    }
    pub fn can_command(&self) -> bool {
        !self.pending
            && !self.disposed
            && self.phase == Phase::Paused
            && self.current_snapshot
            && self.snapshot.is_some()
            && self.envelope.as_ref().is_some_and(|value| !value.busy)
    }
    pub fn can_restart(&self) -> bool {
        !self.pending
            && !self.disposed
            && self.current_snapshot
            && matches!(self.phase, Phase::Paused | Phase::Finished)
            && self
                .snapshot
                .as_ref()
                .is_some_and(|value| value.capabilities.restart)
    }
    pub fn can_refresh(&self) -> bool {
        !self.pending
            && !self.disposed
            && self.envelope.is_some()
            && !matches!(
                self.phase,
                Phase::Cancelled | Phase::Cancelling | Phase::Idle
            )
    }
    pub fn begin_start(&mut self) -> Option<Ticket> {
        if !self.can_start() {
            return None;
        }
        self.epoch = self.epoch.wrapping_add(1);
        self.operation = self.operation.wrapping_add(1);
        self.pending = true;
        self.disposed = false;
        self.envelope = None;
        self.snapshot = None;
        self.current_snapshot = false;
        self.recovery_request = None;
        self.orphan_session = None;
        self.result = None;
        self.notice.clear();
        self.phase = Phase::Preparing;
        Some(Ticket {
            epoch: self.epoch,
            operation: self.operation,
            id: None,
        })
    }
    pub fn begin_command(&mut self, method: &str, action: Option<&str>) -> Option<Ticket> {
        let allowed = match method {
            "navigate" if action == Some("restart") => self.can_restart(),
            "navigate" | "evaluate" | "breakpoint" => self.can_command(),
            "snapshot" | "refresh" => self.can_refresh(),
            _ => false,
        };
        if !allowed {
            return None;
        }
        self.pending = true;
        self.operation = self.operation.wrapping_add(1);
        self.notice.clear();
        Some(self.ticket())
    }
    pub fn ticket(&self) -> Ticket {
        Ticket {
            epoch: self.epoch,
            operation: self.operation,
            id: self.envelope.as_ref().map(|value| value.id.clone()),
        }
    }
    pub fn accepts(&self, ticket: &Ticket) -> bool {
        ticket.epoch == self.epoch
            && ticket.operation == self.operation
            && !self.disposed
            && ticket
                .id
                .as_ref()
                .is_none_or(|id| self.envelope.as_ref().is_some_and(|value| &value.id == id))
    }
    pub fn accept(&mut self, ticket: &Ticket, value: &Value) -> Result<Acceptance, String> {
        let incoming: Envelope = serde_json::from_value(value.clone())
            .map_err(|_| "The debugger returned a malformed session envelope.".to_string())?;
        if incoming.id.is_empty()
            || incoming.id.len() > 128
            || incoming.generation == 0
            || incoming.state == Phase::Idle
        {
            return Err("The debugger returned an invalid session identity or phase.".into());
        }
        if !self.accepts(ticket) {
            return Ok(
                if ticket.id.is_none()
                    && self
                        .envelope
                        .as_ref()
                        .is_none_or(|current| current.id != incoming.id)
                {
                    Acceptance::Cancel(incoming.id)
                } else {
                    Acceptance::Ignored
                },
            );
        }
        if matches!(
            incoming.state,
            Phase::Preparing | Phase::Running | Phase::Cancelling
        ) && !incoming.busy
            || matches!(
                incoming.state,
                Phase::Paused | Phase::Finished | Phase::Failed | Phase::Cancelled
            ) && incoming.busy
        {
            return Err("Debugger phase and busy state disagree.".into());
        }
        if let Some(snapshot) = &incoming.snapshot {
            validate_snapshot(snapshot)?;
        }
        if incoming.state == Phase::Paused && incoming.snapshot.is_none() {
            return Err("Paused debugger envelope has no current snapshot.".into());
        }
        if let Some(current) = &self.envelope {
            if incoming.id != current.id
                || incoming.generation < current.generation
                || (incoming.generation == current.generation
                    && (incoming.sequence < current.sequence
                        || incoming.revision < current.revision))
            {
                return Ok(Acceptance::Ignored);
            }
        } else if ticket.id.is_some() {
            return Ok(Acceptance::Ignored);
        }
        if let Some(snapshot) = &incoming.snapshot {
            self.snapshot = Some(snapshot.clone());
        }
        if let Some(result) = &incoming.result {
            self.result = Some(result.clone());
        }
        if let Some(error) = &incoming.error {
            self.notice = format!("{}: {}", error.code, error.message);
        }
        self.current_snapshot = incoming.snapshot.as_ref().is_some_and(|snapshot| {
            incoming.state != Phase::Paused
                || snapshot.status.get("stopped") == Some(&Value::Bool(true))
        }) && !incoming.busy;
        self.phase = incoming.state;
        self.pending = incoming.busy;
        self.disposed = incoming.state == Phase::Cancelled;
        self.envelope = Some(incoming);
        Ok(Acceptance::Accepted)
    }
    pub fn failure(&mut self, ticket: &Ticket, message: String) {
        if !self.accepts(ticket) {
            return;
        }
        self.pending = false;
        self.current_snapshot = false;
        self.notice = message;
        if self.envelope.is_none() {
            self.phase = Phase::Failed;
        }
    }
    pub fn orphan(&mut self, id: String) {
        self.orphan_session = Some(id);
        self.phase = Phase::Cancelling;
        self.disposed = true;
        self.pending = true;
        self.notice = "Malformed launch acknowledgement; disposing its allocated worker.".into();
    }
    pub fn launch_failed(&mut self, request: String, message: String, ambiguous: bool) {
        self.pending = false;
        self.notice = message;
        if ambiguous {
            self.recovery_request = Some(request);
            self.disposed = true;
            self.phase = Phase::Cancelling;
            self.notice.push_str(" Cancel replays the exact request ID to locate and dispose the worker; if the original never arrived, this can create the offline fixture briefly before disposal.");
        } else {
            self.recovery_request = None;
            self.disposed = true;
            self.phase = Phase::Cancelled;
            self.notice
                .push_str(" No worker was accepted. You may retry Start.");
        }
    }
    pub fn cancel(&mut self) -> (Option<String>, u64) {
        self.epoch = self.epoch.wrapping_add(1);
        self.pending = true;
        self.disposed = true;
        self.phase = Phase::Cancelling;
        self.notice = "Cancelling the isolated worker; this is not pause.".into();
        (
            self.envelope
                .as_ref()
                .map(|value| value.id.clone())
                .or_else(|| self.orphan_session.clone()),
            self.epoch,
        )
    }
    pub fn track_cancelled_launch(&mut self, value: &Value) -> bool {
        if self.phase != Phase::Cancelling || self.envelope.is_some() {
            return false;
        }
        let Ok(envelope) = serde_json::from_value::<Envelope>(value.clone()) else {
            return false;
        };
        if envelope.id.is_empty() {
            return false;
        }
        self.envelope = Some(envelope);
        true
    }
    pub fn cancelled(&mut self, epoch: u64, result: Result<(), String>) {
        if epoch != self.epoch {
            return;
        }
        self.pending = false;
        match result {
            Ok(()) => {
                self.recovery_request = None;
                self.phase = Phase::Cancelled;
                self.notice = "Worker cancelled. Last snapshot retained for inspection.".into();
            }
            Err(error) => {
                self.phase = Phase::Cancelling;
                self.notice=format!("Cancellation not confirmed: {error}. Retry Cancel; do not start another session.");
                self.disposed = false;
            }
        }
    }
    pub fn pending(&self) -> bool {
        self.pending
    }
    pub fn cancel_epoch(&self) -> u64 {
        self.epoch
    }
    pub fn select_code(&mut self, mode: i32) {
        self.code_mode = mode.clamp(0, 1);
    }
    pub fn select_inspector(&mut self, tab: i32) {
        self.inspector_tab = tab.clamp(0, 2);
    }
    pub fn select_inspection(&mut self, tab: i32) {
        self.inspect_tab = tab.clamp(0, 6);
    }
}

fn validate_snapshot(snapshot: &Snapshot) -> Result<(), String> {
    if !snapshot.status.is_object() {
        return Err("Debugger snapshot status is not an object.".into());
    }
    if snapshot.status.get("stopped") == Some(&Value::Bool(true))
        && snapshot.status.get("pc").and_then(Value::as_u64).is_none()
    {
        return Err("Paused snapshot has no valid PC.".into());
    }
    for value in [
        &snapshot.panes.source,
        &snapshot.panes.disassembly,
        &snapshot.panes.frames,
        &snapshot.panes.locals,
        &snapshot.panes.arguments,
        &snapshot.panes.stack,
        &snapshot.panes.memory,
        &snapshot.panes.storage,
        &snapshot.panes.calldata,
        &snapshot.panes.logs,
        &snapshot.panes.gas,
        &snapshot.panes.diff,
    ] {
        if !value.is_null() && !value.is_object() && !value.is_array() {
            return Err("Debugger inspection pane is malformed.".into());
        }
    }
    for (pane, key) in [
        (&snapshot.panes.source, "items"),
        (&snapshot.panes.disassembly, "rows"),
    ] {
        if let Some(rows) = pane.get(key) {
            let Some(rows) = rows.as_array() else {
                return Err("Debugger code window is malformed.".into());
            };
            if rows.len() > 1024
                || rows.iter().any(|row| {
                    !row.is_object() || row.get("current").is_some_and(|value| !value.is_boolean())
                })
            {
                return Err("Debugger code rows are malformed or oversized.".into());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn envelope(id: &str, sequence: u64, state: &str) -> Value {
        json!({"id":id,"generation":1,"revision":sequence,"sequence":sequence,"state":state,"busy":state=="preparing"||state=="running","snapshot":{"status":{"pc":42,"stopped":true},"capabilities":{"source":true,"locals":true,"solidity":true,"restart":true},"panes":{}},"error":null,"result":null})
    }
    #[test]
    fn commands_serialize_and_finished_is_not_running() {
        let mut session = DebugSession::default();
        let start = session.begin_start().unwrap();
        assert!(!session.can_command());
        assert!(session.can_cancel());
        assert!(session.begin_start().is_none());
        session.accept(&start, &envelope("a", 1, "paused")).unwrap();
        let command = session.begin_command("navigate", Some("continue")).unwrap();
        assert!(session
            .begin_command("navigate", Some("step_line"))
            .is_none());
        assert!(session.can_cancel());
        session
            .accept(&command, &envelope("a", 2, "finished"))
            .unwrap();
        assert!(!session.can_command());
        assert!(session.can_restart());
        assert!(!session.can_start());
    }
    #[test]
    fn late_ack_after_cancel_is_disposed_instead_of_reviving() {
        let mut session = DebugSession::default();
        let start = session.begin_start().unwrap();
        let (_, cancel) = session.cancel();
        assert!(!session.can_start());
        let late = envelope("old", 1, "preparing");
        assert_eq!(
            session.accept(&start, &late).unwrap(),
            Acceptance::Cancel("old".into())
        );
        assert!(session.track_cancelled_launch(&late));
        session.cancelled(cancel, Ok(()));
        assert_eq!(session.phase, Phase::Cancelled);
        assert!(session.snapshot.is_none());
        assert!(session.can_start());
    }
    #[test]
    fn cancelled_snapshot_survives_late_command_and_new_session() {
        let mut session = DebugSession::default();
        let start = session.begin_start().unwrap();
        session.accept(&start, &envelope("a", 1, "paused")).unwrap();
        session.select_inspection(3);
        session.select_inspector(2);
        let command = session
            .begin_command("navigate", Some("step_opcode"))
            .unwrap();
        let (_, cancel) = session.cancel();
        session.cancelled(cancel, Ok(()));
        assert_eq!(
            session
                .accept(&command, &envelope("a", 8, "paused"))
                .unwrap(),
            Acceptance::Ignored
        );
        assert_eq!(session.snapshot.as_ref().unwrap().status["pc"], 42);
        assert_eq!(session.inspect_tab, 3);
        assert_eq!(session.inspector_tab, 2);
        let next = session.begin_start().unwrap();
        session.accept(&next, &envelope("b", 1, "paused")).unwrap();
        assert_eq!(
            session
                .accept(&command, &envelope("a", 9, "paused"))
                .unwrap(),
            Acceptance::Ignored
        );
        assert_eq!(session.envelope.as_ref().unwrap().id, "b");
    }
    #[test]
    fn rejected_and_uncertain_launches_have_distinct_recovery_paths() {
        let mut session = DebugSession::default();
        session.begin_start().unwrap();
        session.launch_failed("rejected".into(), "rejected".into(), false);
        assert!(session.can_start());
        session.begin_start().unwrap();
        session.cancel();
        session.launch_failed("lost-ack".into(), "timeout".into(), true);
        assert!(!session.can_start());
        assert!(session.can_cancel());
        assert_eq!(session.recovery_request.as_deref(), Some("lost-ack"));
        let (_, epoch) = session.cancel();
        session.cancelled(epoch, Err("delete failed".into()));
        assert!(session.can_cancel());
        assert!(!session.can_start());
        let (_, epoch) = session.cancel();
        session.cancelled(epoch, Ok(()));
        assert!(session.can_start());
    }
    #[test]
    fn malformed_and_snapshotless_settled_envelopes_cannot_authorize_commands() {
        let mut session = DebugSession::default();
        let start = session.begin_start().unwrap();
        session.accept(&start, &envelope("a", 1, "paused")).unwrap();
        let ticket = session
            .begin_command("navigate", Some("step_opcode"))
            .unwrap();
        let mut missing = envelope("a", 2, "paused");
        missing["snapshot"] = Value::Null;
        assert!(session.accept(&ticket, &missing).is_err());
        session.failure(&ticket, "malformed response".into());
        assert!(!session.can_command());
        assert!(!session.can_restart());
        assert!(session.snapshot.is_some());
        assert!(session.can_refresh());
        let mut bad = envelope("a", 3, "paused");
        bad["snapshot"]["status"] = Value::Null;
        assert!(session.accept(&session.ticket(), &bad).is_err());
        bad = envelope("a", 3, "paused");
        bad["busy"] = Value::Bool(true);
        assert!(session.accept(&session.ticket(), &bad).is_err());
        bad = envelope("a", 3, "paused");
        bad["snapshot"]["panes"]["memory"] = Value::String("oops".into());
        assert!(session.accept(&session.ticket(), &bad).is_err());
    }
    #[test]
    fn out_of_order_unknown_phase_and_bad_capabilities_fail_closed() {
        let mut session = DebugSession::default();
        let start = session.begin_start().unwrap();
        session.accept(&start, &envelope("a", 3, "paused")).unwrap();
        let ticket = session.ticket();
        assert_eq!(
            session
                .accept(&ticket, &envelope("a", 2, "paused"))
                .unwrap(),
            Acceptance::Ignored
        );
        assert_eq!(
            session
                .accept(&ticket, &envelope("other", 5, "paused"))
                .unwrap(),
            Acceptance::Ignored
        );
        assert!(session
            .accept(&ticket, &envelope("a", 4, "unknown"))
            .is_err());
        let mut bad = envelope("a", 4, "paused");
        bad["snapshot"]["capabilities"]["source"] = json!("yes");
        assert!(session.accept(&ticket, &bad).is_err());
        assert_eq!(session.envelope.as_ref().unwrap().sequence, 3);
    }
}
