use serde_json::Value;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};

pub(crate) const WORKER_PYTHON: &str = if cfg!(windows) {
    ".venv/Scripts/python.exe"
} else {
    ".venv/bin/python"
};

// Cargo builds use the worker in this checkout. An installed executable uses
// only its own payload, never a sibling repository or the opened workspace.
fn service_dir(exe: &Path, checkout: &Path, cargo_profile: &Path) -> PathBuf {
    let bin = exe.parent().unwrap_or(exe);
    let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_owned());
    if canonical(bin).starts_with(canonical(cargo_profile)) {
        return checkout.join("debugger");
    }
    if cfg!(windows) {
        bin.join("debugger")
    } else {
        bin.parent().unwrap_or(bin).join("share/chasm/debugger")
    }
}

pub const PROTOCOL: &str = "chasm-web-debugger/1";
pub const MAX_BODY: usize = 8 * 1024 * 1024;
pub const MAX_REQUEST: usize = 256 * 1024;
pub const MAX_RESPONSE: usize = 16 * 1024 * 1024;

pub type BuildInfo = std::sync::Arc<dyn Fn() -> anyhow::Result<Value> + Send + Sync>;

#[derive(Clone)]
pub struct Config {
    pub workspace: PathBuf,
    pub python: PathBuf,
    pub service_dir: PathBuf,
    pub example: PathBuf,
    pub port: u16,
    pub dev_origin: Option<String>,
    pub providers: BTreeMap<String, String>,
    pub timeout: Duration,
    pub build_info: Option<BuildInfo>,
    pub(crate) reason: Option<String>,
    #[cfg(test)]
    pub(crate) worker_script: Option<PathBuf>,
}

impl Config {
    pub fn new(
        workspace: PathBuf,
        python: Option<PathBuf>,
        dev_origin: Option<String>,
        example: Option<PathBuf>,
    ) -> Self {
        let checkout = Path::new(env!("CARGO_MANIFEST_DIR"));
        // Cargo's OUT_DIR is <profile>/build/<package>/out, including for a custom target dir.
        let profile = Path::new(env!("OUT_DIR")).ancestors().nth(3).unwrap();
        let service_dir = std::env::current_exe()
            .map(|exe| service_dir(&exe, checkout, profile))
            .unwrap_or_else(|_| checkout.join("debugger"));
        let default_python = service_dir.join(WORKER_PYTHON);
        let python = python
            .or_else(|| std::env::var_os("CHASM_DEBUGGER_PYTHON").map(PathBuf::from))
            .unwrap_or(default_python);
        let mut reason = None;
        let providers: BTreeMap<String, String> = match std::env::var("CHASM_DEBUGGER_PROVIDERS") {
            Ok(raw) => match serde_json::from_str::<BTreeMap<String, String>>(&raw) {
                Ok(map)
                    if map.len() <= 32
                        && map.iter().all(|(id, name)| {
                            valid_identifier(id)
                                && valid_identifier(name)
                                && ![
                                    "PATH",
                                    "PYTHONPATH",
                                    "PYTHONHOME",
                                    "SYSTEMROOT",
                                    "COMSPEC",
                                    "HOME",
                                    "TEMP",
                                    "TMP",
                                    "LD_PRELOAD",
                                ]
                                .contains(&name.to_uppercase().as_str())
                        }) =>
                {
                    map
                }
                _ => {
                    reason = Some("CHASM_DEBUGGER_PROVIDERS must map provider aliases to environment variable names.".into());
                    BTreeMap::new()
                }
            },
            Err(_) => BTreeMap::new(),
        };
        let dev_origin = dev_origin.and_then(|origin| {
            // Development access is an explicit CLI opt-in, never a wildcard.
            let valid = ["http://localhost:", "http://127.0.0.1:"]
                .iter()
                .any(|prefix| {
                    origin
                        .strip_prefix(prefix)
                        .and_then(|port| port.parse::<u16>().ok())
                        .is_some_and(|p| p != 0)
                });
            if !valid {
                reason = Some(
                    "--debugger-dev-origin must be an exact loopback HTTP origin with a port."
                        .into(),
                );
                None
            } else {
                Some(origin)
            }
        });
        Self {
            workspace,
            python,
            example: example
                .unwrap_or_else(|| service_dir.join("examples/mainnet/synthetic-replay.json")),
            service_dir,
            port: 3000,
            dev_origin,
            providers,
            timeout: Duration::from_secs(60),
            build_info: None,
            reason,
            #[cfg(test)]
            worker_script: None,
        }
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    #[test]
    fn cargo_and_installed_binaries_use_their_own_worker() {
        let tmp = tempfile::tempdir().unwrap();
        let checkout = tmp.path().join("checkout");
        let profile = tmp.path().join("custom-target/debug");
        for file in ["chasm", "deps/chasm-test"] {
            assert_eq!(service_dir(&profile.join(file), &checkout, &profile), checkout.join("debugger"));
        }
        let bin = tmp.path().join("install/bin");
        let expected = if cfg!(windows) { bin.join("debugger") } else { tmp.path().join("install/share/chasm/debugger") };
        assert_eq!(service_dir(&bin.join("chasm"), &checkout, &profile), expected);
    }
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
