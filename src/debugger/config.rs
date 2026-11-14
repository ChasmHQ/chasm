use serde_json::Value;
use std::{collections::BTreeMap, path::PathBuf, time::Duration};

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
        let service_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("debugger");
        let default_python = service_dir.join(if cfg!(windows) {
            ".venv/Scripts/python.exe"
        } else {
            ".venv/bin/python"
        });
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

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
