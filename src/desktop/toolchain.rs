//! Compiler and library settings for the project: the solc version (Auto or
//! locked in `.chasm/chasm.toml`) and Solidity libraries installed once per
//! machine under `~/.chasm/lib/<name>@<version>` and remapped per project.
use crate::config::{global_lib_dir, ChasmConfig};
use std::path::{Path, PathBuf};

/// A library Chasm knows how to install.
pub struct Library {
    pub id: &'static str,
    pub title: &'static str,
    pub repo: &'static str,
    /// Remapping prefix used in imports.
    pub prefix: &'static str,
    /// Folder inside the checkout that the prefix points at.
    pub source: &'static str,
    /// Tag to install when the user gives no version.
    pub default_tag: &'static str,
}

pub const LIBRARIES: [Library; 5] = [
    Library { id: "openzeppelin-contracts", title: "OpenZeppelin Contracts", repo: "https://github.com/OpenZeppelin/openzeppelin-contracts", prefix: "@openzeppelin/contracts/", source: "contracts/", default_tag: "v5.1.0" },
    Library { id: "openzeppelin-contracts-upgradeable", title: "OpenZeppelin Upgradeable", repo: "https://github.com/OpenZeppelin/openzeppelin-contracts-upgradeable", prefix: "@openzeppelin/contracts-upgradeable/", source: "contracts/", default_tag: "v5.1.0" },
    Library { id: "forge-std", title: "forge-std", repo: "https://github.com/foundry-rs/forge-std", prefix: "forge-std/", source: "src/", default_tag: "v1.9.4" },
    Library { id: "solmate", title: "Solmate", repo: "https://github.com/transmissions11/solmate", prefix: "solmate/", source: "src/", default_tag: "v7" },
    Library { id: "solady", title: "Solady", repo: "https://github.com/Vectorized/solady", prefix: "solady/", source: "src/", default_tag: "v0.0.281" },
];

pub fn library(id: &str) -> Option<&'static Library> {
    LIBRARIES.iter().find(|library| library.id == id)
}

/// Installed versions of a library, newest first by tag text.
pub fn installed(id: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(global_lib_dir()) else { return Vec::new() };
    let prefix = format!("{id}@");
    let mut versions: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().to_string_lossy().strip_prefix(&prefix).map(str::to_string))
        // A clone still in progress lives in `<tag>.partial`.
        .filter(|tag| !tag.ends_with(".partial"))
        .collect();
    versions.sort_by(|a, b| version_key(b).cmp(&version_key(a)));
    versions
}

fn version_key(tag: &str) -> Vec<u64> {
    tag.trim_start_matches('v').split(|c: char| !c.is_ascii_digit()).filter_map(|part| part.parse().ok()).collect()
}

fn checkout(id: &str, tag: &str) -> PathBuf {
    global_lib_dir().join(format!("{id}@{tag}"))
}

/// Tags are passed to git; allow only plain version text.
fn valid_tag(tag: &str) -> bool {
    !tag.is_empty() && tag.len() <= 40 && !tag.starts_with('-') && tag.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// Shallow clone of one tag into the global library folder. Blocking.
pub fn install(id: &str, tag: &str) -> Result<PathBuf, String> {
    let library = library(id).ok_or("Unknown library.")?;
    let tag = if tag.trim().is_empty() { library.default_tag } else { tag.trim() };
    if !valid_tag(tag) {
        return Err("Enter a release tag such as v5.1.0.".into());
    }
    let target = checkout(id, tag);
    if target.join(library.source).is_dir() {
        return Ok(target);
    }
    std::fs::create_dir_all(global_lib_dir()).map_err(|error| error.to_string())?;
    let partial = target.with_extension("partial");
    let _ = std::fs::remove_dir_all(&partial);
    let mut command = std::process::Command::new("git");
    command.args(["clone", "--depth", "1", "--branch", tag, "--quiet", library.repo]).arg(&partial);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    }
    let output = command.output().map_err(|error| format!("Could not run git ({error}). Install Git to download libraries."))?;
    if !output.status.success() {
        let _ = std::fs::remove_dir_all(&partial);
        let detail = String::from_utf8_lossy(&output.stderr);
        let line = detail.lines().find(|line| !line.trim().is_empty()).unwrap_or("git failed").trim();
        return Err(format!("Could not download {} {tag}: {line}", library.title));
    }
    std::fs::rename(&partial, &target).map_err(|error| error.to_string())?;
    Ok(target)
}

/// Point the project's remapping for the library at an installed version.
pub fn use_in_project(workspace: &Path, id: &str, tag: &str) -> Result<String, String> {
    let library = library(id).ok_or("Unknown library.")?;
    let source = checkout(id, tag).join(library.source);
    if !source.is_dir() {
        return Err(format!("{} {tag} is not installed.", library.title));
    }
    let path = source.to_string_lossy().trim_start_matches(r"\\?\").replace('\\', "/");
    let path = if path.ends_with('/') { path } else { format!("{path}/") };
    let remapping = format!("{}={path}", library.prefix);
    ChasmConfig::update(workspace, |config| {
        config.remappings.retain(|item| !item.starts_with(&format!("{}=", library.prefix)));
        config.remappings.push(remapping.clone());
    })
    .map_err(|error| error.to_string())?;
    Ok(remapping)
}

pub fn remove_from_project(workspace: &Path, id: &str) -> Result<(), String> {
    let library = library(id).ok_or("Unknown library.")?;
    ChasmConfig::update(workspace, |config| config.remappings.retain(|item| !item.starts_with(&format!("{}=", library.prefix))))
        .map_err(|error| error.to_string())
}

/// The version a project remapping points at, if it is a Chasm-managed one.
pub fn used_version(workspace: &Path, id: &str) -> Option<String> {
    let library = library(id)?;
    let config = ChasmConfig::load(workspace).ok()?;
    let remapping = config.remappings.iter().find(|item| item.starts_with(&format!("{}=", library.prefix)))?;
    let marker = format!("{id}@");
    let rest = &remapping[remapping.find(&marker)? + marker.len()..];
    Some(rest.split('/').next()?.to_string())
        .or_else(|| Some("custom path".into()))
}

/// Imports in the workspace sources that no remapping or file resolves, by
/// prefix, so the UI can offer the matching library.
pub fn missing_imports(workspace: &Path) -> Vec<&'static Library> {
    // With a Foundry build the project's own remappings decide what resolves.
    let remappings = match crate::build_config::resolve(workspace) {
        Ok(effective) => effective.remappings,
        Err(_) => ChasmConfig::load(workspace).map(|config| config.remappings).unwrap_or_default(),
    };
    let mut missing = Vec::new();
    for entry in walkdir::WalkDir::new(workspace)
        .max_depth(6)
        .into_iter()
        .filter_entry(|entry| !matches!(entry.file_name().to_str(), Some("lib" | "node_modules" | ".chasm" | "out" | "cache" | ".git")))
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "sol"))
    {
        let Ok(text) = std::fs::read_to_string(entry.path()) else { continue };
        for library in &LIBRARIES {
            let used = text.lines().any(|line| line.trim_start().starts_with("import") && line.contains(&format!("\"{}", library.prefix)));
            let mapped = remappings.iter().any(|item| item.starts_with(library.prefix));
            let vendored = workspace.join("lib").join(library.id).is_dir();
            if used && !mapped && !vendored && !missing.iter().any(|known: &&Library| known.id == library.id) {
                missing.push(library);
            }
        }
    }
    missing
}

/// Installed solc versions, newest first.
pub fn installed_solc() -> Vec<String> {
    let mut versions = foundry_compilers::solc::Solc::installed_versions();
    versions.sort();
    versions.reverse();
    versions.into_iter().map(|version| version.to_string()).collect()
}

/// The locked solc version, or empty for Auto.
pub fn locked_solc(workspace: &Path) -> String {
    ChasmConfig::load(workspace).map(|config| config.compiler.solc).unwrap_or_default()
}

/// The `[compiler]` settings, or defaults when the config cannot be read.
pub fn compiler_settings(workspace: &Path) -> crate::config::CompilerConfig {
    ChasmConfig::load(workspace).map(|config| config.compiler).unwrap_or_default()
}

/// The build source and what it resolves to, for the UI: (chosen, active, origin, settings, error).
pub fn build_source(workspace: &Path) -> (String, String, String, String, String) {
    let chosen = crate::build_config::chosen(workspace).label().to_string();
    match crate::build_config::resolve(workspace) {
        Ok(effective) => (chosen, effective.active.label().into(), effective.origin, effective.compiler.describe(), String::new()),
        Err(error) => (chosen, String::new(), String::new(), String::new(), error),
    }
}

/// Saves optimizer, runs, via-IR, EVM version and metadata hash. Empty text
/// clears a key back to solc's default.
pub fn save_compiler_settings(workspace: &Path, optimizer: Option<bool>, runs: &str, via_ir: Option<bool>, evm_version: &str, bytecode_hash: &str) -> Result<String, String> {
    let runs = runs.trim();
    let runs = if runs.is_empty() { None } else { Some(runs.parse::<usize>().map_err(|_| "Optimizer runs must be a whole number.".to_string())?) };
    let mut next = compiler_settings(workspace);
    next.optimizer = optimizer;
    next.optimizer_runs = runs;
    next.via_ir = via_ir;
    next.evm_version = evm_version.trim().to_ascii_lowercase();
    next.bytecode_hash = bytecode_hash.trim().to_ascii_lowercase();
    next.validate()?;
    let summary = next.describe();
    ChasmConfig::update(workspace, |config| {
        let solc = std::mem::take(&mut config.compiler.solc);
        config.compiler = next.clone();
        config.compiler.solc = solc;
    })
    .map_err(|error| error.to_string())?;
    Ok(summary)
}

pub fn lock_solc(workspace: &Path, version: &str) -> Result<(), String> {
    let version = version.trim();
    if !version.is_empty() {
        semver::Version::parse(version).map_err(|_| "Enter a version like 0.8.24, or leave it empty for Auto.".to_string())?;
    }
    ChasmConfig::update(workspace, |config| config.compiler.solc = version.to_string()).map_err(|error| error.to_string())
}

/// Public RPCs to fork without an account: (chain, URL, chain id). They
/// rate-limit heavy use; a provider URL with a key works the same way.
pub const PUBLIC_RPCS: [(&str, &str, u64); 6] = [
    ("Ethereum", "https://ethereum-rpc.publicnode.com", 1),
    ("Base", "https://base-rpc.publicnode.com", 8453),
    ("Arbitrum", "https://arbitrum-one-rpc.publicnode.com", 42161),
    ("Optimism", "https://optimism-rpc.publicnode.com", 10),
    ("Polygon", "https://polygon-bor-rpc.publicnode.com", 137),
    ("Sepolia", "https://ethereum-sepolia-rpc.publicnode.com", 11155111),
];

/// What Chasm knows about a chain beyond its id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChainInfo {
    /// "Ethereum", "Sepolia", a known L2 name, or "chain {id}".
    pub name: &'static str,
    /// Native token symbol when known (ETH, BNB, POL, AVAX); none means
    /// "native token".
    pub symbol: Option<&'static str>,
    /// "ethereum" for the chains Chasm treats as first-class (Ethereum and
    /// Sepolia); "other" for everything else, which forks with warnings.
    pub kind: &'static str,
}

/// Chain names, native tokens and kind. Only Ethereum mainnet (1) and
/// Sepolia (11155111) are first-class: precompiles, system transactions and
/// fees follow Ethereum there. Other EVM chains fork without special
/// handling and are labelled as such.
pub fn chain_info(id: u64) -> ChainInfo {
    let (name, symbol) = match id {
        1 => ("Ethereum", Some("ETH")),
        11155111 => ("Sepolia", Some("ETH")),
        17000 => ("Holesky", Some("ETH")),
        8453 => ("Base", Some("ETH")),
        84532 => ("Base Sepolia", Some("ETH")),
        10 => ("Optimism", Some("ETH")),
        11155420 => ("Optimism Sepolia", Some("ETH")),
        42161 => ("Arbitrum", Some("ETH")),
        421614 => ("Arbitrum Sepolia", Some("ETH")),
        56 => ("BNB Chain", Some("BNB")),
        137 => ("Polygon", Some("POL")),
        43114 => ("Avalanche", Some("AVAX")),
        other => (PUBLIC_RPCS.iter().find(|(_, _, chain)| *chain == other).map(|(name, _, _)| *name).unwrap_or("chain"), None),
    };
    let kind = if id == 1 || id == 11155111 { "ethereum" } else { "other" };
    ChainInfo { name, symbol, kind }
}

/// A readable name for a chain id, e.g. "Ethereum (1)".
pub fn chain_name(id: u64) -> String {
    let info = chain_info(id);
    match info.name {
        "chain" => format!("chain {id}"),
        name => format!("{name} ({id})"),
    }
}

/// The native token of a chain: its symbol when known, else a neutral label.
pub fn native_token(id: Option<u64>) -> &'static str {
    id.and_then(|id| chain_info(id).symbol).unwrap_or("native token")
}

/// Whether a chain is first-class for forking and tracing.
pub fn chains_first_class(id: u64) -> bool {
    chain_info(id).kind == "ethereum"
}

/// Rough hardfork rank of a block header, so a build can be compared with the
/// network it will run on. Ranks: Paris 0, Shanghai 1, Cancun 2, Prague 3.
pub fn header_hardfork(header: &serde_json::Value) -> (&'static str, u8) {
    if header.get("requestsHash").is_some_and(|value| !value.is_null()) {
        ("Prague", 3)
    } else if header.get("blobGasUsed").is_some_and(|value| !value.is_null()) {
        ("Cancun", 2)
    } else if header.get("withdrawalsRoot").is_some_and(|value| !value.is_null()) {
        ("Shanghai", 1)
    } else {
        ("Paris", 0)
    }
}

/// Rank of a solc evm_version name, for the same comparison.
pub fn evm_version_rank(version: &str) -> Option<u8> {
    match version.trim().to_ascii_lowercase().as_str() {
        "" => None,
        "paris" | "merge" => Some(0),
        "shanghai" => Some(1),
        "cancun" => Some(2),
        "prague" | "osaka" => Some(3),
        _ => None,
    }
}

/// One line per check of a fork RPC, before it is saved. Only the host is
/// named: provider keys often sit in the URL path.
pub async fn test_rpc(url: &str, block: &str, workspace_evm: Option<&str>) -> Result<Vec<String>, String> {
    let url = url.trim();
    let parsed = reqwest::Url::parse(url).map_err(|_| "Enter an http(s) RPC URL.".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err("The fork RPC must be an http(s) URL.".into());
    }
    let block = block.trim();
    let fork_block = if block.is_empty() { None } else { Some(block.parse::<u64>().map_err(|_| "Enter the fork block as a whole number.".to_string())?) };
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(15)).build().map_err(|error| error.to_string())?;
    let call = |method: &'static str, params: serde_json::Value| {
        let client = client.clone();
        let url = url.to_string();
        async move {
            let body = serde_json::json!({"jsonrpc":"2.0","id":1,"method":method,"params":params});
            let response = super::retry::send(|| client.post(&url).json(&body)).await.map_err(|_| "no answer".to_string())?;
            if response.status().as_u16() == 429 {
                return Err("rate limited (HTTP 429) even after retries; use a provider URL with a key".to_string());
            }
            let value: serde_json::Value = response.json().await.map_err(|_| "not JSON-RPC".to_string())?;
            match value.get("error").filter(|error| !error.is_null()) {
                Some(error) => Err(format!("refused (code {})", error["code"].as_i64().unwrap_or_default())),
                None => Ok(value["result"].clone()),
            }
        }
    };
    let host = parsed.host_str().unwrap_or("the RPC");
    let quantity = |value: &serde_json::Value| value.as_str().and_then(|text| u64::from_str_radix(text.trim_start_matches("0x"), 16).ok());
    let mut lines = Vec::new();
    let chain = call("eth_chainId", serde_json::json!([])).await;
    let Some(chain) = chain.as_ref().ok().and_then(quantity) else {
        return Err(format!("{host} did not answer eth_chainId ({}). Check the URL.", chain.err().unwrap_or_else(|| "invalid answer".into())));
    };
    let info = chain_info(chain);
    lines.push(format!("ok  chain: {}", chain_name(chain)));
    if info.kind == "ethereum" {
        lines.push("ok  chain kind: Ethereum — native token, precompiles and fees follow Ethereum".into());
    } else {
        lines.push(format!(
            "--  chain kind: other chain — precompiles, system transactions and fees may differ from Ethereum; Chasm forks it without special handling (native token: {})",
            info.symbol.unwrap_or("reported by the chain")
        ));
    }
    let head = call("eth_blockNumber", serde_json::json!([])).await.ok().as_ref().and_then(quantity);
    match head {
        Some(head) => lines.push(format!("ok  head: block {head}")),
        None => lines.push("no  head: eth_blockNumber failed".into()),
    }
    // Capabilities the fork and the debugger depend on.
    match call("eth_getBlockByNumber", serde_json::json!(["latest", false])).await {
        Ok(header) if !header.is_null() => {
            let (fork, rank) = header_hardfork(&header);
            lines.push(format!("ok  latest hardfork: {fork}"));
            if let Some(rank_workspace) = workspace_evm.and_then(evm_version_rank) {
                if rank_workspace > rank {
                    lines.push(format!(
                        "no  the workspace builds for {} but the network's latest block is {fork}: deployed code may use opcodes this chain does not have",
                        workspace_evm.unwrap_or_default()
                    ));
                }
            }
        }
        _ => lines.push("--  hardfork: the latest block header is unavailable".into()),
    }
    match debug_trace_supported(url).await {
        Some(true) => lines.push("ok  debug_traceTransaction: supported".into()),
        Some(false) => lines.push("--  debug_traceTransaction: not supported; replaying a forked transaction needs it".into()),
        None => lines.push("--  debug_traceTransaction: not checked".into()),
    }
    if let Some(number) = fork_block {
        let found = call("eth_getBlockByNumber", serde_json::json!([format!("{number:#x}"), false])).await;
        match found {
            Ok(block) if !block.is_null() => lines.push(format!("ok  fork block {number} is available")),
            Ok(_) => lines.push(format!("no  fork block {number}: not served (pruned, or after the head)")),
            Err(error) => lines.push(format!("no  fork block {number}: {error}")),
        }
    }
    match call("eth_getBlockByNumber", serde_json::json!(["0x0", false])).await {
        Ok(genesis) if !genesis.is_null() => lines.push("ok  block 0 is served: identity pins to the genesis hash".into()),
        _ => lines.push("--  block 0 is not served: Chasm pins to the fork block instead".into()),
    }
    // Historical state at the fork block is what an archive node provides.
    let at = fork_block.map(|number| format!("{number:#x}")).unwrap_or_else(|| "latest".into());
    match call("eth_getBalance", serde_json::json!(["0x0000000000000000000000000000000000000000", at])).await {
        Ok(_) => lines.push("ok  state at the fork block is readable".into()),
        Err(error) => lines.push(format!("no  state at the fork block: {error} (an archive RPC is needed for old blocks)")),
    }
    Ok(lines)
}

/// Whether the RPC answers `debug_traceTransaction` at all: a method-not-found
/// error (-32601) means no, any other answer (including "unknown hash") yes.
async fn debug_trace_supported(url: &str) -> Option<bool> {
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(10)).build().ok()?;
    let body = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"debug_traceTransaction","params":["0x0000000000000000000000000000000000000000000000000000000000000000", {}]});
    let value: serde_json::Value = super::retry::send(|| client.post(url).json(&body)).await.ok()?.json().await.ok()?;
    let error = &value["error"];
    let code = error["code"].as_i64();
    let message = error["message"].as_str().unwrap_or_default().to_ascii_lowercase();
    let missing = code == Some(-32601)
        || message.contains("method not found")
        || message.contains("does not exist")
        || message.contains("is not available")
        || message.contains("not supported");
    Some(!missing)
}

/// The saved profiles, the active id, and the network the project boots with.
/// A config that cannot be read resolves to the local chain: boot refuses it,
/// this only renders the view.
pub fn load_network(workspace: &Path) -> (Vec<crate::config::NetworkProfile>, String, crate::config::NetworkSettings) {
    match ChasmConfig::load(workspace) {
        Ok(config) => (config.networks.clone(), config.network.active.clone(), config.resolve_network().unwrap_or_default()),
        Err(_) => (Vec::new(), String::new(), crate::config::NetworkSettings::default()),
    }
}

/// Parse a profile from API JSON. An omitted `chain_id_mode` keeps the mode
/// of the saved profile with that id (Local for a new profile), so a client
/// that does not know the field cannot silently downgrade a Source profile.
pub fn profile_from_json(workspace: &Path, value: &serde_json::Value) -> Result<crate::config::NetworkProfile, String> {
    let mut profile: crate::config::NetworkProfile = serde_json::from_value(value.clone())
        .map_err(|error| format!("Invalid profile: {error}"))?;
    let explicit = value.get("chain_id_mode").is_some_and(|mode| !mode.is_null());
    if !explicit {
        if let Ok(config) = ChasmConfig::load(workspace) {
            if let Some(saved) = config.profile(&profile.id) {
                profile.chain_id_mode = saved.chain_id_mode;
            }
        }
    }
    Ok(profile)
}

/// Validate and save one profile, inserting or replacing by id.
pub fn save_profile(workspace: &Path, profile: crate::config::NetworkProfile) -> Result<crate::config::NetworkProfile, String> {
    profile.validate()?;
    ChasmConfig::update(workspace, |config| config.upsert_profile(profile.clone())).map_err(|error| error.to_string())?;
    Ok(profile)
}

/// Remove a saved profile. `local` and the active profile are refused.
pub fn remove_profile(workspace: &Path, id: &str) -> Result<(), String> {
    let mut config = ChasmConfig::load(workspace).map_err(|error| error.to_string())?;
    config.remove_profile(id)?;
    config.save(workspace).map_err(|error| error.to_string())
}

/// A proposed switch. Neither a new active id nor an edited active profile
/// goes to disk until the tested settings can actually be used.
pub struct NetworkSwitch {
    previous_active: String,
    previous_target: Option<crate::config::NetworkProfile>,
    edit: Option<crate::config::NetworkProfile>,
    pub settings: crate::config::NetworkSettings,
}

impl NetworkSwitch {
    pub fn prepare(workspace: &Path, id: &str, edit: Option<crate::config::NetworkProfile>) -> Result<Self, String> {
        let config = ChasmConfig::load(workspace).map_err(|_| "Could not read the project configuration.".to_string())?;
        Self::from_config(config, id, edit)
    }

    fn from_config(mut config: ChasmConfig, id: &str, edit: Option<crate::config::NetworkProfile>) -> Result<Self, String> {
        let id = id.trim();
        let previous_target = config.profile(id).cloned();
        let previous_active = config.network.active.clone();
        if let Some(profile) = &edit {
            profile.validate()?;
            if profile.id != id { return Err("The edited profile does not match the switch target.".into()); }
            config.upsert_profile(profile.clone());
        } else if let Some(profile) = &previous_target {
            profile.validate()?;
        }
        config.network.active = id.to_string();
        let settings = config.resolve_network()?;
        Ok(Self { previous_active, previous_target, edit, settings })
    }

    /// Keep unrelated edits made during the RPC test. Refuse a changed target
    /// (including a newly resolved environment URL) rather than activating it.
    pub fn checked_config(&self, workspace: &Path) -> Result<ChasmConfig, String> {
        let mut config = ChasmConfig::load(workspace).map_err(|_| "Could not read the project configuration.".to_string())?;
        let id = &self.settings.profile_id;
        if config.network.active != self.previous_active || config.profile(id) != self.previous_target.as_ref() {
            return Err("Network configuration changed. Review the profile and switch again.".into());
        }
        if let Some(profile) = &self.edit { config.upsert_profile(profile.clone()); }
        config.network.active = id.clone();
        if config.resolve_network()? != self.settings {
            return Err("The target RPC changed. Test the profile again.".into());
        }
        Ok(config)
    }
}

/// The legacy single-network save behind `toolchain.action network` and the
/// Network form's Save button: it edits the active fork profile's URL and
/// block (creating `fork-1` when the local chain is active), or switches back
/// to a local chain on an empty URL. The saved profile's chain id mode is
/// kept — editing the URL must not silently downgrade a Source profile back
/// to Local; `network.save` sets the mode explicitly.
pub fn prepare_network(workspace: &Path, url: &str, block: &str) -> Result<NetworkSwitch, String> {
    let url = url.trim();
    let block = block.trim();
    let fork_block = if block.is_empty() {
        None
    } else {
        Some(block.parse::<u64>().map_err(|_| "Enter the fork block as a whole number, or leave it empty for the latest block.".to_string())?)
    };
    if url.is_empty() { return NetworkSwitch::prepare(workspace, "", None); }
    let config = ChasmConfig::load(workspace).map_err(|_| "Could not read the project configuration.".to_string())?;
    let id = match config.network.active.trim() {
        "" | crate::config::NetworkProfile::LOCAL_ID => "fork-1",
        active => active,
    }.to_string();
    let name = config.profile(&id).map(|profile| profile.name.clone()).unwrap_or_default();
    let chain_id_mode = config.profile(&id).map(|profile| profile.chain_id_mode).unwrap_or(crate::config::ChainIdMode::Local);
    let profile = crate::config::NetworkProfile { id: id.clone(), name, fork_url: url.into(), fork_url_env: String::new(), fork_block, chain_id_mode };
    NetworkSwitch::from_config(config, &id, Some(profile))
}

/// The `pragma solidity` line of each workspace source, for the compiler view.
pub fn pragmas(workspace: &Path) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    for entry in walkdir::WalkDir::new(workspace)
        .max_depth(6)
        .into_iter()
        .filter_entry(|entry| !matches!(entry.file_name().to_str(), Some("lib" | "node_modules" | ".chasm" | "out" | "cache" | ".git")))
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "sol"))
        .take(200)
    {
        let Ok(text) = std::fs::read_to_string(entry.path()) else { continue };
        let pragma = text
            .lines()
            .map(str::trim)
            .find(|line| line.starts_with("pragma solidity"))
            .map(|line| line.trim_start_matches("pragma solidity").trim().trim_end_matches(';').trim().to_string())
            .unwrap_or_else(|| "no pragma".into());
        let name = entry.path().strip_prefix(workspace).unwrap_or(entry.path()).to_string_lossy().replace('\\', "/");
        rows.push((name, pragma));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_are_plain_and_versions_sort_numerically() {
        assert!(valid_tag("v5.1.0"));
        assert!(!valid_tag("--upload-pack=evil"));
        assert!(!valid_tag("v1;rm"));
        let mut tags = vec!["v4.9.6".to_string(), "v10.0.0".to_string(), "v5.1.0".to_string()];
        tags.sort_by(|a, b| version_key(b).cmp(&version_key(a)));
        assert_eq!(tags, ["v10.0.0", "v5.1.0", "v4.9.6"]);
    }

    #[test]
    fn project_settings_lock_solc_and_report_missing_imports() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("A.sol"), "pragma solidity ^0.8.20;\nimport \"@openzeppelin/contracts/token/ERC20/ERC20.sol\";\nimport \"solady/utils/LibString.sol\";\n").unwrap();
        assert_eq!(pragmas(workspace.path()), vec![("A.sol".to_string(), "^0.8.20".to_string())]);
        let missing: Vec<_> = missing_imports(workspace.path()).iter().map(|library| library.id).collect();
        assert_eq!(missing, vec!["openzeppelin-contracts", "solady"]);
        assert!(lock_solc(workspace.path(), "0.8.x").is_err());
        lock_solc(workspace.path(), "0.8.24").unwrap();
        assert_eq!(locked_solc(workspace.path()), "0.8.24");
        let saved = std::fs::read_to_string(workspace.path().join(".chasm/chasm.toml")).unwrap();
        assert!(saved.contains("[compiler]") && saved.contains("solc = \"0.8.24\""), "{saved}");
        lock_solc(workspace.path(), "").unwrap();
        assert!(!std::fs::read_to_string(workspace.path().join(".chasm/chasm.toml")).unwrap().contains("[compiler]"));
        ChasmConfig::update(workspace.path(), |config| config.remappings.push("solady/=/x/solady@v0.0.1/src/".into())).unwrap();
        assert_eq!(used_version(workspace.path(), "solady").as_deref(), Some("v0.0.1"));
        let missing: Vec<_> = missing_imports(workspace.path()).iter().map(|library| library.id).collect();
        assert_eq!(missing, vec!["openzeppelin-contracts"]);
        remove_from_project(workspace.path(), "solady").unwrap();
        assert_eq!(used_version(workspace.path(), "solady"), None);
        assert!(prepare_network(workspace.path(), "ftp://x", "").is_err());
        assert!(prepare_network(workspace.path(), "https://rpc.example", "latest").is_err());
        let plan = prepare_network(workspace.path(), "https://rpc.example/key", "21000000").unwrap();
        assert_eq!(plan.settings.fork_block, Some(21_000_000));
        assert_eq!(plan.settings.profile_id, "fork-1");
        assert!(ChasmConfig::load(workspace.path()).unwrap().profile("fork-1").is_none());
        plan.checked_config(workspace.path()).unwrap().save(workspace.path()).unwrap();
        let config = ChasmConfig::load(workspace.path()).unwrap();
        assert_eq!(config.network.active, "fork-1");
        assert_eq!(config.profile("fork-1").unwrap().fork_url, "https://rpc.example/key");
        // The legacy alias switches back to the built-in local chain; the
        // saved profile stays listed for reuse.
        let plan = prepare_network(workspace.path(), "", "").unwrap();
        assert!(plan.settings.is_local() && plan.settings.profile_id.is_empty());
        plan.checked_config(workspace.path()).unwrap().save(workspace.path()).unwrap();
        let saved = std::fs::read_to_string(workspace.path().join(".chasm/chasm.toml")).unwrap();
        assert!(!saved.contains("active"), "{saved}");
        assert!(saved.contains("[[networks]]"), "{saved}");
        // An explicit switch picks any saved profile.
        let switch = |id| NetworkSwitch::prepare(workspace.path(), id, None).and_then(|plan| plan.checked_config(workspace.path()));
        switch("fork-1").unwrap().save(workspace.path()).unwrap();
        assert_eq!(ChasmConfig::load(workspace.path()).unwrap().network.active, "fork-1");
        assert!(switch("ghost").is_err());
        // Profiles save, replace and remove by id; `local` never does.
        let profile = crate::config::NetworkProfile { id: "mainnet".into(), name: "Ethereum mainnet".into(), fork_url: "https://m.example".into(), fork_url_env: String::new(), fork_block: Some(1), chain_id_mode: crate::config::ChainIdMode::Source };
        assert_eq!(save_profile(workspace.path(), profile.clone()).unwrap(), profile);
        assert!(save_profile(workspace.path(), crate::config::NetworkProfile { id: "Local".into(), name: String::new(), fork_url: String::new(), fork_url_env: String::new(), fork_block: None, chain_id_mode: crate::config::ChainIdMode::Local }).is_err());
        assert!(remove_profile(workspace.path(), "fork-1").is_err(), "the active profile is refused");
        switch("").unwrap().save(workspace.path()).unwrap();
        remove_profile(workspace.path(), "fork-1").unwrap();
        assert!(ChasmConfig::load(workspace.path()).unwrap().profile("fork-1").is_none());
    }

    #[test]
    fn staged_active_edit_preserves_config_and_rejects_a_changed_target() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let first = prepare_network(root, "https://original.invalid", "1").unwrap();
        first.checked_config(root).unwrap().save(root).unwrap();
        let path = root.join(".chasm/chasm.toml");
        let before = std::fs::read(&path).unwrap();
        let staged = prepare_network(root, "https://proposed.invalid", "2").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), before);
        ChasmConfig::update(root, |config| config.remappings.push("a/=b/".into())).unwrap();
        assert_eq!(staged.checked_config(root).unwrap().remappings, ["a/=b/"]);
        ChasmConfig::update(root, |config| config.networks[0].fork_block = Some(3)).unwrap();
        assert!(staged.checked_config(root).is_err());
        let config = ChasmConfig::load(root).unwrap();
        assert_eq!(config.resolve_network().unwrap().fork_url, "https://original.invalid");
        assert_eq!(config.resolve_network().unwrap().fork_block, Some(3));
    }

    #[test]
    fn the_legacy_alias_keeps_the_saved_chain_id_mode() {
        let root = tempfile::tempdir().unwrap();
        let source = crate::config::NetworkProfile {
            id: "mainnet".into(), name: String::new(), fork_url: "https://old.example".into(),
            fork_url_env: String::new(), fork_block: None, chain_id_mode: crate::config::ChainIdMode::Source,
        };
        save_profile(root.path(), source).unwrap();
        ChasmConfig::update(root.path(), |config| config.network.active = "mainnet".into()).unwrap();
        let switched = prepare_network(root.path(), "https://new.example", "").unwrap();
        assert_eq!(switched.settings.chain_id_mode, crate::config::ChainIdMode::Source, "editing the URL must not downgrade a Source profile");
        let committed = switched.checked_config(root.path()).unwrap();
        assert_eq!(committed.profile("mainnet").unwrap().chain_id_mode, crate::config::ChainIdMode::Source);
        // A brand-new fork-1 profile stays Local, as before profiles existed.
        let fresh = prepare_network(tempfile::tempdir().unwrap().path(), "https://rpc.example", "").unwrap();
        assert_eq!(fresh.settings.chain_id_mode, crate::config::ChainIdMode::Local);
    }

    #[test]
    fn an_omitted_chain_id_mode_keeps_the_saved_mode() {
        let root = tempfile::tempdir().unwrap();
        let source = crate::config::NetworkProfile {
            id: "mainnet".into(), name: String::new(), fork_url: "https://rpc.example".into(),
            fork_url_env: String::new(), fork_block: None, chain_id_mode: crate::config::ChainIdMode::Source,
        };
        save_profile(root.path(), source).unwrap();
        let parsed = profile_from_json(root.path(), &serde_json::json!({"id": "mainnet", "fork_url": "https://rpc.example"})).unwrap();
        assert_eq!(parsed.chain_id_mode, crate::config::ChainIdMode::Source, "a client that omits the field cannot downgrade a Source profile");
        let explicit = profile_from_json(root.path(), &serde_json::json!({"id": "mainnet", "fork_url": "https://rpc.example", "chain_id_mode": "local"})).unwrap();
        assert_eq!(explicit.chain_id_mode, crate::config::ChainIdMode::Local, "an explicit mode is honoured");
        let fresh = profile_from_json(root.path(), &serde_json::json!({"id": "new", "fork_url": "https://rpc.example"})).unwrap();
        assert_eq!(fresh.chain_id_mode, crate::config::ChainIdMode::Local, "a new profile defaults to Local");
        assert!(profile_from_json(root.path(), &serde_json::json!({"id": "Bad Id"})).is_ok(), "validation happens after parsing");
        assert!(profile_from_json(root.path(), &serde_json::json!({"id": "x", "unknown": 1})).is_err(), "unknown keys are refused");
    }

    #[test]
    fn public_rpcs_are_https_and_chains_are_named() {
        for (_, url, _) in PUBLIC_RPCS {
            assert!(prepare_network(tempfile::tempdir().unwrap().path(), url, "").is_ok(), "{url}");
            assert!(url.starts_with("https://"));
        }
        assert_eq!(chain_name(1), "Ethereum (1)");
        assert_eq!(chain_name(8453), "Base (8453)");
        assert_eq!(chain_name(56), "BNB Chain (56)");
        assert_eq!(chain_name(999_999), "chain 999999");
    }

    #[test]
    fn only_ethereum_and_sepolia_are_first_class_and_tokens_are_named() {
        assert_eq!(chain_info(1).kind, "ethereum");
        assert_eq!(chain_info(11155111).name, "Sepolia");
        assert_eq!(chain_info(11155111).kind, "ethereum");
        assert_eq!(chain_info(8453).kind, "other", "L2s fork with warnings, not special handling");
        assert!(chains_first_class(1) && chains_first_class(11155111));
        assert!(!chains_first_class(8453) && !chains_first_class(999_999));
        assert_eq!(native_token(Some(1)), "ETH");
        assert_eq!(native_token(Some(11155111)), "ETH");
        assert_eq!(native_token(Some(137)), "POL");
        assert_eq!(native_token(Some(43114)), "AVAX");
        assert_eq!(native_token(Some(999_999)), "native token");
        assert_eq!(native_token(None), "native token");
    }

    #[test]
    fn a_block_header_names_a_hardfork_and_ranks_evm_versions() {
        let json = |fields: serde_json::Value| fields;
        assert_eq!(header_hardfork(&json(serde_json::json!({"number":"0x1"}))).0, "Paris");
        assert_eq!(header_hardfork(&json(serde_json::json!({"withdrawalsRoot":"0x00"}))).0, "Shanghai");
        assert_eq!(header_hardfork(&json(serde_json::json!({"blobGasUsed":"0x0","withdrawalsRoot":"0x00"}))).0, "Cancun");
        assert_eq!(header_hardfork(&json(serde_json::json!({"requestsHash":"0x00","blobGasUsed":"0x0"}))).0, "Prague");
        assert_eq!(header_hardfork(&json(serde_json::json!({"blobGasUsed":null,"withdrawalsRoot":"0x00"}))).0, "Shanghai", "a null field is not a fork marker");
        assert_eq!(evm_version_rank("cancun"), Some(2));
        assert!(evm_version_rank("cancun").unwrap() > header_hardfork(&json(serde_json::json!({"withdrawalsRoot":"0x00"}))).1, "a Cancun build on a Shanghai chain is a warning");
        assert!(!(evm_version_rank("shanghai").unwrap() > header_hardfork(&json(serde_json::json!({"blobGasUsed":"0x0"}))).1));
        assert_eq!(evm_version_rank(""), None);
        assert_eq!(evm_version_rank("not-a-fork"), None);
    }
}
