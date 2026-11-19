//! Where a workspace build takes its remappings and solc settings from.
//!
//! `[build] source` in `.chasm/chasm.toml` picks one of:
//! - `auto` (default): Foundry when the folder has `foundry.toml`, else Chasm;
//! - `foundry`: the project's own Foundry configuration, as `forge config
//!   --json` resolves it (profile, auto-detected remappings, compiler settings);
//! - `chasm`: `remappings` and `[compiler]` from `.chasm/chasm.toml`.
//!
//! The two are never merged: a Chasm-managed library remapping next to a
//! Foundry one can resolve the same import to two different files.
use crate::config::{ChasmConfig, CompilerConfig};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    #[default]
    Auto,
    Foundry,
    Chasm,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Foundry => "foundry",
            Self::Chasm => "chasm",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "auto" | "" => Some(Self::Auto),
            "foundry" => Some(Self::Foundry),
            "chasm" => Some(Self::Chasm),
            _ => None,
        }
    }
}

/// The settings a build uses, and where they came from.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Effective {
    /// What `[build] source` says.
    pub chosen: Source,
    /// Foundry or Chasm, after Auto is resolved.
    pub active: Source,
    pub remappings: Vec<String>,
    pub compiler: CompilerConfig,
    /// For the UI, e.g. "foundry.toml, profile default".
    pub origin: String,
}

/// Reads `[build] source` without failing on an unreadable config.
pub fn chosen(root: &Path) -> Source {
    ChasmConfig::load(root).ok().and_then(|config| config.build.source).unwrap_or_default()
}

/// Resolves the effective build settings. A Foundry project whose
/// configuration cannot be read is an error, not a silent switch to Chasm.
pub fn resolve(root: &Path) -> Result<Effective, String> {
    let chasm = ChasmConfig::load(root).map_err(|error| error.to_string())?;
    let chosen = chasm.build.source.unwrap_or_default();
    let has_foundry = root.join("foundry.toml").is_file();
    let active = match chosen {
        Source::Auto if has_foundry => Source::Foundry,
        Source::Auto => Source::Chasm,
        other => other,
    };
    match active {
        Source::Foundry => {
            if !has_foundry {
                return Err("Build source is Foundry, but this folder has no foundry.toml. Choose Auto or Chasm in Compiler and libraries.".into());
            }
            let (remappings, compiler, profile) = foundry(root)?;
            Ok(Effective { chosen, active, remappings, compiler, origin: format!("foundry.toml, profile {profile}") })
        }
        _ => Ok(Effective { chosen, active: Source::Chasm, remappings: chasm.remappings, compiler: chasm.compiler, origin: ".chasm/chasm.toml".into() }),
    }
}

/// Remappings and compiler settings exactly as Foundry resolves them.
fn foundry(root: &Path) -> Result<(Vec<String>, CompilerConfig, String), String> {
    let mut command = std::process::Command::new("forge");
    command.args(["config", "--json", "--root"]).arg(root).stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    }
    let output = command.output().map_err(|error| format!("Could not run forge to read foundry.toml ({error}). Install Foundry, or choose Chasm as the build source."))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        let line = detail.lines().find(|line| !line.trim().is_empty()).unwrap_or("forge config failed").trim();
        return Err(format!("foundry.toml could not be read: {line}"));
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).map_err(|_| "forge config returned invalid JSON.".to_string())?;
    let profile = std::env::var("FOUNDRY_PROFILE").ok().filter(|name| !name.is_empty()).unwrap_or_else(|| "default".into());
    Ok((remappings_from(&value, root), compiler_from(&value), profile))
}

/// Foundry remappings, made absolute so they do not depend on where solc runs.
fn remappings_from(config: &serde_json::Value, root: &Path) -> Vec<String> {
    let base = root.to_string_lossy().trim_start_matches(r"\\?\").replace('\\', "/");
    config["remappings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item.as_str())
        .filter_map(|item| {
            // Context remappings (`ctx:prefix=path`) are kept as they are.
            let (name, path) = item.split_once('=')?;
            let path = path.replace('\\', "/");
            let absolute = path.starts_with('/') || path.get(1..3) == Some(":/");
            Some(if absolute { format!("{name}={path}") } else { format!("{name}={base}/{}", path.trim_start_matches("./")) })
        })
        .collect()
}

fn compiler_from(config: &serde_json::Value) -> CompilerConfig {
    let text = |key: &str| config[key].as_str().unwrap_or_default().to_string();
    CompilerConfig {
        // `solc` may be a version or a path; only a version is passed through.
        solc: config["solc"].as_str().or(config["solc_version"].as_str()).filter(|value| semver::Version::parse(value).is_ok()).unwrap_or_default().to_string(),
        optimizer: config["optimizer"].as_bool(),
        optimizer_runs: config["optimizer_runs"].as_u64().map(|runs| runs as usize),
        via_ir: config["via_ir"].as_bool(),
        evm_version: text("evm_version"),
        bytecode_hash: text("bytecode_hash"),
    }
}

/// Saves `[build] source`.
pub fn choose(root: &Path, source: Source) -> Result<(), String> {
    ChasmConfig::update(root, |config| config.build.source = (source != Source::Auto).then_some(source)).map_err(|error| error.to_string())
}

/// The compile error for two copies of one library: the files and the
/// remappings that point at each. Solidity reports it as "Identifier already
/// declared" with two paths for the same contract.
pub fn duplicate_library_hint(error: &str, remappings: &[String]) -> Option<String> {
    if !error.contains("Identifier already declared") && !error.contains("defined twice") {
        return None;
    }
    let mut paths: Vec<String> = Vec::new();
    for line in error.lines() {
        if let Some(at) = line.find("--> ") {
            let path = line[at + 4..].split(".sol:").next().unwrap_or_default().to_string() + ".sol";
            let path = path.replace('\\', "/");
            if !path.starts_with("src/") && !paths.contains(&path) {
                paths.push(path);
            }
        }
    }
    let file = |path: &str| path.rsplit('/').next().unwrap_or(path).to_string();
    let (first, second) = paths.iter().enumerate().find_map(|(index, a)| paths[index + 1..].iter().find(|b| file(b) == file(a)).map(|b| (a.clone(), b.clone())))?;
    let source = |path: &str| {
        remappings
            .iter()
            .filter(|item| item.split_once('=').is_some_and(|(_, target)| path.starts_with(target.trim_end_matches('/'))))
            .max_by_key(|item| item.len())
            .cloned()
            .unwrap_or_else(|| "a relative import".into())
    };
    Some(format!(
        "Two copies of {} are compiled together:\n- {first} (from {})\n- {second} (from {})\nMap both import prefixes to one copy, or use the project's own foundry.toml (Compiler and libraries > Build source).",
        file(&first),
        source(&first),
        source(&second)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn foundry_config_becomes_absolute_remappings_and_settings() {
        let config = json!({
            "remappings": ["@openzeppelin/=lib/openzeppelin/", "forge-std/=C:/x/forge-std/src/", "ctx:a/=./b/"],
            "optimizer": true, "optimizer_runs": 200, "via_ir": true, "evm_version": "cancun", "bytecode_hash": "none", "solc": "0.8.30",
        });
        let remappings = remappings_from(&config, Path::new(r"C:\work\poc"));
        assert_eq!(remappings, vec!["@openzeppelin/=C:/work/poc/lib/openzeppelin/", "forge-std/=C:/x/forge-std/src/", "ctx:a/=C:/work/poc/b/"]);
        let compiler = compiler_from(&config);
        assert_eq!(compiler.describe(), "optimizer 200 runs, via-IR, cancun, no metadata hash");
        assert_eq!(compiler.solc, "0.8.30");
        assert_eq!(compiler_from(&json!({"solc": "/usr/bin/solc"})).solc, "");
    }

    #[test]
    fn auto_follows_foundry_toml_and_chasm_never_merges() {
        let root = tempfile::tempdir().unwrap();
        ChasmConfig::update(root.path(), |config| config.remappings.push("x/=y/".into())).unwrap();
        let chasm = resolve(root.path()).unwrap();
        assert_eq!((chasm.active, chasm.remappings.as_slice()), (Source::Chasm, &["x/=y/".to_string()][..]));
        choose(root.path(), Source::Foundry).unwrap();
        assert!(resolve(root.path()).unwrap_err().contains("no foundry.toml"));
        choose(root.path(), Source::Auto).unwrap();
        assert_eq!(chosen(root.path()), Source::Auto);
        assert!(!std::fs::read_to_string(root.path().join(".chasm/chasm.toml")).unwrap().contains("[build]"));
    }

    #[test]
    fn duplicate_library_errors_name_both_copies() {
        let error = "Error (9097): Identifier already declared.\n  --> C:/Users/u/.chasm/lib/oz@v5/contracts/proxy/utils/Initializable.sol:65:5:\nNote: The previous declaration is here:\n  --> C:/Users/u/.chasm/lib/ozu@v5/contracts/proxy/utils/Initializable.sol:65:5:";
        let remappings = vec![
            "@openzeppelin/contracts/=C:/Users/u/.chasm/lib/oz@v5/contracts/".to_string(),
            "@openzeppelin/contracts-upgradeable/=C:/Users/u/.chasm/lib/ozu@v5/contracts/".to_string(),
        ];
        let hint = duplicate_library_hint(error, &remappings).unwrap();
        assert!(hint.starts_with("Two copies of Initializable.sol"), "{hint}");
        assert!(hint.contains("(from @openzeppelin/contracts/=") && hint.contains("(from @openzeppelin/contracts-upgradeable/="), "{hint}");
        assert!(duplicate_library_hint("ParserError: Source not found", &remappings).is_none());
    }
}

#[cfg(test)]
mod workspace_probe {
    /// Compiles `CHASM_PROBE_WORKSPACE` with the build source Chasm would use.
    #[test]
    #[ignore = "needs CHASM_PROBE_WORKSPACE, a copy of a real project"]
    fn probe_workspace_compiles() {
        let root = std::path::PathBuf::from(std::env::var("CHASM_PROBE_WORKSPACE").unwrap());
        let effective = super::resolve(&root).unwrap();
        println!("active {:?} origin {} settings {}", effective.active, effective.origin, effective.compiler.describe());
        let report = crate::desktop::compile_workspace_probe(&root).unwrap();
        println!("compiled {report}");
    }
}
