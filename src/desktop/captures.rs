//! Offline-engine captures saved in the project: `.chasm/captures/<time>-<label>.json`,
//! each a `chasm-fork-capture` bundle exported by the debugger worker. They
//! reopen through the gateway's resource upload as an Offline capture launch.
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const FORMAT: &str = "chasm-fork-capture";
/// The gateway's resource limit.
const MAX_BYTES: usize = 8 * 1024 * 1024;

pub fn dir(workspace: &Path) -> PathBuf {
    workspace.join(".chasm").join("captures")
}

/// Saves an exported capture. Rejects anything that is not a capture bundle.
pub fn save(workspace: &Path, label: &str, capture: &Value) -> Result<PathBuf, String> {
    if capture["format"] != FORMAT {
        return Err("The debugger did not return a capture bundle.".into());
    }
    let bytes = serde_json::to_vec_pretty(capture).map_err(|error| error.to_string())?;
    if bytes.len() > MAX_BYTES {
        return Err("The capture exceeds the 8 MiB resource limit and was not saved.".into());
    }
    let folder = dir(workspace);
    std::fs::create_dir_all(&folder).map_err(|error| format!("Could not create {}: {error}", folder.display()))?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|time| time.as_secs()).unwrap_or(0);
    let label: String = label.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    let label = label.split('-').filter(|part| !part.is_empty()).collect::<Vec<_>>().join("-");
    let path = folder.join(format!("{stamp}-{}.json", if label.is_empty() { "capture" } else { &label[..label.len().min(40)] }));
    std::fs::write(&path, bytes).map_err(|error| format!("Could not write {}: {error}", path.display()))?;
    Ok(path)
}

/// Saved captures, newest first: file names.
pub fn list(workspace: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir(workspace)) else { return Vec::new() };
    let mut items: Vec<(std::time::SystemTime, String)> = entries
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.file_name().to_string_lossy().to_string())))
        .collect();
    items.sort_by(|a, b| b.0.cmp(&a.0));
    items.into_iter().take(50).map(|(_, name)| name).collect()
}

/// Reads a saved capture by file name; the name must stay inside the folder.
pub fn read(workspace: &Path, name: &str) -> Result<Value, String> {
    if name.is_empty() || name.contains(['/', '\\', ':']) || name.starts_with('.') || !name.ends_with(".json") {
        return Err("Choose a saved capture from the list.".into());
    }
    let path = dir(workspace).join(name);
    let bytes = std::fs::read(&path).map_err(|error| format!("Could not read {}: {error}", path.display()))?;
    if bytes.len() > MAX_BYTES {
        return Err("The capture exceeds the 8 MiB resource limit.".into());
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| format!("{name} is not JSON."))?;
    if value["format"] != FORMAT {
        return Err(format!("{name} is not a Chasm capture."));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn captures_round_trip_and_reject_other_files() {
        let workspace = tempfile::tempdir().unwrap();
        let capture = json!({"format": FORMAT, "version": 1, "plan": {}, "baseline": {}, "witness": {"a": 1}});
        let path = save(workspace.path(), "Replay 0xabc", &capture).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.ends_with("-replay-0xabc.json"), "{name}");
        assert_eq!(list(workspace.path()), vec![name.clone()]);
        assert_eq!(read(workspace.path(), &name).unwrap(), capture);
        assert!(save(workspace.path(), "x", &json!({"format": "other"})).is_err());
        assert!(read(workspace.path(), "../secret.json").is_err());
        std::fs::write(dir(workspace.path()).join("other.json"), "{}").unwrap();
        assert!(read(workspace.path(), "other.json").unwrap_err().contains("not a Chasm capture"));
    }
}
