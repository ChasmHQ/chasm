//! Foundry builds added to the debugger without recompiling. A project's
//! `out/**/*.json` artifacts give each contract's deployed code, source map,
//! ABI, AST and (with `extra_output = ["storageLayout"]`) storage layout; the
//! source text is read from the project. Code built with any settings maps,
//! because the artifacts are the build that was deployed.
use super::trace_debug::Program;
use serde_json::Value;
use std::path::{Path, PathBuf};

const MAX_ARTIFACTS: usize = 5_000;
const MAX_ARTIFACT_BYTES: u64 = 16 * 1024 * 1024;

/// The artifacts folder: `out` in foundry.toml's default profile, else `out`.
fn out_dir(root: &Path) -> Result<PathBuf, String> {
    let config = std::fs::read_to_string(root.join("foundry.toml")).map_err(|_| format!("{} has no foundry.toml.", root.display()))?;
    let table: toml::Table = config.parse().map_err(|error| format!("foundry.toml is not valid TOML: {error}"))?;
    let out = table.get("profile").and_then(|profile| profile.get("default")).and_then(|profile| profile.get("out")).and_then(|out| out.as_str()).unwrap_or("out");
    Ok(root.join(out))
}

/// Checks a folder before it is saved as an import: a Foundry project with
/// built artifacts. Returns how many contracts it adds.
pub fn check(root: &Path) -> Result<usize, String> {
    let count = programs(root)?.len();
    if count == 0 {
        return Err(format!("{} has no built contracts. Run forge build there first.", root.display()));
    }
    Ok(count)
}

/// Every deployable contract in the project's artifacts.
pub fn programs(root: &Path) -> Result<Vec<Program>, String> {
    let out = out_dir(root)?;
    if !out.is_dir() {
        return Err(format!("{} does not exist. Run forge build in the project first.", out.display()));
    }
    let mut programs = Vec::new();
    let mut seen = 0usize;
    for entry in walkdir::WalkDir::new(&out).max_depth(3).into_iter().filter_map(Result::ok) {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") || path.components().any(|part| part.as_os_str() == "build-info") {
            continue;
        }
        seen += 1;
        if seen > MAX_ARTIFACTS {
            break;
        }
        if entry.metadata().map(|meta| meta.len() > MAX_ARTIFACT_BYTES).unwrap_or(true) {
            continue;
        }
        let Ok(bytes) = std::fs::read(path) else { continue };
        let Ok(artifact) = serde_json::from_slice::<Value>(&bytes) else { continue };
        if let Some(program) = program(root, &artifact) {
            programs.push(program);
        }
    }
    Ok(programs)
}

/// One artifact as a debugger program, or None for interfaces, abstract
/// contracts and artifacts without a source map.
pub fn program(root: &Path, artifact: &Value) -> Option<Program> {
    let deployed = &artifact["deployedBytecode"];
    let code = deployed["object"].as_str()?.trim_start_matches("0x");
    if code.is_empty() {
        return None;
    }
    // Unlinked library placeholders (`__$…$__`) decode once zeroed; the
    // link ranges are ignored when matching.
    let runtime = ethers::utils::hex::decode(link_placeholders_zeroed(code)).ok()?;
    let source_map = deployed["sourceMap"].as_str().filter(|map| !map.is_empty())?.to_string();
    let (file, name) = artifact["metadata"]["settings"]["compilationTarget"].as_object()?.iter().next().map(|(file, name)| (file.clone(), name.as_str().unwrap_or_default().to_string()))?;
    let path = root.join(&file);
    // Foundry hands solc LF sources; offsets drift on a CRLF checkout otherwise.
    let source = std::fs::read_to_string(&path).ok()?.replace("\r\n", "\n");
    Some(Program {
        name,
        runtime,
        source_map,
        source_path: path.display().to_string().replace('\\', "/"),
        source,
        source_id: artifact["id"].as_u64().map(|id| id as u32),
        layout: artifact["storageLayout"].as_object().map(|_| artifact["storageLayout"].clone()),
        abi: serde_json::from_value(artifact["abi"].clone()).ok(),
        ast: artifact["ast"].as_object().map(|_| artifact["ast"].clone()),
        variable: super::trace_debug::variable_ranges(deployed),
    })
}

fn link_placeholders_zeroed(code: &str) -> String {
    let mut out = String::with_capacity(code.len());
    let bytes = code.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'_' && at + 40 <= bytes.len() && bytes[at..at + 40].starts_with(b"__$") {
            out.push_str(&"0".repeat(40));
            at += 40;
        } else {
            out.push(bytes[at] as char);
            at += 1;
        }
    }
    out
}

/// Imported projects saved in `.chasm/chasm.toml`.
pub fn saved(workspace: &Path) -> Vec<String> {
    crate::config::ChasmConfig::load(workspace).map(|config| config.build.imports).unwrap_or_default()
}

/// Programs of every saved import, newest artifacts each time; a project
/// that fails to read is skipped and named in the second list.
pub fn all(workspace: &Path) -> (Vec<Program>, Vec<String>) {
    let mut programs = Vec::new();
    let mut failed = Vec::new();
    for root in saved(workspace) {
        match self::programs(Path::new(&root)) {
            Ok(found) => programs.extend(found),
            Err(error) => failed.push(error),
        }
    }
    (programs, failed)
}

/// Adds a project folder; returns how many contracts it brings.
pub fn add(workspace: &Path, root: &str) -> Result<usize, String> {
    let root = root.trim().trim_matches('"');
    if root.is_empty() {
        return Err("Enter the folder of a Foundry project (the one with foundry.toml).".into());
    }
    let path = std::fs::canonicalize(root).map_err(|_| format!("{root} does not exist."))?;
    let count = check(&path)?;
    let text = path.display().to_string().trim_start_matches(r"\\?\").replace('\\', "/");
    crate::config::ChasmConfig::update(workspace, |config| {
        if !config.build.imports.contains(&text) {
            config.build.imports.push(text.clone());
        }
    })
    .map_err(|error| error.to_string())?;
    Ok(count)
}

pub fn remove(workspace: &Path, root: &str) -> Result<(), String> {
    crate::config::ChasmConfig::update(workspace, |config| config.build.imports.retain(|item| item != root.trim())).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn artifacts_become_programs_with_source_and_variable_ranges() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("foundry.toml"), "[profile.default]\nsrc = \"src\"\nout = \"build\"\n").unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/Vault.sol"), "contract Vault {}\r\n").unwrap();
        std::fs::create_dir_all(root.join("build/Vault.sol")).unwrap();
        std::fs::create_dir_all(root.join("build/build-info")).unwrap();
        let artifact = json!({
            "abi": [{"type":"function","name":"total","stateMutability":"view","inputs":[],"outputs":[{"name":"","type":"uint256"}]}],
            "deployedBytecode": {"object": "0x6080__$0123456789abcdef0123456789abcdef01$__00", "sourceMap": "0:17:0:-", "linkReferences": {"src/L.sol": {"L": [{"start": 2, "length": 20}]}}, "immutableReferences": {"5": [{"start": 23, "length": 32}]}},
            "metadata": {"settings": {"compilationTarget": {"src/Vault.sol": "Vault"}}},
            "id": 3,
            "ast": {"nodeType": "SourceUnit"},
            "storageLayout": {"storage": [], "types": {}},
        });
        std::fs::write(root.join("build/Vault.sol/Vault.json"), artifact.to_string()).unwrap();
        // Interfaces have no code; build-info is not an artifact.
        std::fs::write(root.join("build/Vault.sol/IVault.json"), json!({"deployedBytecode": {"object": "0x", "sourceMap": ""}}).to_string()).unwrap();
        std::fs::write(root.join("build/build-info/x.json"), "{}").unwrap();
        let programs = programs(root).unwrap();
        assert_eq!(programs.len(), 1);
        let vault = &programs[0];
        assert_eq!((vault.name.as_str(), vault.source_id), ("Vault", Some(3)));
        assert_eq!(vault.source, "contract Vault {}\n");
        assert_eq!(vault.runtime.len(), 23);
        assert_eq!(&vault.runtime[2..22], &[0u8; 20]);
        assert_eq!(vault.variable, vec![(23, 32), (2, 20)]);
        assert!(vault.abi.is_some() && vault.ast.is_some() && vault.layout.is_some());
        assert!(vault.source_path.ends_with("/src/Vault.sol"));
        assert_eq!(check(root).unwrap(), 1);
        let empty = tempfile::tempdir().unwrap();
        assert!(check(empty.path()).unwrap_err().contains("no foundry.toml"));
        std::fs::write(empty.path().join("foundry.toml"), "").unwrap();
        assert!(check(empty.path()).unwrap_err().contains("Run forge build"));
    }
}
