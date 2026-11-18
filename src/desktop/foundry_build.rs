use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    fs::{self, File, Metadata},
    io::{self, Read},
    path::{Component, Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use ethers::utils::{hex, keccak256};
use foundry_compilers::{artifacts::remappings::Remapping, resolver::parse::SolData};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{io::AsyncReadExt, process::Command};
use tokio_util::sync::CancellationToken;
use walkdir::WalkDir;

const CONFIG_LIMIT: usize = 256 * 1024;
const SOURCE_LIMIT: usize = 2 * 1024 * 1024;
const MANIFEST_BYTES_LIMIT: usize = 64 * 1024 * 1024;
const MANIFEST_FILES_LIMIT: usize = 10_000;
const WALK_ENTRIES_LIMIT: usize = 40_000;
const PATH_BYTES_LIMIT: usize = 4096;
const BUILD_INFO_LIMIT: usize = 8 * 1024 * 1024;
const OUTPUT_LIMIT: usize = 1024 * 1024;
const BUILD_TIMEOUT: Duration = Duration::from_secs(120);
#[cfg(windows)]
const TRUST_WARNING: &str = "A trusted build runs the local forge and solc programs with your user permissions. Offline mode prevents Foundry from downloading a missing compiler; it is not a sandbox and cannot stop a malicious executable or concurrent workspace changes. Windows job containment begins immediately after process launch; a process created before assignment or intentionally escaping the job may survive.";
#[cfg(not(windows))]
const TRUST_WARNING: &str = "A trusted build runs the local forge and solc programs with your user permissions. Offline mode prevents Foundry from downloading a missing compiler; it is not a sandbox and cannot stop a malicious executable or concurrent workspace changes. Cancellation terminates Forge itself but does not contain or terminate its child processes on this platform.";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveValue<T> {
    pub value: T,
    pub explicit: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoundrySettings {
    pub solc: String,
    pub optimizer: EffectiveValue<bool>,
    pub optimizer_runs: EffectiveValue<u32>,
    pub via_ir: EffectiveValue<bool>,
    pub evm_version: String,
    pub bytecode_hash: EffectiveValue<String>,
    pub cbor_metadata: EffectiveValue<bool>,
    pub src: String,
    pub out: String,
    pub libs: Vec<String>,
    pub remappings: Vec<String>,
    pub auto_detect_remappings: bool,
    pub test: String,
    pub script: String,
    pub cache_path: String,
}

impl FoundrySettings {
    pub fn profile_summary(&self) -> String {
        fn origin(explicit: bool) -> &'static str {
            if explicit {
                "explicit"
            } else {
                "Foundry default"
            }
        }
        format!(
            "profile.default\nsolc: {} (explicit)\noptimizer: {} ({})\noptimizer runs: {} ({})\nvia IR: {} ({})\nEVM version: {} (explicit)\nbytecode hash: {} ({})\nCBOR metadata: {} ({})\nsrc: {}\ntest: {}\nscript: {}\nconfigured out: {}\nconfigured cache: {}\nlibs: {}\nremappings: {}\nauto-detect remappings: {}\nThe build uses isolated temporary out and cache directories.",
            self.solc,
            self.optimizer.value, origin(self.optimizer.explicit),
            self.optimizer_runs.value, origin(self.optimizer_runs.explicit),
            self.via_ir.value, origin(self.via_ir.explicit),
            self.evm_version,
            self.bytecode_hash.value, origin(self.bytecode_hash.explicit),
            self.cbor_metadata.value, origin(self.cbor_metadata.explicit),
            self.src, self.test, self.script, self.out, self.cache_path,
            if self.libs.is_empty() { "(none)".into() } else { self.libs.join(", ") },
            if self.remappings.is_empty() { "(none)".into() } else { self.remappings.join(", ") },
            self.auto_detect_remappings,
        )
    }

    #[cfg(test)]
    pub fn matches_required(&self, required: &FoundrySettings) -> bool {
        self.solc == required.solc
            && self.optimizer.value == required.optimizer.value
            && self.optimizer_runs.value == required.optimizer_runs.value
            && self.via_ir.value == required.via_ir.value
            && self.evm_version == required.evm_version
            && self.bytecode_hash.value == required.bytecode_hash.value
            && self.cbor_metadata.value == required.cbor_metadata.value
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceManifestEntry {
    pub path: String,
    pub size: u64,
    pub keccak256: String,
}

#[derive(Clone, Debug)]
pub struct FoundryPreflight {
    pub workspace: PathBuf,
    pub config_path: PathBuf,
    pub settings: FoundrySettings,
    pub source_manifest: Vec<SourceManifestEntry>,
    pub digest: String,
    pub trust_warning: String,
}

impl FoundryPreflight {
    pub fn profile_summary(&self) -> String {
        format!(
            "Workspace: {}\n{}\nReview digest: {}\n{}",
            self.workspace.display(),
            self.settings.profile_summary(),
            self.digest,
            self.trust_warning,
        )
    }
}

#[derive(Clone, Debug)]
pub struct FoundryArtifactSummary {
    pub id: String,
    pub source: String,
    pub contract: String,
    pub abi: Value,
    pub abi_keccak256: String,
    pub creation_bytecode: Option<String>,
    pub runtime_bytecode: Option<String>,
    pub creation_keccak256: Option<String>,
    pub runtime_keccak256: Option<String>,
    pub creation_source_map: Option<String>,
    pub runtime_source_map: Option<String>,
    pub storage_layout: Option<Value>,
    pub method_identifiers: Option<Value>,
    pub creation_link_references: Option<Value>,
    pub runtime_link_references: Option<Value>,
    pub immutable_references: Option<Value>,
    pub linked: bool,
}

impl FoundryArtifactSummary {
    pub fn deployable(&self) -> bool {
        self.linked && self.creation_bytecode.is_some()
    }
}

#[derive(Debug)]
pub struct FoundryBuildReport {
    pub workspace: PathBuf,
    pub preflight_digest: String,
    pub settings: FoundrySettings,
    pub artifacts: Vec<FoundryArtifactSummary>,
    #[allow(dead_code)]
    pub build_info_path: PathBuf,
    #[allow(dead_code)]
    pub build_info: Value,
    pub warnings: Vec<String>,
    pub settings_verified: bool,
    pub sources_verified: bool,
    pub stdout: String,
    pub stderr: String,
    /// Keeps the build folder alive while the report exists.
    #[allow(dead_code)]
    temp_dir: Arc<tempfile::TempDir>,
}

impl FoundryBuildReport {
    pub fn exact_target_eligible(&self) -> bool {
        self.settings_verified && self.sources_verified
    }

    pub fn artifact_deployable(&self, artifact_id: &str) -> bool {
        self.exact_target_eligible()
            && self
                .artifacts
                .iter()
                .any(|artifact| artifact.id == artifact_id && artifact.deployable())
    }

}

#[cfg(test)]
#[path = "foundry_build_tests.rs"]
mod tests;

fn hash(bytes: impl AsRef<[u8]>) -> String {
    format!("0x{}", hex::encode(keccak256(bytes)))
}

fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|e| format!("Cannot inspect {}: {e}", path.display()))?;
    if is_link_or_reparse(&metadata) || !metadata.is_file() {
        return Err(format!(
            "Expected a regular file without a link: {}",
            path.display()
        ));
    }
    if metadata.len() > limit as u64 {
        return Err(format!("File exceeds {limit} bytes: {}", path.display()));
    }
    let mut data = Vec::new();
    File::open(path)
        .and_then(|file| file.take(limit as u64 + 1).read_to_end(&mut data))
        .map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    if data.len() > limit {
        return Err(format!("File exceeds {limit} bytes: {}", path.display()));
    }
    Ok(data)
}

fn is_link_or_reparse(metadata: &Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn no_links(path: &Path) -> Result<(), String> {
    let mut part = PathBuf::new();
    for component in path.components() {
        part.push(component.as_os_str());
        if matches!(component, Component::Prefix(_) | Component::RootDir) {
            continue;
        }
        match fs::symlink_metadata(&part) {
            Ok(metadata) if is_link_or_reparse(&metadata) => {
                return Err(format!(
                    "Links and reparse points are not allowed: {}",
                    part.display()
                ));
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => break,
            Err(e) => return Err(format!("Cannot inspect {}: {e}", part.display())),
        }
    }
    Ok(())
}

fn canonical_workspace(path: &Path) -> Result<PathBuf, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| format!("Cannot resolve workspace: {e}"))?
            .join(path)
    };
    if absolute
        .components()
        .any(|part| part == Component::ParentDir)
    {
        return Err("Workspace path must not contain parent-directory components.".into());
    }
    no_links(&absolute)?;
    let root = fs::canonicalize(&absolute)
        .map_err(|e| format!("Cannot resolve workspace {}: {e}", absolute.display()))?;
    no_links(&root)?;
    if !fs::metadata(&root).map_err(|e| e.to_string())?.is_dir() {
        return Err("Workspace must be a directory.".into());
    }
    Ok(root)
}

fn relative_path(raw: &str, label: &str) -> Result<PathBuf, String> {
    if raw.is_empty()
        || raw.len() > PATH_BYTES_LIMIT
        || raw.contains('\0')
        || raw.contains('\\')
        || raw.contains(':')
        || raw.chars().any(char::is_control)
    {
        return Err(format!("Invalid {label} path."));
    }
    let path = Path::new(raw);
    if path.is_absolute()
        || path.components().any(|part| {
            matches!(
                part,
                Component::Prefix(_) | Component::RootDir | Component::ParentDir
            )
        })
    {
        return Err(format!("{label} must remain inside the workspace: {raw}"));
    }
    Ok(path.to_path_buf())
}

fn confined_path(root: &Path, raw: &str, label: &str) -> Result<PathBuf, String> {
    let path = root.join(relative_path(raw, label)?);
    no_links(&path)?;
    if path.exists() {
        let resolved = fs::canonicalize(&path)
            .map_err(|e| format!("Cannot resolve {}: {e}", path.display()))?;
        if !resolved.starts_with(root) {
            return Err(format!("{label} resolves outside the workspace: {raw}"));
        }
    }
    Ok(path)
}

fn string_value(
    table: &toml::map::Map<String, toml::Value>,
    key: &str,
) -> Result<Option<String>, String> {
    table
        .get(key)
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{key} must be a string."))
        })
        .transpose()
}

fn bool_value(
    table: &toml::map::Map<String, toml::Value>,
    key: &str,
    default: bool,
) -> Result<EffectiveValue<bool>, String> {
    match table.get(key) {
        Some(value) => Ok(EffectiveValue {
            value: value
                .as_bool()
                .ok_or_else(|| format!("{key} must be a boolean."))?,
            explicit: true,
        }),
        None => Ok(EffectiveValue {
            value: default,
            explicit: false,
        }),
    }
}

fn strings_value(
    table: &toml::map::Map<String, toml::Value>,
    key: &str,
    default: &[&str],
) -> Result<Vec<String>, String> {
    let Some(value) = table.get(key) else {
        return Ok(default.iter().map(|value| (*value).to_string()).collect());
    };
    let array = value
        .as_array()
        .ok_or_else(|| format!("{key} must be an array of strings."))?;
    if array.len() > 256 {
        return Err(format!("Too many {key} entries."));
    }
    array
        .iter()
        .map(|item| {
            let string = item
                .as_str()
                .ok_or_else(|| format!("{key} must contain strings."))?;
            if string.is_empty()
                || string.len() > PATH_BYTES_LIMIT
                || string.chars().any(char::is_control)
            {
                return Err(format!("Invalid {key} entry."));
            }
            Ok(string.to_owned())
        })
        .collect()
}

fn parse_version(version: &str) -> bool {
    let mut parts = version.split('.');
    let valid = (0..3).all(|_| {
        parts.next().is_some_and(|part| {
            !part.is_empty()
                && part.len() <= 5
                && part.bytes().all(|b| b.is_ascii_digit())
                && (part.len() == 1 || !part.starts_with('0'))
        })
    });
    valid && parts.next().is_none()
}

fn remapping_target(remapping: &str) -> Result<&str, String> {
    let (prefix, target) = remapping
        .split_once('=')
        .ok_or("Remapping must have the form prefix=path.")?;
    if prefix.is_empty()
        || remapping.len() > PATH_BYTES_LIMIT
        || target.is_empty()
        || target.contains('=')
        || prefix.contains('\\')
        || prefix.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err("Invalid remapping.".into());
    }
    if let Some((context, name)) = prefix.split_once(':') {
        if context.is_empty() || name.is_empty() || name.contains(':') {
            return Err("Invalid remapping context.".into());
        }
        relative_path(context, "remapping context")?;
    }
    if !target.ends_with('/') && !target.ends_with(".sol") {
        return Err("A remapping target must end in / or name a Solidity file.".into());
    }
    relative_path(target, "remapping target")?;
    Ok(target)
}

fn parse_settings(bytes: &[u8]) -> Result<FoundrySettings, String> {
    if bytes.len() > CONFIG_LIMIT {
        return Err("foundry.toml exceeds the configuration size limit.".into());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "foundry.toml must be UTF-8.")?;
    if text
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err("foundry.toml contains unsupported control characters.".into());
    }
    let value: toml::Value =
        toml::from_str(text).map_err(|e| format!("Invalid foundry.toml: {e}"))?;
    let root = value.as_table().ok_or("foundry.toml must be a table.")?;
    for key in root.keys() {
        if !matches!(key.as_str(), "profile" | "fs_permissions") {
            return Err(format!(
                "Unsupported top-level Foundry setting: {key}. Only profile settings and fs_permissions are accepted."
            ));
        }
    }
    if let Some(permissions) = root.get("fs_permissions") {
        let entries = permissions
            .as_array()
            .ok_or("fs_permissions must be an array.")?;
        for entry in entries {
            let entry = entry
                .as_table()
                .ok_or("fs_permissions entries must be tables.")?;
            if entry
                .keys()
                .any(|key| !matches!(key.as_str(), "access" | "path"))
                || entry.get("access").and_then(toml::Value::as_str).is_none()
                || entry.get("path").and_then(toml::Value::as_str).is_none()
            {
                return Err(
                    "fs_permissions entries require only string access and path fields.".into(),
                );
            }
        }
    }
    let profiles = root
        .get("profile")
        .and_then(toml::Value::as_table)
        .ok_or("foundry.toml must contain [profile.default].")?;
    // Only profile.default affects this service: the command pins FOUNDRY_PROFILE=default.
    // Other named profiles are inert but remain part of the reviewed config digest.
    let profile = profiles
        .get("default")
        .and_then(toml::Value::as_table)
        .ok_or("foundry.toml must contain [profile.default].")?;
    const ALLOWED: &[&str] = &[
        "solc",
        "solc_version",
        "optimizer",
        "optimizer_runs",
        "via_ir",
        "evm_version",
        "bytecode_hash",
        "cbor_metadata",
        "src",
        "out",
        "libs",
        "remappings",
        "auto_detect_remappings",
        "test",
        "script",
        "cache_path",
        "auto_detect_solc",
        "offline",
        "fs_permissions",
    ];
    for key in profile.keys() {
        if !ALLOWED.contains(&key.as_str()) {
            return Err(format!("Unsupported profile.default setting: {key}. Review it before using the trusted build service."));
        }
    }
    let solc = string_value(profile, "solc")?;
    let solc_version = string_value(profile, "solc_version")?;
    if solc.is_some() && solc_version.is_some() {
        return Err("Use only one of solc or solc_version.".into());
    }
    let solc = solc
        .or(solc_version)
        .ok_or("An explicit exact solc version is required.")?;
    if !parse_version(&solc) {
        return Err(
            "solc must be an exact version such as 0.8.28, not a path or version range.".into(),
        );
    }
    let evm_version =
        string_value(profile, "evm_version")?.ok_or("An explicit evm_version is required.")?;
    const EVM_VERSIONS: &[&str] = &[
        "homestead",
        "tangerineWhistle",
        "spuriousDragon",
        "byzantium",
        "constantinople",
        "petersburg",
        "istanbul",
        "berlin",
        "london",
        "paris",
        "shanghai",
        "cancun",
        "prague",
        "osaka",
    ];
    if !EVM_VERSIONS.contains(&evm_version.as_str()) {
        return Err("Unsupported or invalid evm_version.".into());
    }
    let optimizer_runs = match profile.get("optimizer_runs") {
        Some(value) => EffectiveValue {
            value: match value {
                toml::Value::Integer(n) => u32::try_from(*n).ok(),
                toml::Value::String(text)
                    if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) =>
                {
                    text.parse::<u32>().ok()
                }
                _ => None,
            }
            .ok_or("optimizer_runs must be a nonnegative 32-bit integer.")?,
            explicit: true,
        },
        None => EffectiveValue {
            value: 200,
            explicit: false,
        },
    };
    let bytecode_hash = match string_value(profile, "bytecode_hash")? {
        Some(value) if ["ipfs", "bzzr1", "none"].contains(&value.as_str()) => EffectiveValue {
            value,
            explicit: true,
        },
        Some(_) => return Err("bytecode_hash must be ipfs, bzzr1, or none.".into()),
        None => EffectiveValue {
            value: "ipfs".into(),
            explicit: false,
        },
    };
    for key in ["auto_detect_solc", "offline"] {
        if let Some(value) = profile.get(key) {
            if value.as_bool().is_none() {
                return Err(format!("{key} must be a boolean."));
            }
        }
    }
    if let Some(permissions) = profile.get("fs_permissions") {
        let entries = permissions
            .as_array()
            .ok_or("fs_permissions must be an array.")?;
        if entries.len() > 64 {
            return Err("Too many fs_permissions entries.".into());
        }
        for entry in entries {
            let entry = entry
                .as_table()
                .ok_or("fs_permissions entries must be tables.")?;
            if entry
                .keys()
                .any(|key| !matches!(key.as_str(), "access" | "path"))
            {
                return Err("fs_permissions entries may contain only access and path.".into());
            }
            let access = entry
                .get("access")
                .and_then(toml::Value::as_str)
                .ok_or("fs_permissions access must be a string.")?;
            if !matches!(access, "read" | "read-write") {
                return Err("fs_permissions access must be read or read-write.".into());
            }
            let path = entry
                .get("path")
                .and_then(toml::Value::as_str)
                .ok_or("fs_permissions path must be a string.")?;
            relative_path(path, "fs_permissions path")?;
        }
    }
    let settings = FoundrySettings {
        solc,
        optimizer: bool_value(profile, "optimizer", false)?,
        optimizer_runs,
        via_ir: bool_value(profile, "via_ir", false)?,
        evm_version,
        bytecode_hash,
        cbor_metadata: bool_value(profile, "cbor_metadata", true)?,
        src: string_value(profile, "src")?.unwrap_or_else(|| "src".into()),
        out: string_value(profile, "out")?.unwrap_or_else(|| "out".into()),
        libs: strings_value(profile, "libs", &["lib"])?,
        remappings: strings_value(profile, "remappings", &[])?,
        auto_detect_remappings: bool_value(profile, "auto_detect_remappings", true)?.value,
        test: string_value(profile, "test")?.unwrap_or_else(|| "test".into()),
        script: string_value(profile, "script")?.unwrap_or_else(|| "script".into()),
        cache_path: string_value(profile, "cache_path")?.unwrap_or_else(|| "cache".into()),
    };
    for (label, path) in [
        ("src", &settings.src),
        ("out", &settings.out),
        ("test", &settings.test),
        ("script", &settings.script),
        ("cache_path", &settings.cache_path),
    ] {
        relative_path(path, label)?;
    }
    for path in &settings.libs {
        relative_path(path, "lib")?;
    }
    for mapping in &settings.remappings {
        remapping_target(mapping)?;
    }
    Ok(settings)
}

fn env_override(key: &OsStr) -> bool {
    let key = key.to_string_lossy().to_ascii_uppercase();
    key.starts_with("FOUNDRY_") || key.starts_with("DAPP_") || key == "SOLC_VERSION"
}

fn reject_env_overrides() -> Result<(), String> {
    if let Some((key, _)) = std::env::vars_os().find(|(key, _)| env_override(key)) {
        return Err(format!("Foundry environment override {} is set. Unset it before reviewing the default profile.", key.to_string_lossy()));
    }
    Ok(())
}

fn remappings_file(root: &Path) -> Result<(Vec<String>, Option<Vec<u8>>), String> {
    let path = root.join("remappings.txt");
    no_links(&path)?;
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok((Vec::new(), None)),
        Err(e) => Err(format!("Cannot inspect remappings.txt: {e}")),
        Ok(_) => {
            let bytes = read_bounded(&path, CONFIG_LIMIT)?;
            let text = std::str::from_utf8(&bytes).map_err(|_| "remappings.txt must be UTF-8.")?;
            let mut remappings = Vec::new();
            for line in text
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty() && !line.starts_with('#'))
            {
                if remappings.len() >= 256 {
                    return Err("Too many remappings.txt entries.".into());
                }
                remapping_target(line)?;
                remappings.push(line.to_owned());
            }
            Ok((remappings, Some(bytes)))
        }
    }
}

fn portable_relative(root: &Path, path: &Path) -> Result<String, String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| "Path escaped the workspace.")?;
    let mut parts = Vec::new();
    for part in relative.components() {
        match part {
            Component::Normal(name) => {
                let name = name
                    .to_str()
                    .ok_or("Workspace contains a non-UTF-8 path.")?;
                if name.chars().any(char::is_control) || name.contains('\\') || name.contains(':') {
                    return Err(
                        "Workspace contains a source path that cannot be represented safely."
                            .into(),
                    );
                }
                parts.push(name);
            }
            Component::CurDir => {}
            _ => return Err("Path escaped the workspace.".into()),
        }
    }
    let result = parts.join("/");
    if result.is_empty() || result.len() > PATH_BYTES_LIMIT {
        return Err("Invalid workspace-relative path.".into());
    }
    Ok(result)
}

fn scan_manifest(root: &Path) -> Result<Vec<SourceManifestEntry>, String> {
    let mut manifest = Vec::new();
    let mut total = 0usize;
    let mut count = 0usize;
    for entry in WalkDir::new(root)
        .follow_links(false)
        .min_depth(1)
        .max_depth(64)
    {
        let entry = entry.map_err(|e| format!("Cannot scan workspace: {e}"))?;
        count += 1;
        if count > WALK_ENTRIES_LIMIT {
            return Err("Workspace entry limit exceeded.".into());
        }
        if entry.depth() == 64 && entry.file_type().is_dir() {
            return Err("Workspace nesting limit exceeded.".into());
        }
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|e| format!("Cannot inspect workspace entry: {e}"))?;
        if is_link_or_reparse(&metadata) {
            return Err(format!(
                "Workspace contains a link or reparse point: {}",
                entry.path().display()
            ));
        }
        if !metadata.is_file() && !metadata.is_dir() {
            return Err(format!(
                "Unsupported workspace entry: {}",
                entry.path().display()
            ));
        }
        if metadata.is_file()
            && entry
                .path()
                .extension()
                .and_then(OsStr::to_str)
                .is_some_and(|ext| ext.eq_ignore_ascii_case("yul"))
        {
            return Err(format!(
                "Standalone Yul sources are not supported by the reviewed import policy: {}",
                entry.path().display()
            ));
        }
        if metadata.is_file()
            && entry
                .path()
                .extension()
                .and_then(OsStr::to_str)
                .is_some_and(|ext| {
                    ext.eq_ignore_ascii_case("sol") || ext.eq_ignore_ascii_case("yul")
                })
        {
            if manifest.len() >= MANIFEST_FILES_LIMIT {
                return Err("Solidity source count limit exceeded.".into());
            }
            let bytes = read_bounded(entry.path(), SOURCE_LIMIT)?;
            total = total
                .checked_add(bytes.len())
                .ok_or("Source size limit exceeded.")?;
            if total > MANIFEST_BYTES_LIMIT {
                return Err("Solidity source size limit exceeded.".into());
            }
            manifest.push(SourceManifestEntry {
                path: portable_relative(root, entry.path())?,
                size: bytes.len() as u64,
                keccak256: hash(&bytes),
            });
        }
    }
    manifest.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(manifest)
}

fn normalize_under_root(base: &Path, relative: &Path) -> Option<PathBuf> {
    let mut result = base.to_path_buf();
    for component in relative.components() {
        match component {
            Component::Normal(name) => result.push(name),
            Component::CurDir => {}
            Component::ParentDir => {
                if !result.pop() {
                    return None;
                }
            }
            _ => return None,
        }
    }
    Some(result)
}

fn validate_imports(
    root: &Path,
    manifest: &[SourceManifestEntry],
    remappings: &[String],
) -> Result<(), String> {
    let known: BTreeSet<&str> = manifest.iter().map(|entry| entry.path.as_str()).collect();
    for entry in manifest.iter().filter(|entry| {
        Path::new(&entry.path)
            .extension()
            .and_then(OsStr::to_str)
            .is_some_and(|ext| ext.eq_ignore_ascii_case("sol"))
    }) {
        let path = root.join(&entry.path);
        let bytes = read_bounded(&path, SOURCE_LIMIT)?;
        if bytes.len() as u64 != entry.size || hash(&bytes) != entry.keccak256 {
            return Err("Workspace changed during preflight. Review it again.".into());
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| format!("Source is not UTF-8: {}", entry.path))?;
        let parsed = SolData::parse(text, &path);
        if parsed.parse_result.is_err() {
            return Err(format!(
                "Cannot safely inspect Solidity imports in {}.",
                entry.path
            ));
        }
        for import in parsed.imports {
            let raw = import.data.path();
            let raw_text = raw.to_str().ok_or("Invalid Solidity import path.")?;
            if raw_text.is_empty()
                || raw_text.contains('\\')
                || raw_text.contains(':')
                || raw.is_absolute()
                || raw_text.chars().any(char::is_control)
            {
                return Err(format!("Unsupported import in {}: {raw_text}", entry.path));
            }
            if raw
                .components()
                .next()
                .is_some_and(|c| matches!(c, Component::CurDir | Component::ParentDir))
            {
                let base = Path::new(&entry.path).parent().unwrap_or(Path::new(""));
                let target = normalize_under_root(base, raw).ok_or_else(|| {
                    format!("Import escapes workspace in {}: {raw_text}", entry.path)
                })?;
                let target = target
                    .to_str()
                    .ok_or("Invalid import path.")?
                    .replace('\\', "/");
                if !known.contains(target.as_str()) {
                    return Err(format!(
                        "Relative import is not in the reviewed source manifest: {} -> {raw_text}",
                        entry.path
                    ));
                }
            } else if raw.components().any(|c| {
                matches!(
                    c,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            }) {
                return Err(format!("Unsupported import in {}: {raw_text}", entry.path));
            }
            for mapping in remappings {
                let (prefix, target) = mapping.split_once('=').ok_or("Invalid remapping.")?;
                let (context, prefix) = prefix
                    .split_once(':')
                    .map_or((None, prefix), |(c, p)| (Some(c), p));
                if context.is_some_and(|c| !entry.path.starts_with(c)) {
                    continue;
                }
                if let Some(suffix) = raw_text.strip_prefix(prefix) {
                    let combined = format!("{target}{suffix}");
                    relative_path(&combined, "remapped import")?;
                    confined_path(root, &combined, "remapped import")?;
                    if !known.contains(combined.trim_start_matches("./")) {
                        return Err(format!("Remapped import is not in the reviewed source manifest: {} -> {raw_text}", entry.path));
                    }
                }
            }
        }
    }
    Ok(())
}

fn add_digest_field(data: &mut Vec<u8>, bytes: &[u8]) {
    data.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    data.extend_from_slice(bytes);
}

pub fn preflight(workspace: &Path) -> Result<FoundryPreflight, String> {
    reject_env_overrides()?;
    let root = canonical_workspace(workspace)?;
    let config_path = root.join("foundry.toml");
    no_links(&config_path)?;
    let config = read_bounded(&config_path, CONFIG_LIMIT).map_err(|e| {
        format!("The exact workspace root must contain a readable foundry.toml. {e}")
    })?;
    #[cfg(windows)]
    let user_home = std::env::var_os("USERPROFILE").map(PathBuf::from);
    #[cfg(not(windows))]
    let user_home = std::env::var_os("HOME").map(PathBuf::from);
    let user_home = user_home
        .ok_or("Cannot determine the user home to check for global Foundry configuration.")?;
    let global_config = user_home.join(".foundry").join("foundry.toml");
    match fs::symlink_metadata(&global_config) {
        Ok(_) => return Err("Global ~/.foundry/foundry.toml is present; its settings cannot be included in this workspace-only review.".into()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {},
        Err(e) => return Err(format!("Cannot inspect global Foundry configuration: {e}")),
    }
    let mut settings = parse_settings(&config)?;
    for (label, path) in [
        ("src", &settings.src),
        ("out", &settings.out),
        ("test", &settings.test),
        ("script", &settings.script),
        ("cache_path", &settings.cache_path),
    ] {
        confined_path(&root, path, label)?;
    }
    for path in &settings.libs {
        confined_path(&root, path, "lib")?;
    }
    let (file_remappings, remappings_bytes) = remappings_file(&root)?;
    settings.remappings.extend(file_remappings);
    let source_manifest = scan_manifest(&root)?;
    if settings.auto_detect_remappings {
        for lib in &settings.libs {
            let directory = confined_path(&root, lib, "lib")?;
            if directory.is_dir() {
                for mapping in Remapping::find_many(&directory) {
                    let target = Path::new(&mapping.path);
                    let relative = portable_relative(&root, target)?;
                    let rendered = format!("{}={}/", mapping.name, relative.trim_end_matches('/'));
                    if !settings.remappings.contains(&rendered) {
                        settings.remappings.push(rendered);
                    }
                    if settings.remappings.len() > 1024 {
                        return Err("Too many effective remappings.".into());
                    }
                }
            }
        }
    }
    for mapping in &settings.remappings {
        let target = remapping_target(mapping)?;
        confined_path(&root, target, "remapping target")?;
    }
    validate_imports(&root, &source_manifest, &settings.remappings)?;
    let mut digest_data = Vec::new();
    add_digest_field(&mut digest_data, b"chasm-foundry-preflight-v1");
    add_digest_field(&mut digest_data, root.to_string_lossy().as_bytes());
    add_digest_field(&mut digest_data, &config);
    add_digest_field(
        &mut digest_data,
        if remappings_bytes.is_some() {
            b"present"
        } else {
            b"absent"
        },
    );
    if let Some(bytes) = &remappings_bytes {
        add_digest_field(&mut digest_data, bytes);
    }
    for entry in &source_manifest {
        add_digest_field(&mut digest_data, entry.path.as_bytes());
        digest_data.extend_from_slice(&entry.size.to_be_bytes());
        add_digest_field(&mut digest_data, entry.keccak256.as_bytes());
    }
    for mapping in &settings.remappings {
        add_digest_field(&mut digest_data, mapping.as_bytes());
    }
    let digest = hash(digest_data);
    let trust_warning = format!(
        "{TRUST_WARNING}\nThis review inspected {} Solidity sources. Source and configuration changes require a new approval.",
        source_manifest.len()
    );
    Ok(FoundryPreflight {
        workspace: root,
        config_path,
        settings,
        source_manifest,
        digest,
        trust_warning,
    })
}

#[derive(Debug)]
struct BuildCommand {
    program: OsString,
    args: Vec<OsString>,
    cwd: PathBuf,
    solc: PathBuf,
}

fn command_plan(review: &FoundryPreflight, temp: &Path, solc: &Path) -> BuildCommand {
    let mut args = Vec::new();
    let out = temp.join("out");
    let cache = temp.join("cache");
    let build_info = temp.join("build-info");
    for arg in [
        OsStr::new("build"),
        OsStr::new("--build-info"),
        OsStr::new("--offline"),
        OsStr::new("--no-auto-detect"),
        OsStr::new("--extra-output"),
        OsStr::new("storageLayout"),
        OsStr::new("evm.methodIdentifiers"),
        OsStr::new("evm.bytecode.sourceMap"),
        OsStr::new("evm.bytecode.linkReferences"),
        OsStr::new("evm.deployedBytecode.sourceMap"),
        OsStr::new("evm.deployedBytecode.linkReferences"),
        OsStr::new("evm.deployedBytecode.immutableReferences"),
        OsStr::new("--use"),
        solc.as_os_str(),
        OsStr::new("--root"),
        review.workspace.as_os_str(),
        OsStr::new("--config-path"),
        review.config_path.as_os_str(),
        OsStr::new("--out"),
        out.as_os_str(),
        OsStr::new("--cache-path"),
        cache.as_os_str(),
        OsStr::new("--build-info-path"),
        build_info.as_os_str(),
        OsStr::new("--color"),
        OsStr::new("never"),
    ] {
        args.push(arg.to_os_string());
    }
    BuildCommand {
        program: OsString::from("forge"),
        args,
        cwd: review.workspace.clone(),
        solc: solc.to_path_buf(),
    }
}

fn local_solc(version: &str) -> Result<PathBuf, String> {
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from);
    #[cfg(not(windows))]
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let home = home.ok_or("Cannot locate the local solc cache.")?;
    let default_cache = home.join(".svm");
    #[cfg(windows)]
    let data_cache = std::env::var_os("APPDATA").map(|v| PathBuf::from(v).join("svm"));
    #[cfg(not(windows))]
    let data_cache = Some(
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"))
            .join("svm"),
    );
    let mut caches = vec![default_cache];
    if let Some(cache) = data_cache {
        if !caches.contains(&cache) {
            caches.push(cache);
        }
    }
    for cache in &caches {
        match local_solc_in_cache(cache, version) {
            Ok(binary) => return Ok(binary),
            Err(error) if error.contains("is not installed locally") => {}
            Err(error) => return Err(error),
        }
    }
    Err(format!(
        "solc {version} is not installed locally. Offline mode will not install it."
    ))
}

fn local_solc_in_cache(cache: &Path, version: &str) -> Result<PathBuf, String> {
    if !parse_version(version) {
        return Err("Local solc version must be exact.".into());
    }
    let directory = cache.join(version);
    let base = format!("solc-{version}");
    #[cfg(windows)]
    let candidates = [base.clone(), format!("{base}.exe")];
    #[cfg(not(windows))]
    let candidates = [base];
    for candidate in candidates {
        let binary = directory.join(candidate);
        match fs::metadata(&binary) {
            Ok(metadata) if metadata.is_file() => {
                return fs::canonicalize(&binary)
                    .map_err(|e| format!("Cannot resolve local solc {version}: {e}"));
            }
            Ok(_) => return Err(format!("Local solc {version} is not a regular file.")),
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("Cannot inspect local solc {version}: {e}")),
        }
    }
    Err(format!(
        "solc {version} is not installed locally. Offline mode will not install it."
    ))
}

fn into_command(plan: &BuildCommand) -> Command {
    let mut command = Command::new(&plan.program);
    command
        .args(&plan.args)
        .current_dir(&plan.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (key, _) in std::env::vars_os() {
        if env_override(&key) {
            command.env_remove(key);
        }
    }
    command.env("FOUNDRY_PROFILE", "default");
    command.env("FOUNDRY_SOLC", &plan.solc);
    #[cfg(windows)]
    command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    command
}

#[derive(Debug, PartialEq, Eq)]
enum ProcessError {
    Cancelled,
    Timeout,
    TooLarge,
    Io(String),
    Exit(String),
}

async fn read_pipe<R: tokio::io::AsyncRead + Unpin>(
    mut pipe: R,
    used: &AtomicUsize,
    limit: usize,
) -> Result<Vec<u8>, ProcessError> {
    let mut output = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let count = pipe
            .read(&mut chunk)
            .await
            .map_err(|e| ProcessError::Io(e.to_string()))?;
        if count == 0 {
            return Ok(output);
        }
        if used
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                n.checked_add(count).filter(|sum| *sum <= limit)
            })
            .is_err()
        {
            return Err(ProcessError::TooLarge);
        }
        output.extend_from_slice(&chunk[..count]);
    }
}

#[cfg(windows)]
struct WindowsJob(std::os::windows::io::OwnedHandle);

#[cfg(windows)]
impl WindowsJob {
    fn new() -> io::Result<Self> {
        use std::os::windows::io::{AsRawHandle, FromRawHandle};
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };

        let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        let handle = unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(raw) };
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = unsafe {
            SetInformationJobObject(
                handle.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if configured == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(handle))
    }

    fn assign(&self, child: &tokio::process::Child) -> io::Result<()> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;

        let process = child.raw_handle().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "Forge exited before job assignment",
            )
        })?;
        if unsafe { AssignProcessToJobObject(self.0.as_raw_handle(), process) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn terminate(&self) -> io::Result<()> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;

        if unsafe { TerminateJobObject(self.0.as_raw_handle(), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

async fn terminate(child: &mut tokio::process::Child) -> Result<(), ProcessError> {
    if child
        .try_wait()
        .map_err(|e| ProcessError::Io(e.to_string()))?
        .is_some()
    {
        return Ok(());
    }
    if let Err(error) = child.start_kill() {
        if child
            .try_wait()
            .map_err(|e| ProcessError::Io(e.to_string()))?
            .is_some()
        {
            return Ok(());
        }
        return Err(ProcessError::Io(format!("Cannot kill Forge: {error}")));
    }
    match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(error)) => Err(ProcessError::Io(format!("Cannot reap Forge: {error}"))),
        Err(_) => Err(ProcessError::Io(
            "Timed out reaping Forge after termination.".into(),
        )),
    }
}

async fn terminate_tree(
    child: &mut tokio::process::Child,
    #[cfg(windows)] job: &WindowsJob,
) -> Result<(), ProcessError> {
    #[cfg(windows)]
    let job_result = job.terminate();
    let child_result = terminate(child).await;
    #[cfg(windows)]
    if let Err(error) = job_result {
        child_result?;
        return Err(ProcessError::Io(format!(
            "Cannot terminate Forge process job: {error}"
        )));
    }
    child_result
}

async fn process_deadline<F: std::future::Future>(
    work: F,
    cancel: &CancellationToken,
    timeout: Duration,
) -> Result<F::Output, ProcessError> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(ProcessError::Cancelled),
        result = tokio::time::timeout(timeout, work) => result.map_err(|_| ProcessError::Timeout),
    }
}

async fn collect_process(
    child: &mut tokio::process::Child,
    #[cfg(windows)] job: &WindowsJob,
    cancel: &CancellationToken,
    timeout: Duration,
    limit: usize,
) -> Result<(String, String), ProcessError> {
    let (stdout, stderr) = match (child.stdout.take(), child.stderr.take()) {
        (Some(stdout), Some(stderr)) => (stdout, stderr),
        _ => {
            terminate_tree(
                child,
                #[cfg(windows)]
                job,
            )
            .await?;
            return Err(ProcessError::Io("Missing Forge output pipes.".into()));
        }
    };
    let used = AtomicUsize::new(0);
    let result = {
        let work = async {
            let (stdout, stderr, status) = tokio::try_join!(
                read_pipe(stdout, &used, limit),
                read_pipe(stderr, &used, limit),
                async {
                    child
                        .wait()
                        .await
                        .map_err(|e| ProcessError::Io(e.to_string()))
                }
            )?;
            let stdout = String::from_utf8_lossy(&stdout).into_owned();
            let stderr = String::from_utf8_lossy(&stderr).into_owned();
            if status.success() {
                Ok((stdout, stderr))
            } else {
                Err(ProcessError::Exit(format!(
                    "Forge exited with {status}.\n{stderr}\n{stdout}"
                )))
            }
        };
        match process_deadline(work, cancel, timeout).await {
            Ok(result) => result,
            Err(error) => Err(error),
        }
    };
    if result.is_err() {
        terminate_tree(
            child,
            #[cfg(windows)]
            job,
        )
        .await?;
    }
    result
}

fn process_error(error: ProcessError) -> String {
    match error {
        ProcessError::Cancelled => "Foundry build cancelled; Forge was terminated.".into(),
        ProcessError::Timeout => "Foundry build exceeded 120 seconds; Forge was terminated.".into(),
        ProcessError::TooLarge => {
            "Foundry output exceeded the combined 1 MiB limit; Forge was terminated.".into()
        }
        ProcessError::Io(error) | ProcessError::Exit(error) => error,
    }
}

fn build_info_file(dir: &Path) -> Result<PathBuf, String> {
    no_links(dir)?;
    let mut paths = Vec::new();
    for entry in
        fs::read_dir(dir).map_err(|e| format!("Cannot read isolated build-info directory: {e}"))?
    {
        let entry = entry.map_err(|e| format!("Cannot inspect build-info directory: {e}"))?;
        if paths.len() >= 2 {
            return Err("More than one build-info unit was produced; select a single compiler unit separately.".into());
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|e| format!("Cannot inspect build-info file: {e}"))?;
        if is_link_or_reparse(&metadata)
            || !metadata.is_file()
            || path.extension() != Some(OsStr::new("json"))
        {
            return Err("Unexpected entry in the isolated build-info directory.".into());
        }
        paths.push(path);
    }
    if paths.len() != 1 {
        return Err(format!(
            "Expected exactly one build-info unit, found {}.",
            paths.len()
        ));
    }
    Ok(paths.remove(0))
}

fn settings_warnings(info: &Value, expected: &FoundrySettings) -> Vec<String> {
    let mut warnings = Vec::new();
    let version = info.get("solcVersion").and_then(Value::as_str);
    if version != Some(expected.solc.as_str()) {
        warnings.push(format!(
            "solc version mismatch or missing: expected {}, got {}.",
            expected.solc,
            version.unwrap_or("missing")
        ));
    }
    let settings = info.pointer("/input/settings");
    let optimizer = settings
        .and_then(|v| v.pointer("/optimizer/enabled"))
        .and_then(Value::as_bool);
    let runs = settings
        .and_then(|v| v.pointer("/optimizer/runs"))
        .and_then(Value::as_u64);
    let via_ir = settings
        .and_then(|v| v.get("viaIR"))
        .and_then(Value::as_bool);
    let evm = settings
        .and_then(|v| v.get("evmVersion"))
        .and_then(Value::as_str);
    let bytecode_hash = settings
        .and_then(|v| v.pointer("/metadata/bytecodeHash"))
        .and_then(Value::as_str);
    let cbor = settings
        .and_then(|v| v.pointer("/metadata/appendCBOR"))
        .and_then(Value::as_bool);
    if settings
        .and_then(|v| v.pointer("/metadata/useLiteralContent"))
        .is_some_and(|value| value != &Value::Bool(false))
    {
        warnings.push(
            "Compiler metadata literal-content setting differs from the reviewed Foundry default."
                .into(),
        );
    }
    if settings
        .and_then(|v| v.pointer("/optimizer/details"))
        .is_some_and(|value| !value.is_null())
    {
        warnings.push(
            "Compiler output includes optimizer details not modeled by the reviewed profile."
                .into(),
        );
    }
    if settings
        .and_then(|v| v.get("libraries"))
        .is_some_and(|value| {
            !value.is_null()
                && value
                    .as_object()
                    .map_or(true, |libraries| !libraries.is_empty())
        })
    {
        warnings.push("Compiler output includes library linking settings not modeled by the reviewed profile.".into());
    }
    if optimizer != Some(expected.optimizer.value) {
        warnings.push(
            "Optimizer enabled value differs from the reviewed profile or is missing.".into(),
        );
    }
    if runs != Some(expected.optimizer_runs.value as u64) {
        warnings
            .push("Optimizer runs value differs from the reviewed profile or is missing.".into());
    }
    if via_ir != Some(expected.via_ir.value) {
        warnings.push("viaIR value differs from the reviewed profile or is missing.".into());
    }
    if evm != Some(expected.evm_version.as_str()) {
        warnings.push("EVM version differs from the reviewed profile or is missing.".into());
    }
    if bytecode_hash != Some(expected.bytecode_hash.value.as_str()) {
        warnings
            .push("Bytecode metadata hash differs from the reviewed profile or is missing.".into());
    }
    if cbor != Some(expected.cbor_metadata.value) {
        warnings
            .push("CBOR metadata setting differs from the reviewed profile or is missing.".into());
    }
    warnings
}

fn normalize_line_endings(bytes: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
            index += 1;
        }
        output.push(bytes[index]);
        index += 1;
    }
    output
}

// Only non-ASCII escapes are interchangeable inside comments. ASCII escapes may spell
// a NatSpec directive or a comment delimiter and must remain distinguishable.
fn normalize_comment_encoding(comment: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(comment.len());
    let mut index = 0;
    while index < comment.len() {
        if comment[index] == b'\\' && comment.get(index + 1) == Some(&b'u') {
            if let Some(digits) = comment.get(index + 2..index + 6) {
                let value = digits.iter().try_fold(0u16, |value, digit| {
                    let digit = (*digit as char).to_digit(16)?;
                    Some(value * 16 + digit as u16)
                });
                if let Some(value) = value.filter(|value| *value >= 0x80) {
                    if let Some(character) = char::from_u32(u32::from(value)) {
                        output.extend_from_slice(character.encode_utf8(&mut [0; 4]).as_bytes());
                        index += 6;
                        continue;
                    }
                }
            }
        }
        output.push(comment[index]);
        index += 1;
    }
    output
}

// Preserve every comment and every uninterrupted code span. Ignoring whitespace
// only between spans is deliberately stricter than comparing Solidity tokens.
fn solidity_without_trivia(bytes: &[u8]) -> Option<Vec<u8>> {
    std::str::from_utf8(bytes).ok()?;
    let normalized = normalize_line_endings(bytes);
    let bytes = normalized.as_slice();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    let mut code_ended_at = None;
    while index < bytes.len() {
        let start = index;
        match bytes[index] {
            b' ' | b'\t' | b'\r' | b'\n' => index += 1,
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index += 2;
                while index < bytes.len() && !matches!(bytes[index], b'\r' | b'\n') {
                    index += 1;
                }
                let comment = normalize_comment_encoding(&bytes[start..index]);
                output.push(b'c');
                add_digest_field(&mut output, &comment);
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index += 2;
                let mut closed = false;
                while index + 1 < bytes.len() {
                    if bytes[index] == b'*' && bytes[index + 1] == b'/' {
                        index += 2;
                        closed = true;
                        break;
                    }
                    index += 1;
                }
                if !closed {
                    return None;
                }
                let comment = normalize_comment_encoding(&bytes[start..index]);
                output.push(b'c');
                add_digest_field(&mut output, &comment);
            }
            quote @ (b'\'' | b'"') => {
                index += 1;
                let mut closed = false;
                while index < bytes.len() {
                    let byte = bytes[index];
                    index += 1;
                    if byte == b'\\' {
                        bytes.get(index)?;
                        index += 1;
                    } else if byte == quote {
                        closed = true;
                        break;
                    }
                }
                if !closed {
                    return None;
                }
                // Prefixes such as unicode and hex must touch their quotes.
                output.push(b's');
                output.push(u8::from(code_ended_at == Some(start)));
                add_digest_field(&mut output, &bytes[start..index]);
            }
            _ => {
                while index < bytes.len()
                    && !matches!(bytes[index], b' ' | b'\t' | b'\r' | b'\n' | b'\'' | b'"')
                    && !(bytes[index] == b'/' && matches!(bytes.get(index + 1), Some(b'/' | b'*')))
                {
                    index += 1;
                }
                if index == start {
                    return None;
                }
                output.push(b't');
                add_digest_field(&mut output, &bytes[start..index]);
                code_ended_at = Some(index);
            }
        }
    }
    Some(output)
}

fn solidity_sources_match(reviewed: &[u8], compiled: &[u8]) -> bool {
    match (
        solidity_without_trivia(reviewed),
        solidity_without_trivia(compiled),
    ) {
        (Some(reviewed), Some(compiled)) => reviewed == compiled,
        _ => false,
    }
}

fn source_warnings(info: &Value, manifest: &[SourceManifestEntry], root: &Path) -> Vec<String> {
    let mut warnings = Vec::new();
    let by_name: BTreeMap<&str, &SourceManifestEntry> = manifest
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect();
    let Some(sources) = info.pointer("/input/sources").and_then(Value::as_object) else {
        return vec!["Build-info does not contain compiler input sources.".into()];
    };
    if sources.is_empty() {
        warnings.push("Build-info contains no compiler input sources.".into());
    }
    for (raw, value) in sources {
        let name = raw.strip_prefix("./").unwrap_or(raw);
        let Some(entry) = by_name.get(name) else {
            warnings.push(format!(
                "Compiler input source was not in the reviewed manifest: {raw}"
            ));
            continue;
        };
        match value.get("content").and_then(Value::as_str) {
            Some(content)
                if content.len() as u64 == entry.size
                    && hash(content.as_bytes()) == entry.keccak256 => {}
            Some(content) => {
                let reviewed = read_bounded(&root.join(&entry.path), SOURCE_LIMIT).ok();
                let same_code = reviewed.as_deref().is_some_and(|bytes| {
                    bytes.len() as u64 == entry.size
                        && hash(bytes) == entry.keccak256
                        && solidity_sources_match(bytes, content.as_bytes())
                });
                if !same_code {
                    warnings.push(format!("Compiler input source code differs from or cannot be checked against the reviewed manifest: {raw}"));
                }
            }
            None => warnings.push(format!("Compiler input source content is missing: {raw}")),
        }
    }
    if let Some(contracts) = info.pointer("/output/contracts").and_then(Value::as_object) {
        for source in contracts.keys() {
            if !sources.contains_key(source) {
                warnings.push(format!(
                    "Compiler output source has no corresponding reviewed input: {source}"
                ));
            }
        }
    }
    warnings
}

fn bytecode(value: Option<&Value>) -> Result<(Option<String>, Option<String>, bool), String> {
    let Some(value) = value else {
        return Ok((None, None, false));
    };
    let text = value
        .as_str()
        .ok_or("Invalid bytecode object in build-info.")?;
    let text = text.strip_prefix("0x").unwrap_or(text);
    if text.is_empty() {
        return Ok((None, None, false));
    }
    if text.len() > BUILD_INFO_LIMIT {
        return Err("Bytecode exceeds the build-info limit.".into());
    }
    if !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok((None, None, true));
    }
    let bytes = hex::decode(text).map_err(|_| "Invalid hexadecimal bytecode in build-info.")?;
    Ok((
        Some(format!("0x{}", hex::encode(&bytes))),
        Some(hash(bytes)),
        false,
    ))
}

fn optional_string(value: Option<&Value>, label: &str) -> Result<Option<String>, String> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if value.len() <= BUILD_INFO_LIMIT => Ok(Some(value.clone())),
        _ => Err(format!("Invalid {label} in build-info.")),
    }
}

fn optional_object(value: Option<&Value>, label: &str) -> Result<Option<Value>, String> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(_)) => Ok(value.cloned()),
        _ => Err(format!("Invalid {label} in build-info.")),
    }
}

fn artifact_id(digest: &str, source: &str, contract: &str) -> String {
    let mut input = Vec::new();
    add_digest_field(&mut input, b"chasm-foundry-artifact-v1");
    add_digest_field(&mut input, digest.as_bytes());
    add_digest_field(&mut input, source.as_bytes());
    add_digest_field(&mut input, contract.as_bytes());
    hash(input)
}

fn artifact_summaries(
    info: &Value,
    digest: &str,
) -> Result<(Vec<FoundryArtifactSummary>, Vec<String>), String> {
    let contracts = info
        .pointer("/output/contracts")
        .and_then(Value::as_object)
        .ok_or("Build-info has no compiler output contracts.")?;
    let mut summaries = Vec::new();
    let mut warnings = Vec::new();
    for (source, contracts) in contracts {
        let contracts = contracts
            .as_object()
            .ok_or("Invalid build-info contract collection.")?;
        for (contract, artifact) in contracts {
            if summaries.len() >= MANIFEST_FILES_LIMIT {
                return Err("Build-info contract count limit exceeded.".into());
            }
            let abi = artifact
                .get("abi")
                .filter(|v| v.is_array())
                .cloned()
                .ok_or("A contract is missing its ABI.")?;
            let abi_bytes =
                serde_json::to_vec(&abi).map_err(|e| format!("Cannot serialize ABI: {e}"))?;
            let (creation_bytecode, creation_keccak256, creation_unlinked) =
                bytecode(artifact.pointer("/evm/bytecode/object"))?;
            let (runtime_bytecode, runtime_keccak256, runtime_unlinked) =
                bytecode(artifact.pointer("/evm/deployedBytecode/object"))?;
            let creation_link_references = optional_object(
                artifact.pointer("/evm/bytecode/linkReferences"),
                "creation link references",
            )?;
            let runtime_link_references = optional_object(
                artifact.pointer("/evm/deployedBytecode/linkReferences"),
                "runtime link references",
            )?;
            let references_present = creation_link_references
                .as_ref()
                .and_then(Value::as_object)
                .is_some_and(|references| !references.is_empty())
                || runtime_link_references
                    .as_ref()
                    .and_then(Value::as_object)
                    .is_some_and(|references| !references.is_empty());
            let linked = !creation_unlinked && !runtime_unlinked && !references_present;
            if !linked {
                warnings.push(format!("{source}:{contract} contains link references or unresolved bytecode; it cannot be deployed by this service."));
            }
            let creation_bytecode = if linked { creation_bytecode } else { None };
            let creation_source_map = optional_string(
                artifact.pointer("/evm/bytecode/sourceMap"),
                "creation source map",
            )?;
            let runtime_source_map = optional_string(
                artifact.pointer("/evm/deployedBytecode/sourceMap"),
                "runtime source map",
            )?;
            let storage_layout = optional_object(artifact.get("storageLayout"), "storage layout")?;
            let method_identifiers = optional_object(
                artifact.pointer("/evm/methodIdentifiers"),
                "method identifiers",
            )?;
            if creation_bytecode.is_some()
                && (creation_source_map.is_none()
                    || runtime_source_map.is_none()
                    || storage_layout.is_none()
                    || method_identifiers.is_none())
            {
                warnings.push(format!("{source}:{contract} lacks requested debugger source maps, storage layout, or method identifiers."));
            }
            let immutable_references = optional_object(
                artifact.pointer("/evm/deployedBytecode/immutableReferences"),
                "immutable references",
            )?;
            if runtime_bytecode.is_some() && immutable_references.is_none() {
                warnings.push(format!("{source}:{contract} has no immutable reference information; an exact runtime match cannot be assumed."));
            }
            summaries.push(FoundryArtifactSummary {
                id: artifact_id(digest, source, contract),
                source: source.clone(),
                contract: contract.clone(),
                abi,
                abi_keccak256: hash(abi_bytes),
                creation_bytecode,
                runtime_bytecode,
                creation_keccak256,
                runtime_keccak256,
                creation_source_map,
                runtime_source_map,
                storage_layout,
                method_identifiers,
                creation_link_references,
                runtime_link_references,
                immutable_references,
                linked,
            });
        }
    }
    Ok((summaries, warnings))
}

pub async fn build(
    review: &FoundryPreflight,
    trusted: bool,
    cancel: CancellationToken,
) -> Result<FoundryBuildReport, String> {
    if !trusted {
        return Err("Explicit workspace trust is required before running Forge.".into());
    }
    if cancel.is_cancelled() {
        return Err("Foundry build cancelled before starting.".into());
    }
    let current = preflight(&review.workspace)?;
    if current.workspace != review.workspace
        || current.digest != review.digest
        || current.settings != review.settings
        || current.source_manifest != review.source_manifest
        || current.config_path != review.config_path
    {
        return Err(
            "Workspace or Foundry settings changed after review. Review again before building."
                .into(),
        );
    }
    if cancel.is_cancelled() {
        return Err("Foundry build cancelled before starting.".into());
    }
    let solc = local_solc(&review.settings.solc)?;
    let temp_dir = Arc::new(
        tempfile::Builder::new()
            .prefix("chasm-foundry-")
            .tempdir()
            .map_err(|e| format!("Cannot create isolated Foundry output: {e}"))?,
    );
    let temp_root = fs::canonicalize(temp_dir.path())
        .map_err(|e| format!("Cannot resolve isolated Foundry output: {e}"))?;
    if temp_root.starts_with(&review.workspace) {
        return Err("System temporary directory must be outside the reviewed workspace.".into());
    }
    if cancel.is_cancelled() {
        return Err("Foundry build cancelled before starting.".into());
    }
    // Forge on Windows rejects verbatim (`\\?\`) output paths even though the
    // canonical path is useful for confinement checks. TempDir's original path
    // names the same directory without that prefix.
    let temp_cli = temp_dir.path().to_path_buf();
    let plan = command_plan(review, &temp_cli, &solc);
    #[cfg(windows)]
    let job = WindowsJob::new().map_err(|e| format!("Cannot create Forge process job: {e}"))?;
    let mut child = into_command(&plan)
        .spawn()
        .map_err(|e| format!("Cannot start local Forge: {e}"))?;
    #[cfg(windows)]
    if let Err(error) = job.assign(&child) {
        return match terminate(&mut child).await {
            Ok(()) => Err(format!(
                "Cannot contain Forge in a Windows process job: {error}"
            )),
            Err(cleanup) => Err(format!(
                "Cannot contain Forge in a Windows process job: {error}; {}",
                process_error(cleanup)
            )),
        };
    }
    let (stdout, stderr) = collect_process(
        &mut child,
        #[cfg(windows)]
        &job,
        &cancel,
        BUILD_TIMEOUT,
        OUTPUT_LIMIT,
    )
    .await
    .map_err(process_error)?;
    #[cfg(windows)]
    drop(job);
    if cancel.is_cancelled() {
        return Err("Foundry build cancelled.".into());
    }
    let after = preflight(&review.workspace)?;
    if after.digest != review.digest || after.settings != review.settings {
        return Err(
            "Workspace changed while building. Discard the output and review again.".into(),
        );
    }
    let path = build_info_file(&temp_cli.join("build-info"))?;
    let bytes = read_bounded(&path, BUILD_INFO_LIMIT)?;
    let build_info: Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("Invalid build-info JSON: {e}"))?;
    if !build_info.is_object() {
        return Err("Build-info must be a JSON object.".into());
    }
    let mut warnings = settings_warnings(&build_info, &review.settings);
    let settings_verified = warnings.is_empty();
    let source_warnings = source_warnings(&build_info, &review.source_manifest, &review.workspace);
    let sources_verified = source_warnings.is_empty();
    warnings.extend(source_warnings);
    let (artifacts, artifact_warnings) = artifact_summaries(&build_info, &review.digest)?;
    warnings.extend(artifact_warnings);
    Ok(FoundryBuildReport {
        workspace: review.workspace.clone(),
        preflight_digest: review.digest.clone(),
        settings: review.settings.clone(),
        artifacts,
        build_info_path: path,
        build_info,
        warnings,
        settings_verified,
        sources_verified,
        stdout,
        stderr,
        temp_dir,
    })
}
