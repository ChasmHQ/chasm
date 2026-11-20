use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct JobId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Operation {
    Compile,
    Deploy,
    Call,
    Storage,
    Explorer,
    Chain,
}

#[derive(Clone, Debug)]
pub struct JobEntry {
    pub id: JobId,
    pub name: String,
    pub status: String,
    pub output: String,
    pub context: String,
    pub transaction_hash: Option<String>,
    /// Contract document the job reports back to, when it has one.
    pub document: Option<String>,
}

#[derive(Clone, Debug)]
pub struct RequestToken {
    pub id: JobId,
    pub operation: Operation,
    pub workspace_epoch: u64,
    pub network_epoch: u64,
    pub rpc: String,
    pub view_revision: Option<u64>,
}

#[derive(Default)]
pub struct JobTracker {
    next: u64,
    latest: HashMap<Operation, JobId>,
}

impl JobTracker {
    pub fn begin(&mut self, operation: Operation) -> JobId {
        self.next += 1;
        let id = JobId(self.next);
        self.latest.insert(operation, id);
        id
    }

    pub fn is_latest(&self, token: &RequestToken) -> bool {
        self.latest.get(&token.operation) == Some(&token.id)
    }
}

pub fn prune(entries: &mut Vec<JobEntry>) {
    while entries.len() > 50 {
        let Some(index) = entries.iter().position(|entry| entry.status != "running") else { break };
        entries.remove(index);
    }
}

pub fn bounded_output(output: &str) -> String {
    if output.len() <= 64 * 1024 { return output.to_string(); }
    let mut end = 64 * 1024;
    while !output.is_char_boundary(end) { end -= 1; }
    format!("{}\n[Output truncated at 64 KiB]", &output[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn older_requests_are_not_latest() {
        let mut tracker = JobTracker::default();
        let first = tracker.begin(Operation::Call);
        let second = tracker.begin(Operation::Call);
        let token = |id| RequestToken { id, operation: Operation::Call, workspace_epoch: 1, network_epoch: 1, rpc: "fixture".into(), view_revision: Some(0) };
        assert_ne!(first, second);
        assert!(!tracker.is_latest(&token(first)));
        assert!(tracker.is_latest(&token(second)));
    }

    #[test]
    fn pruning_keeps_running_jobs_and_utf8_output_is_bounded() {
        let mut entries: Vec<_> = (0..60).map(|id| JobEntry { id: JobId(id), name: "fixture".into(), status: "running".into(), output: String::new(), context: String::new(), transaction_hash: None, document: None }).collect();
        prune(&mut entries);
        assert_eq!(entries.len(), 60);
        entries[30].status = "ok".into();
        prune(&mut entries);
        assert_eq!(entries.len(), 59);
        assert!(!entries.iter().any(|entry| entry.id == JobId(30)));
        let output = bounded_output(&"é".repeat(40_000));
        assert!(output.ends_with("[Output truncated at 64 KiB]"));
    }
}
