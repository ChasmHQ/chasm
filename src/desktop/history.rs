//! Per-project record of contract calls, saved as JSON lines in
//! `.chasm/history.jsonl`. Each entry keeps what is needed to repeat the
//! call (contract, address, function, arguments, value) and how it ended.
//! Signers are recorded by public address only.
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Entries kept on disk; older lines are dropped when the file is rewritten.
pub const MAX_ENTRIES: usize = 500;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// Unix seconds.
    pub time: u64,
    /// "send" or "simulate".
    pub kind: String,
    pub contract: String,
    pub address: String,
    /// `name(types)`.
    pub function: String,
    /// Positional JSON arguments, as the request editor stores them.
    pub arguments: String,
    /// Wei.
    pub value: String,
    pub from: String,
    /// "ok", "reverted", "failed" or "unknown".
    pub status: String,
    /// One line: return value, revert reason or error.
    pub summary: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tx: String,
    /// 0x calldata, for macros.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub data: String,
    /// The network the call ran on (profile:chain:block). Entries without
    /// one predate network profiles; they are shown as unknown network.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
}

pub fn path(workspace: &Path) -> PathBuf {
    workspace.join(".chasm").join("history.jsonl")
}

/// Newest first. Lines that do not parse are skipped.
pub fn load(workspace: &Path) -> Vec<Entry> {
    let Ok(text) = std::fs::read_to_string(path(workspace)) else { return Vec::new() };
    let mut entries: Vec<Entry> = text.lines().filter_map(|line| serde_json::from_str(line).ok()).collect();
    entries.reverse();
    entries.truncate(MAX_ENTRIES);
    entries
}

pub fn append(workspace: &Path, entry: &Entry) -> Result<(), String> {
    let file = path(workspace);
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    }
    let line = serde_json::to_string(entry).map_err(|error| error.to_string())?;
    let mut handle = std::fs::OpenOptions::new().create(true).append(true).open(&file).map_err(|error| error.to_string())?;
    writeln!(handle, "{line}").map_err(|error| error.to_string())?;
    drop(handle);
    // Rewrite only once the file is well past the limit, so appends stay cheap.
    let size = std::fs::metadata(&file).map(|meta| meta.len()).unwrap_or(0);
    if size > 2 * 1024 * 1024 {
        let mut kept = load(workspace);
        kept.truncate(MAX_ENTRIES);
        kept.reverse();
        let text: String = kept.iter().filter_map(|entry| serde_json::to_string(entry).ok()).map(|line| line + "\n").collect();
        std::fs::write(&file, text).map_err(|error| error.to_string())?;
    }
    Ok(())
}

pub fn clear(workspace: &Path) -> Result<(), String> {
    match std::fs::remove_file(path(workspace)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

/// First line of a result or error, bounded for the list.
pub fn summarize(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("");
    let mut out: String = line.chars().take(160).collect();
    if line.chars().count() > 160 {
        out.push('…');
    }
    out
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|time| time.as_secs()).unwrap_or(0)
}

/// "HH:MM" today, or "N days ago", in UTC (no time-zone database is bundled).
pub fn when(time: u64) -> String {
    let age = now().saturating_sub(time);
    if age < 86_400 {
        let seconds = time % 86_400;
        format!("{:02}:{:02} UTC", seconds / 3600, seconds % 3600 / 60)
    } else {
        format!("{} days ago", age / 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_append_load_newest_first_and_clear() {
        let workspace = tempfile::tempdir().unwrap();
        assert!(load(workspace.path()).is_empty());
        for (index, status) in ["ok", "reverted"].iter().enumerate() {
            let entry = Entry { time: index as u64, kind: "send".into(), function: format!("f{index}()"), status: (*status).into(), ..Default::default() };
            append(workspace.path(), &entry).unwrap();
        }
        std::fs::OpenOptions::new().append(true).open(path(workspace.path())).unwrap().write_all(b"not json\n").unwrap();
        let entries = load(workspace.path());
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].function, "f1()");
        assert_eq!(entries[0].status, "reverted");
        clear(workspace.path()).unwrap();
        assert!(load(workspace.path()).is_empty());
        clear(workspace.path()).unwrap();
    }

    #[test]
    fn summaries_keep_the_first_line_only() {
        assert_eq!(summarize("\n  Return 1: 5\nmore"), "Return 1: 5");
        assert_eq!(summarize(&"x".repeat(200)).chars().count(), 161);
    }

    #[test]
    fn entries_carry_their_network_and_old_lines_still_load() {
        let workspace = tempfile::tempdir().unwrap();
        let tagged = Entry { time: 1, kind: "send".into(), function: "f()".into(), status: "ok".into(), network: Some("mainnet:1:21000000".into()), ..Default::default() };
        append(workspace.path(), &tagged).unwrap();
        // A line saved before network profiles: no network field at all.
        std::fs::OpenOptions::new().append(true).open(path(workspace.path())).unwrap()
            .write_all(b"{\"time\":2,\"kind\":\"simulate\",\"contract\":\"\",\"address\":\"\",\"function\":\"g()\",\"arguments\":\"\",\"value\":\"0\",\"from\":\"\",\"status\":\"ok\",\"summary\":\"\"}\n").unwrap();
        let entries = load(workspace.path());
        assert_eq!(entries.len(), 2);
        // Newest first: the old line has no network field at all.
        assert_eq!(entries[0].network, None, "an old line parses with an unknown network");
        assert_eq!(entries[1].network.as_deref(), Some("mainnet:1:21000000"));
        // The network round trips through the file.
        let back = &entries[1];
        assert!(serde_json::to_string(back).unwrap().contains("\"network\":\"mainnet:1:21000000\""));
    }
}
