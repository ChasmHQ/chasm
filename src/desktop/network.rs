//! Network profiles at runtime: the identity the node actually runs, the
//! source chain id read, and the marker that lets a failed network switch
//! fall back to the previous profile.
use crate::config::NetworkSettings;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// What the running node forks, pinned at boot. Its `key()` tags saved data
/// with its network from N2 on.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NetworkIdentity {
    /// Active profile id, empty for the built-in local chain.
    pub profile_id: String,
    /// Chain id of the forked network; none on a local chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_chain_id: Option<u64>,
    /// The block actually forked (a `latest` fork pins the boot block).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork_block: Option<u64>,
    /// Hash that anchors the fork: the fork block's hash, when the node
    /// reports one. An earlier saved state with a different anchor is from
    /// another point of the chain and is rejected when restored (N3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_hash: Option<String>,
    /// The chain id the workspace node reports; 31337 unless the profile
    /// runs its source chain id.
    pub execution_chain_id: u64,
}

impl NetworkIdentity {
    /// Stable tag for saved data: `profile:chain:block`. The chain part is
    /// the source chain id, or 0 for a local chain; the block part is the
    /// forked block, or 0 when unknown.
    pub fn key(&self) -> String {
        let chain = self.source_chain_id.map(|id| id.to_string()).unwrap_or_else(|| "0".into());
        let block = self.fork_block.map(|block| block.to_string()).unwrap_or_else(|| "0".into());
        let profile = if self.profile_id.is_empty() { "local" } else { &self.profile_id };
        format!("{profile}:{chain}:{block}")
    }

    /// The compact tag saved inside data files.
    pub fn tag(&self) -> NetworkTag {
        let session = self.anchor_hash.as_deref().filter(|hash| self.source_chain_id.is_none() && self.fork_block.is_none() && valid_hash(hash)).map(|hash| hash[2..10].to_ascii_lowercase());
        NetworkTag { profile: self.profile_id.clone(), chain: self.source_chain_id, fork_block: self.fork_block, session }
    }
}

/// The network an artifact was made on, saved inside history entries, trace
/// files and macro definitions. Untagged data predates profiles.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NetworkTag {
    /// Active profile id, empty for the built-in local chain.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub profile: String,
    /// Source chain id; none on a local chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain: Option<u64>,
    /// The block actually forked when it was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork_block: Option<u64>,
    /// First hex digits of the anchor hash. A local chain's genesis changes
    /// on every start, so local transaction hashes that repeat across
    /// restarts still get distinct trace files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

impl NetworkTag {
    /// Parse back a tag from the `profile:chain:block` key that `key()` and
    /// saved entries use.
    pub fn from_key(key: &str) -> Option<Self> {
        let mut parts = key.split(':');
        let profile = parts.next()?.to_string();
        let chain = parts.next()?.parse::<u64>().ok();
        let fork_block = parts.next()?.parse::<u64>().ok();
        Some(Self { profile: if profile == "local" { String::new() } else { profile }, chain: chain.filter(|chain| *chain != 0), fork_block: fork_block.filter(|block| *block != 0), session: None })
    }

    /// Whether this tag names a different network than the one running.
    /// Untagged data is old, not foreign.
    pub fn is_foreign(&self, current: Option<&NetworkIdentity>) -> bool {
        match current {
            None => false,
            Some(identity) => self.profile != identity.profile_id || self.chain != identity.source_chain_id,
        }
    }

    /// File-name-safe prefix: `local` or `mainnet-21000000`.
    pub fn slug(&self) -> String {
        let profile = if self.profile.is_empty() { "local".to_string() } else { self.profile.clone() };
        let base = match self.fork_block {
            Some(block) => format!("{profile}-{block}"),
            None => profile,
        };
        match &self.session {
            Some(session) => format!("{base}-{session}"),
            None => base,
        }
    }

    /// One line for lists: the chain, else "local chain".
    pub fn describe(&self) -> String {
        match self.chain {
            Some(chain) => {
                let block = self.fork_block.map(|block| format!(" @ {block}")).unwrap_or_default();
                let profile = if self.profile.is_empty() { String::new() } else { format!("{} · ", self.profile) };
                format!("{profile}{}{block}", super::toolchain::chain_name(chain))
            }
            None => "the local chain".into(),
        }
    }
}

/// The signer choice of a saved profile: never a key, never a keystore.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SignerChoice {
    DevAccount { index: u8 },
    Impersonate { address: String },
}

/// The state saved for one profile while its node ran, under
/// `.chasm/networks/<id>/`. Switching away writes it; switching back with
/// `resume` restores what the saved identity matches.
#[derive(Debug)]
pub struct SavedProfile {
    pub identity: NetworkIdentity,
    pub state: String,
    /// Deployments of that network: (contract name, address).
    pub deployments: Vec<(String, String)>,
    /// Bound tabs at save time: (contract name, address).
    pub tabs: Vec<(String, String)>,
    pub signer: Option<SignerChoice>,
}

/// What boot does with a profile's saved state.
#[derive(Debug)]
pub enum LoadOutcome {
    /// Nothing was saved for this profile.
    None,
    Restored { saved: SavedProfile, moved_blocks: bool },
    /// The saved state belongs to another chain point and was archived.
    Rejected { reason: String },
    /// Nothing was restored or moved; the caller must not overwrite the saved files.
    Deferred { reason: String },
}

const STATE_FORMAT: &str = "chasm-profile/2";
const LEGACY_FORMAT: &str = "chasm-profile/1";

fn profile_name(id: &str) -> Result<&str, String> {
    if id.is_empty() { return Ok("local"); }
    if id.len() > 40 || !id.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'-' | b'_'))
        || matches!(id, "con" | "prn" | "aux" | "nul")
        || id.strip_prefix("com").or_else(|| id.strip_prefix("lpt")).is_some_and(|n| matches!(n, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")) {
        return Err("Invalid network profile folder name.".into());
    }
    Ok(id)
}

fn checked_entry(path: &Path, directory: bool) -> Result<bool, String> {
    let meta = match path.symlink_metadata() {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.to_string()),
    };
    #[cfg(windows)]
    let linked = std::os::windows::fs::MetadataExt::file_attributes(&meta)
        & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT != 0;
    #[cfg(not(windows))]
    let linked = meta.file_type().is_symlink();
    if linked || (directory && !meta.is_dir()) || (!directory && !meta.is_file()) {
        return Err(format!("Unexpected file type or link at {}.", path.display()));
    }
    Ok(true)
}

pub fn profile_dir(workspace: &Path, id: &str) -> Result<PathBuf, String> {
    checked_profile_dir(workspace, id, false)
}

fn checked_profile_dir(workspace: &Path, id: &str, create: bool) -> Result<PathBuf, String> {
    let name = profile_name(id)?;
    if !workspace.is_absolute() || workspace.components().any(|p| p == std::path::Component::ParentDir) {
        return Err("Invalid workspace folder.".into());
    }
    for ancestor in workspace.ancestors().collect::<Vec<_>>().into_iter().rev() {
        if !checked_entry(ancestor, true)? { return Err("Missing workspace folder.".into()); }
    }
    let mut dir = workspace.to_path_buf();
    for child in [".chasm", "networks", name] {
        dir.push(child);
        if !checked_entry(&dir, true)? && create {
            std::fs::create_dir(&dir).map_err(|error| error.to_string())?;
            checked_entry(&dir, true)?;
        }
    }
    Ok(dir)
}

/// Read errors count as saved work when deciding whether a clean fork needs confirmation.
pub fn has_saved_state(workspace: &Path, id: &str) -> bool {
    (|| -> Result<bool, String> {
        let dir = profile_dir(workspace, id)?;
        for name in ["state.json", "deployments.json", "signer.json"] {
            if checked_entry(&dir.join(name), false)? { return Ok(true); }
        }
        Ok(false)
    })().unwrap_or(true)
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SavedStateFile {
    format: String,
    identity: NetworkIdentity,
    state: String,
    metadata: Option<CheckpointMetadata>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CheckpointMetadata {
    deployments: Vec<(String, String)>,
    tabs: Vec<(String, String)>,
    signer: Option<SignerChoice>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SavedDeploymentsFile {
    format: String,
    identity: NetworkIdentity,
    deployments: Vec<(String, String)>,
    tabs: Vec<(String, String)>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SavedSignerFile {
    format: String,
    identity: NetworkIdentity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    signer: Option<SignerChoice>,
}

fn valid_hash(hash: &str) -> bool {
    hash.len() == 66 && hash.strip_prefix("0x").is_some_and(|hex| hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn valid_dump(dump: &str) -> bool {
    dump.strip_prefix("0x").is_some_and(|hex| !hex.is_empty() && hex.len() % 2 == 0 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

pub fn identity_complete(identity: &NetworkIdentity, is_fork: bool) -> bool {
    if is_fork {
        identity.source_chain_id.is_some() && identity.fork_block.is_some() && identity.anchor_hash.as_deref().is_some_and(valid_hash)
    } else {
        identity.source_chain_id.is_none() && identity.fork_block.is_none() && identity.anchor_hash.as_deref().map_or(true, valid_hash)
    }
}

/// Move a profile's saved folder aside as `<id>.<label>-<timestamp>` and
/// return its name. Profile ids cannot contain `.`, so an archive never
/// collides with a profile.
pub fn archive_profile_state(workspace: &Path, id: &str, label: &str) -> Result<String, String> {
    let dir = profile_dir(workspace, id)?;
    if !matches!(label, "rejected" | "discarded") { return Err("Invalid archive label.".into()); }
    if !checked_entry(&dir, true)? { return Err("nothing to archive".into()); }
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
    let name = format!("{}.{label}-{stamp}", profile_name(id)?);
    let target = dir.with_file_name(&name);
    if checked_entry(&target, true)? { return Err("Archive destination already exists.".into()); }
    rename_without_replacing(&dir, &target).map_err(|error| error.to_string())?;
    Ok(name)
}

pub fn archive_for_fresh(workspace: &Path, id: &str) -> Result<Option<String>, String> {
    let dir = profile_dir(workspace, id)?;
    if !checked_entry(&dir, true)? { return Ok(None); }
    archive_profile_state(workspace, id, "discarded").map(Some)
}

pub fn restore_archived_profile(workspace: &Path, id: &str, archive: &str) -> Result<(), String> {
    let dir = profile_dir(workspace, id)?;
    let prefix = format!("{}.discarded-", profile_name(id)?);
    if !archive.strip_prefix(prefix.as_str()).is_some_and(|stamp| !stamp.is_empty() && stamp.len() <= 39 && stamp.bytes().all(|b| b.is_ascii_digit())) {
        return Err("Invalid archive name for this profile.".into());
    }
    let source = dir.with_file_name(archive);
    if !checked_entry(&source, true)? { return Err("The profile archive is missing.".into()); }
    if checked_entry(&dir, true)? { return Err("The profile folder already exists.".into()); }
    rename_without_replacing(&source, &dir).map_err(|error| error.to_string())
}

fn rename_without_replacing(source: &Path, target: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let wide = |p: &Path| p.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<_>>();
        let (source, target) = (wide(source), wide(target));
        // MoveFileW fails if the destination already exists, even if it is an empty folder.
        let ok = unsafe { windows_sys::Win32::Storage::FileSystem::MoveFileW(source.as_ptr(), target.as_ptr()) };
        if ok != 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::unix::ffi::OsStrExt;
        let source = std::ffi::CString::new(source.as_os_str().as_bytes())?;
        let target = std::ffi::CString::new(target.as_os_str().as_bytes())?;
        #[cfg(target_os = "linux")]
        let status = unsafe { libc::syscall(libc::SYS_renameat2, libc::AT_FDCWD, source.as_ptr(), libc::AT_FDCWD, target.as_ptr(), libc::RENAME_NOREPLACE) };
        #[cfg(target_os = "macos")]
        let status = unsafe { libc::renamex_np(source.as_ptr(), target.as_ptr(), libc::RENAME_EXCL) };
        if status == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = (source, target);
        Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "Non-overwriting directory rename is unavailable."))
    }
}

/// Publish the dump and its metadata together. A failed write never truncates the previous checkpoint.
fn save_profile_state(workspace: &Path, id: &str, identity: &NetworkIdentity, state_hex: String, deployments: Vec<(String, String)>, tabs: Vec<(String, String)>, signer: Option<SignerChoice>) -> Result<(), String> {
    if !valid_dump(&state_hex) { return Err("The node did not return a valid state dump.".into()); }
    if id != identity.profile_id { return Err("The running profile does not match the checkpoint folder.".into()); }
    let dir = checked_profile_dir(workspace, id, true)?;
    let target = dir.join("state.json");
    checked_entry(&target, false)?;
    let checkpoint = SavedStateFile {
        format: STATE_FORMAT.into(), identity: identity.clone(), state: state_hex,
        metadata: Some(CheckpointMetadata { deployments, tabs, signer }),
    };
    let mut temp = tempfile::NamedTempFile::new_in(&dir).map_err(|error| error.to_string())?;
    serde_json::to_writer(temp.as_file_mut(), &checkpoint).map_err(|error| error.to_string())?;
    temp.as_file().sync_all().map_err(|error| error.to_string())?;
    temp.persist(target).map_err(|error| error.to_string())?;
    Ok(())
}

/// Dump and save the state of the profile that runs now, so switching back
/// (or reopening the project) can resume. Called before a switch and on
/// close. Skipped — never overwriting — while the profile's saved state is
/// held back (its identity was unknown at boot) or the identity is still
/// incomplete. The dump is small: only touched accounts.
/// Returns whether anything was written.
pub async fn save_running_profile(held: &std::sync::Arc<std::sync::Mutex<super::session::DesktopState>>) -> Result<bool, String> {
    let (workspace, profile, identity, rpc, deployments, tabs, signer) = {
        let state = held.lock().unwrap();
        if !state.workspace_ready {
            return Ok(false);
        }
        let Some(identity) = state.network_identity.clone() else { return Ok(false) };
        if state.profile_state_saved || state.profile_state_held {
            return Ok(false);
        }
        if !identity_complete(&identity, !state.network.is_local()) {
            return Err("the running network identity is incomplete; the previous checkpoint was not changed".into());
        }
        profile_name(&state.network.profile_id)?;
        (state.workspace.clone(), state.network.profile_id.clone(), identity, state.rpc.clone(), state.current_network_deployments(), state.current_network_tabs(), state.saved_signer())
    };
    let state_hex = dump_node_state(&rpc).await?;
    save_profile_state(&workspace, &profile, &identity, state_hex, deployments, tabs, signer).map(|_| true)
}

/// Read and validate a profile's saved state against the node that just
/// booted. A saved state from another chain is rejected and archived; the
/// same chain at a later fork block restores (the spike showed
/// anvil_loadState restores the touched accounts over a moved fork), with
/// `moved_blocks` set for the note. An incomplete identity defers.
pub fn load_profile_state(workspace: &Path, id: &str, current: &NetworkIdentity, is_fork: bool) -> LoadOutcome {
    load_profile_with_archive(workspace, id, current, is_fork, || archive_profile_state(workspace, id, "rejected"))
}

fn load_profile_with_archive(workspace: &Path, id: &str, current: &NetworkIdentity, is_fork: bool, archive: impl FnOnce() -> Result<String, String>) -> LoadOutcome {
    let (saved, legacy) = match read_checkpoint(workspace, id) {
        Ok(Some(checkpoint)) => checkpoint,
        Ok(None) => return LoadOutcome::None,
        Err(reason) => return held(reason),
    };
    if current.profile_id != id || saved.identity.profile_id != id {
        return held("The checkpoint does not match its profile folder.".into());
    }
    if !identity_complete(current, is_fork) {
        return held("The node's network identity is incomplete. Retry the connection and reopen the project to restore it.".into());
    }
    let old = &saved.identity;
    let saved_is_fork = is_fork || old.source_chain_id.is_some() || old.fork_block.is_some();
    // Version 1 did not always record an anchor. An explicitly malformed one is never trusted.
    let legacy_anchor = legacy && saved_is_fork && old.source_chain_id.is_some() && old.fork_block.is_some() && old.anchor_hash.is_none();
    if !legacy_anchor && !identity_complete(old, saved_is_fork) {
        return held("The saved network identity is incomplete.".into());
    }
    let reject = |why: String| match archive() {
        Ok(name) => LoadOutcome::Rejected { reason: format!("{why} This network starts clean. The saved state was moved to .chasm/networks/{name}.") },
        Err(error) => held(format!("{why} The saved folder could not be archived: {error}.")),
    };
    if old.source_chain_id != current.source_chain_id {
        let from = old.source_chain_id.map(super::toolchain::chain_name).unwrap_or_else(|| "the local chain".into());
        let to = current.source_chain_id.map(super::toolchain::chain_name).unwrap_or_else(|| "the local chain".into());
        return reject(format!("Saved state for this profile is from {from}, but the node runs {to}."));
    }
    let moved_blocks = old.fork_block != current.fork_block;
    if let (Some(previous), Some(now), true) = (&old.anchor_hash, &current.anchor_hash, is_fork) {
        if !moved_blocks && !previous.eq_ignore_ascii_case(now) {
            return reject("Saved state is anchored to a different block hash at the same height.".into());
        }
    }
    LoadOutcome::Restored { saved, moved_blocks }
}

fn held(reason: String) -> LoadOutcome {
    LoadOutcome::Deferred { reason: format!("{reason} The saved files are held and will not be overwritten.") }
}

fn read_checkpoint(workspace: &Path, id: &str) -> Result<Option<(SavedProfile, bool)>, String> {
    fn read<T: serde::de::DeserializeOwned>(dir: &Path, name: &str) -> Result<T, String> {
        let path = dir.join(name);
        if !checked_entry(&path, false)? { return Err(format!("The checkpoint is missing {name}.")); }
        let bytes = std::fs::read(path).map_err(|error| format!("Could not read {name}: {error}"))?;
        serde_json::from_slice(&bytes).map_err(|_| format!("Invalid checkpoint in {name}."))
    }
    let dir = profile_dir(workspace, id)?;
    if !checked_entry(&dir.join("state.json"), false)? {
        if checked_entry(&dir.join("deployments.json"), false)? || checked_entry(&dir.join("signer.json"), false)? {
            return Err("The checkpoint has metadata but no node state.".into());
        }
        return Ok(None);
    }
    let file: SavedStateFile = read(&dir, "state.json")?;
    if !valid_dump(&file.state) { return Err("The saved node dump is incomplete.".into()); }
    let legacy = file.format == LEGACY_FORMAT;
    let metadata = match file.format.as_str() {
        STATE_FORMAT => file.metadata.ok_or("The checkpoint has no metadata.")?,
        LEGACY_FORMAT => {
            let deployments: SavedDeploymentsFile = read(&dir, "deployments.json")?;
            let signer: SavedSignerFile = read(&dir, "signer.json")?;
            if deployments.format != LEGACY_FORMAT || signer.format != LEGACY_FORMAT || deployments.identity != file.identity || signer.identity != file.identity {
                return Err("The legacy checkpoint files do not describe the same network.".into());
            }
            CheckpointMetadata { deployments: deployments.deployments, tabs: deployments.tabs, signer: signer.signer }
        }
        _ => return Err("Unrecognized checkpoint version.".into()),
    };
    Ok(Some((SavedProfile { identity: file.identity, state: file.state, deployments: metadata.deployments, tabs: metadata.tabs, signer: metadata.signer }, legacy)))
}

/// The saved profiles as JSON for the UI and API. Hosts only, never URLs: a
/// provider key that sits in a fork URL must not reach a consumer.
pub fn list(workspace: &Path) -> Value {
    let config = match crate::config::ChasmConfig::load(workspace) {
        Ok(config) => config,
        Err(_) => return json!([]),
    };
    let active = config.network.active.trim().to_string();
    json!(config.networks.iter().map(|profile| json!({
        "id": profile.id,
        "name": profile.name,
        "host": if profile.is_local() {
            Value::Null
        } else if profile.fork_url.is_empty() {
            json!(format!("env {}", profile.fork_url_env))
        } else {
            json!(host_of(&profile.fork_url))
        },
        "forkBlock": profile.fork_block,
        "chainIdMode": profile.chain_id_mode.as_str(),
        "active": profile.id == active,
        "savedState": has_saved_state(workspace, &profile.id),
    })).collect::<Vec<_>>())
}

fn host_of(url: &str) -> String {
    reqwest::Url::parse(url).ok().and_then(|url| url.host_str().map(str::to_string)).unwrap_or_else(|| "remote".into())
}

/// Hex dump of the running node's state (`anvil_dumpState`): only accounts
/// the node has touched, so a fork dump stays small.
pub async fn dump_node_state(rpc: &str) -> Result<String, String> {
    let value: String = rpc_call(rpc, "anvil_dumpState", json!([])).await?;
    if valid_dump(&value) { Ok(value) } else { Err("The node returned an empty or invalid state dump.".into()) }
}

/// Restore a dump taken by `dump_node_state` onto the running node. The head
/// does not move; only account state changes.
pub async fn load_node_state(rpc: &str, state_hex: &str) -> Result<(), String> {
    match rpc_call::<bool>(rpc, "anvil_loadState", json!([state_hex])).await? {
        true => Ok(()),
        false => Err("The node did not accept the saved state.".into()),
    }
}

/// A snapshot id of the running node, so a restored point is revertable.
pub async fn snapshot(rpc: &str) -> Result<String, String> {
    rpc_call(rpc, "evm_snapshot", json!([])).await
}

async fn rpc_call<T: serde::de::DeserializeOwned>(rpc: &str, method: &str, params: Value) -> Result<T, String> {
    let client = reqwest::Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none()).timeout(std::time::Duration::from_secs(60)).build().map_err(|error| error.to_string())?;
    let mut value: Value = client
        .post(rpc)
        .json(&json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}))
        .send()
        .await
        .map_err(|error| format!("Could not reach the node: {error}"))?
        .json()
        .await
        .map_err(|error| format!("The node returned invalid JSON: {error}"))?;
    if value.get("error").is_some() { return Err(format!("The node refused {method}.")); }
    let result = value.get_mut("result").map(Value::take).ok_or_else(|| format!("The node returned no result for {method}."))?;
    serde_json::from_value(result).map_err(|_| format!("The node returned an invalid result for {method}."))
}

/// The fork record of the running node, from its own `anvil_nodeInfo`.
pub async fn node_info(rpc: &str) -> Option<Value> {
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(5)).build().ok()?;
    let body = json!({"jsonrpc":"2.0","id":1,"method":"anvil_nodeInfo","params":[]});
    let response = super::retry::send(|| client.post(rpc).json(&body)).await.ok()?;
    let value = response.json::<Value>().await.ok()?;
    Some(value)
}

/// The fork block actually in use, from the node's own `anvil_nodeInfo`. A
/// `latest` fork pins the block that was head at boot.
pub fn fork_block_of(node_info: Option<&Value>) -> Option<u64> {
    node_info.and_then(|info| info["result"]["forkConfig"]["forkBlockNumber"].as_u64())
}

/// The anchor of the running node: the hash of its fork block, read from the
/// node itself (Anvil serves forked blocks; `anvil_nodeInfo` has no hash). A
/// local chain anchors to its genesis block, which changes on every start.
pub async fn anchor_hash(rpc: &str, fork_block: Option<u64>) -> Option<String> {
    let block = fork_block.map(|block| format!("{block:#x}")).unwrap_or_else(|| "0x0".into());
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(15)).build().ok()?;
    let body = json!({"jsonrpc":"2.0","id":1,"method":"eth_getBlockByNumber","params":[block, false]});
    let value: Value = super::retry::send(|| client.post(rpc).json(&body)).await.ok()?.json().await.ok()?;
    value["result"]["hash"].as_str().filter(|hash| valid_hash(hash)).map(str::to_ascii_lowercase)
}

/// The source chain id of a fork RPC, with bounded retries for rate limits.
/// The error text names the host, never a key that may sit in the URL.
pub async fn fetch_source_chain_id(url: &str) -> Result<u64, String> {
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(15)).build().map_err(|error| error.to_string())?;
    let body = json!({"jsonrpc":"2.0","id":1,"method":"eth_chainId","params":[]});
    let response = super::retry::send(|| client.post(url).json(&body)).await.map_err(|_| format!("no answer from {}", host_of(url)))?;
    if response.status().as_u16() == 429 {
        return Err("still rate limited (HTTP 429) after retries; use a provider URL with a key".into());
    }
    let value: Value = response.json().await.map_err(|_| "the answer was not JSON-RPC".to_string())?;
    let text = value["result"].as_str().unwrap_or_default();
    text.strip_prefix("0x").and_then(|digits| u64::from_str_radix(digits, 16).ok())
        .ok_or_else(|| "the answer did not contain a hexadecimal chain id".into())
}

/// The switch marker: written before a network switch reopens the project,
/// so boot can fall back to the previous profile if the target never becomes
/// ready. Removed after any boot that gets a node ready.
#[derive(serde::Serialize, serde::Deserialize)]
struct SwitchRecord {
    from: String,
    to: String,
}

fn switch_path(root: &Path) -> PathBuf {
    root.join(".chasm").join("network-switch.json")
}

pub fn write_switch(root: &Path, from: &str, to: &str) {
    let (Ok(from_name), Ok(to_name)) = (profile_name(from), profile_name(to)) else { return };
    if from_name == to_name {
        return;
    }
    let record = SwitchRecord { from: from.to_string(), to: to.to_string() };
    if let Ok(text) = serde_json::to_string(&record) {
        let _ = std::fs::create_dir_all(root.join(".chasm"));
        let _ = std::fs::write(switch_path(root), text);
    }
}

pub fn read_switch(root: &Path) -> Option<(String, String)> {
    let text = std::fs::read_to_string(switch_path(root)).ok()?;
    let record: SwitchRecord = serde_json::from_str(&text).ok()?;
    (record.from != record.to).then_some((record.from, record.to))
}

pub fn clear_switch(root: &Path) {
    let _ = std::fs::remove_file(switch_path(root));
}

/// The profile that a failed switch falls back to, if one is recorded and it
/// still resolves. The marker is cleared either way: it describes one attempt.
pub fn fallback_settings(root: &Path, failed_profile: &str) -> Option<NetworkSettings> {
    let record = read_switch(root);
    clear_switch(root);
    let (from, to) = record?;
    let (from, to) = (profile_name(&from).ok()?, profile_name(&to).ok()?);
    if to != profile_name(failed_profile).ok()? || from == to { return None; }
    let config = crate::config::ChasmConfig::load(root).ok()?;
    if profile_name(config.network.active.trim()).ok()? != to { return None; }
    if from == crate::config::NetworkProfile::LOCAL_ID {
        return Some(NetworkSettings::local());
    }
    config.profile(from).and_then(|profile| profile.resolve_url().ok().map(|url| NetworkSettings {
        profile_id: profile.id.clone(),
        fork_url: url,
        fork_block: profile.fork_block,
        chain_id_mode: profile.chain_id_mode,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_keys_are_stable_per_profile_chain_and_block() {
        let mainnet = NetworkIdentity { profile_id: "mainnet".into(), source_chain_id: Some(1), fork_block: Some(21_000_000), anchor_hash: Some("0xabc".into()), execution_chain_id: 31337 };
        assert_eq!(mainnet.key(), "mainnet:1:21000000");
        let local = NetworkIdentity { profile_id: String::new(), source_chain_id: None, fork_block: None, anchor_hash: None, execution_chain_id: 31337 };
        assert_eq!(local.key(), "local:0:0");
        assert_ne!(mainnet.key(), NetworkIdentity { fork_block: Some(21_000_001), ..mainnet.clone() }.key());
        // Serde round trip keeps every field; key() ignores the anchor.
        let json = serde_json::to_string(&mainnet).unwrap();
        let back: NetworkIdentity = serde_json::from_str(&json).unwrap();
        assert_eq!(back, mainnet);
        assert!(!json.contains("anchor_hash") || json.contains("0xabc"));
    }

    #[test]
    fn fork_block_comes_from_real_node_info() {
        // The shape Anvil 1.x returns: forkConfig has the number, no hash.
        let info = serde_json::json!({"result":{"currentBlockNumber":"0x193cf29","forkConfig":{"forkUrl":"https://rpc.example","forkBlockNumber":26464041,"forkRetryBackoff":1000}}});
        assert_eq!(fork_block_of(Some(&info)), Some(26_464_041));
        assert_eq!(fork_block_of(Some(&serde_json::json!({"result":{"forkConfig":{}}}))), None, "a local chain has no fork block");
        assert_eq!(fork_block_of(None), None);
    }

    #[test]
    fn switch_markers_round_trip_and_clear() {
        let root = tempfile::tempdir().unwrap();
        assert!(read_switch(root.path()).is_none());
        write_switch(root.path(), "local", "local");
        assert!(read_switch(root.path()).is_none(), "same-profile switches write nothing");
        write_switch(root.path(), "", "local");
        assert!(read_switch(root.path()).is_none(), "both spellings refer to the built-in local chain");
        write_switch(root.path(), "local", "mainnet");
        assert_eq!(read_switch(root.path()), Some(("local".to_string(), "mainnet".to_string())));
        clear_switch(root.path());
        assert!(read_switch(root.path()).is_none());
        clear_switch(root.path());
    }

    #[test]
    fn fallback_only_applies_to_a_failed_switch_away() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(".chasm")).unwrap();
        std::fs::write(
            root.path().join(".chasm/chasm.toml"),
            "[[networks]]\nid = \"mainnet\"\nfork_url = \"https://rpc.example\"\n[network]\nactive = \"mainnet\"\n",
        ).unwrap();
        // The marker describes local -> mainnet; mainnet failed. The previous
        // network is the built-in local chain, which is not in [[networks]].
        write_switch(root.path(), "local", "mainnet");
        let fallback = fallback_settings(root.path(), "mainnet").unwrap();
        assert!(fallback.is_local());
        // No marker, or a fallback that names the failed profile: none.
        assert!(fallback_settings(root.path(), "sepolia").is_none());
        write_switch(root.path(), "mainnet", "mainnet");
        assert!(fallback_settings(root.path(), "mainnet").is_none(), "same-profile markers never fall back");
        write_switch(root.path(), "local", "sepolia");
        assert!(fallback_settings(root.path(), "mainnet").is_none(), "a stale marker targets another profile");
        assert!(read_switch(root.path()).is_none());
        std::fs::write(root.path().join(".chasm/chasm.toml"), "[[networks]]\nid = \"mainnet\"\nfork_url = \"https://rpc.example\"\n").unwrap();
        write_switch(root.path(), "mainnet", "local");
        assert_eq!(fallback_settings(root.path(), "").unwrap().profile_id, "mainnet");
    }

    fn identity(profile: &str, chain: Option<u64>, block: Option<u64>) -> NetworkIdentity {
        NetworkIdentity { profile_id: profile.into(), source_chain_id: chain, fork_block: block, anchor_hash: block.map(|_| format!("0x{}", "ab".repeat(32))), execution_chain_id: 31337 }
    }

    #[test]
    fn tags_name_their_network_and_files() {
        let mainnet = identity("mainnet", Some(1), Some(21_000_000)).tag();
        assert_eq!(mainnet.slug(), "mainnet-21000000");
        assert!(mainnet.describe().contains("Ethereum (1)"));
        let local = identity("", None, None).tag();
        assert_eq!(local.slug(), "local");
        assert_eq!(local.describe(), "the local chain");
        // Untagged data (None at the call site) is old, never foreign; an
        // explicit local tag IS foreign to a fork.
        let untagged: Option<NetworkTag> = None;
        assert!(!untagged.map(|tag| tag.is_foreign(Some(&identity("mainnet", Some(1), None)))).unwrap_or(false));
        assert!(Some(local.clone()).is_some_and(|tag| tag.is_foreign(Some(&identity("mainnet", Some(1), None)))));
        assert!(mainnet.is_foreign(Some(&identity("", None, None))), "a mainnet tag is foreign to the local chain");
        assert!(!mainnet.is_foreign(Some(&identity("mainnet", Some(1), Some(21_000_001)))), "only the chain is compared, not the block");
        assert!(mainnet.is_foreign(Some(&identity("sepolia", Some(11155111), None))));
        // Serde round trip keeps every field.
        let back: NetworkTag = serde_json::from_str(&serde_json::to_string(&mainnet).unwrap()).unwrap();
        assert_eq!(back, mainnet);
    }

    #[test]
    fn profile_state_saves_loads_archives_and_defers() {
        let root = tempfile::tempdir().unwrap();
        let networks = root.path().join(".chasm/networks");
        assert!(!has_saved_state(root.path(), "mainnet"));
        let saved = identity("mainnet", Some(1), Some(21_000_000));
        let write = |id: &str, who: &NetworkIdentity| save_profile_state(root.path(), id, who, "0xdeadbeef".into(), vec![("Bank".into(), "0x00aa".into())], vec![("Bank".into(), "0x00aa".into())], Some(SignerChoice::DevAccount { index: 0 })).unwrap();
        write("mainnet", &saved);
        assert!(has_saved_state(root.path(), "mainnet"));
        // Same chain, same block: restored.
        match load_profile_state(root.path(), "mainnet", &saved, true) {
            LoadOutcome::Restored { saved, moved_blocks } => {
                assert!(!moved_blocks);
                assert_eq!(saved.state, "0xdeadbeef");
                assert_eq!(saved.deployments, vec![("Bank".into(), "0x00aa".into())]);
                assert_eq!(saved.signer, Some(SignerChoice::DevAccount { index: 0 }));
            }
            other => panic!("{other:?}"),
        }
        // Same chain, later fork block (a moved `latest`): restored, noted.
        let later = NetworkIdentity { anchor_hash: Some(format!("0x{}", "cd".repeat(32))), ..identity("mainnet", Some(1), Some(21_000_500)) };
        assert!(matches!(load_profile_state(root.path(), "mainnet", &later, true), LoadOutcome::Restored { moved_blocks: true, .. }));
        // Source chain unknown (a 429 at boot): deferred, files untouched.
        let unknown = identity("mainnet", None, Some(21_000_000));
        assert!(matches!(load_profile_state(root.path(), "mainnet", &unknown, true), LoadOutcome::Deferred { .. }));
        assert!(has_saved_state(root.path(), "mainnet"), "a deferred state stays where it is");
        assert!(!identity_complete(&unknown, true));
        // Another chain: rejected and moved aside, never left to be overwritten.
        match load_profile_state(root.path(), "mainnet", &identity("mainnet", Some(11155111), Some(10)), true) {
            LoadOutcome::Rejected { reason } => assert!(reason.contains("Ethereum (1)") && reason.contains("Sepolia (11155111)") && reason.contains("mainnet.rejected-"), "{reason}"),
            other => panic!("{other:?}"),
        }
        assert!(!has_saved_state(root.path(), "mainnet"), "the rejected folder no longer sits where a save would overwrite it");
        let archived: Vec<String> = std::fs::read_dir(&networks).unwrap().flatten().map(|entry| entry.file_name().to_string_lossy().to_string()).collect();
        assert!(archived.iter().any(|name| name.starts_with("mainnet.rejected-")), "{archived:?}");
        // Same block, different anchor hash: rejected (reorg or another chain).
        write("forky", &identity("forky", Some(1), Some(5)));
        let reorganized = NetworkIdentity { anchor_hash: Some(format!("0x{}", "ee".repeat(32))), ..identity("forky", Some(1), Some(5)) };
        assert!(matches!(load_profile_state(root.path(), "forky", &reorganized, true), LoadOutcome::Rejected { .. }));
        // A local chain's genesis changes every start: anchors are not compared.
        write("", &NetworkIdentity { anchor_hash: Some(format!("0x{}", "aa".repeat(32))), ..identity("", None, None) });
        assert!(has_saved_state(root.path(), "local"));
        let restarted = NetworkIdentity { anchor_hash: Some(format!("0x{}", "bb".repeat(32))), ..identity("", None, None) };
        assert!(matches!(load_profile_state(root.path(), "", &restarted, false), LoadOutcome::Restored { .. }));
        // No saved files at all: None. Archiving nothing is an error.
        assert!(matches!(load_profile_state(root.path(), "ghost", &saved, true), LoadOutcome::None));
        assert!(archive_profile_state(root.path(), "ghost", "discarded").is_err());
        // A discarded clean fork is moved aside too.
        write("sepolia", &identity("sepolia", Some(11155111), Some(10)));
        let name = archive_profile_state(root.path(), "sepolia", "discarded").unwrap();
        assert!(name.starts_with("sepolia.discarded-") && !has_saved_state(root.path(), "sepolia"));
    }

    #[test]
    fn failed_archive_and_incomplete_fork_identity_preserve_the_checkpoint() {
        let root = tempfile::tempdir().unwrap();
        let old = identity("fork", Some(1), Some(42));
        save_profile_state(root.path(), "fork", &old, "0x1234".into(), vec![], vec![], None).unwrap();
        let dir = profile_dir(root.path(), "fork").unwrap();
        let bytes = std::fs::read(dir.join("state.json")).unwrap();
        let foreign = identity("fork", Some(2), Some(42));
        let failure = load_profile_with_archive(root.path(), "fork", &foreign, true, || {
            std::fs::rename(&dir, dir.join("missing/target")).map(|_| String::new()).map_err(|e| e.to_string())
        });
        assert!(matches!(failure, LoadOutcome::Deferred { reason } if reason.contains("could not be archived")));
        for incomplete in [NetworkIdentity { source_chain_id: None, ..old.clone() }, NetworkIdentity { fork_block: None, ..old.clone() },
            NetworkIdentity { anchor_hash: None, ..old.clone() }, NetworkIdentity { anchor_hash: Some(format!("0x{}g", "0".repeat(63))), ..old.clone() }] {
            assert!(!identity_complete(&incomplete, true));
            assert!(matches!(load_profile_state(root.path(), "fork", &incomplete, true), LoadOutcome::Deferred { .. }));
        }
        assert_eq!(std::fs::read(dir.join("state.json")).unwrap(), bytes);
    }

    #[test]
    fn legacy_metadata_must_match_and_is_never_mixed_into_a_new_checkpoint() {
        let root = tempfile::tempdir().unwrap();
        let dir = checked_profile_dir(root.path(), "fork", true).unwrap();
        let now = identity("fork", Some(1), Some(5));
        let old = NetworkIdentity { anchor_hash: None, ..now.clone() };
        let write = |name: &str, data: Value| std::fs::write(dir.join(name), data.to_string()).unwrap();
        write("state.json", json!({"format":LEGACY_FORMAT,"identity":old,"state":"0xab"}));
        write("deployments.json", json!({"format":LEGACY_FORMAT,"identity":old,"deployments":[["Legacy","0x42"]],"tabs":[]}));
        write("signer.json", json!({"format":LEGACY_FORMAT,"identity":now}));
        assert!(matches!(load_profile_state(root.path(), "fork", &now, true), LoadOutcome::Deferred { .. }));
        write("signer.json", json!({"format":LEGACY_FORMAT,"identity":old}));
        assert!(matches!(load_profile_state(root.path(), "fork", &now, true), LoadOutcome::Restored { saved, .. } if saved.deployments.len() == 1));
        save_profile_state(root.path(), "fork", &now, "0xcd".into(), vec![], vec![], None).unwrap();
        assert!(matches!(load_profile_state(root.path(), "fork", &now, true), LoadOutcome::Restored { saved, .. } if saved.deployments.is_empty() && saved.state == "0xcd"));
    }

    async fn with_rpc<F, Fut>(replies: Vec<(&'static str, Value, Value)>, run: F)
    where F: FnOnce(String) -> Fut, Fut: std::future::Future<Output = ()> {
        use axum::{routing::post, Json, Router};
        use std::sync::{Arc, Mutex};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let rpc = format!("http://{}", listener.local_addr().unwrap());
        let queue = Arc::new(Mutex::new(std::collections::VecDeque::from(replies)));
        let pending = queue.clone();
        let router = Router::new().route("/", post(move |Json(req): Json<Value>| {
            let (method, params, result) = pending.lock().unwrap().pop_front().expect("unexpected RPC");
            async move {
                assert_eq!(req["method"], method);
                assert_eq!(req["params"], params);
                Json(json!({"jsonrpc":"2.0","id":1,"result":result}))
            }
        }));
        let (stop, shutdown) = tokio::sync::oneshot::channel::<()>();
        let serve = axum::serve(listener, router).with_graceful_shutdown(async { let _ = shutdown.await; });
        tokio::join!(async { serve.await.unwrap(); }, async { run(rpc).await; let _ = stop.send(()); });
        assert!(queue.lock().unwrap().is_empty(), "missing RPC call");
    }

    #[tokio::test]
    async fn node_restore_requires_boolean_true_and_snapshot_uses_evm_rpc() {
        let sequence = vec![
            ("anvil_loadState", json!(["0x12"]), json!(true)),
            ("anvil_loadState", json!(["0x12"]), json!(false)),
            ("anvil_loadState", json!(["0x12"]), json!("true")),
            ("evm_snapshot", json!([]), json!("0x1")),
        ];
        with_rpc(sequence, |rpc| async move {
            assert!(load_node_state(&rpc, "0x12").await.is_ok());
            assert!(load_node_state(&rpc, "0x12").await.is_err());
            assert!(load_node_state(&rpc, "0x12").await.is_err());
            assert_eq!(snapshot(&rpc).await.unwrap(), "0x1");
        }).await;
    }

    #[tokio::test]
    async fn failed_dump_or_load_never_replaces_the_last_good_checkpoint() {
        let sequence = vec![
            ("anvil_dumpState", json!([]), Value::Null),
            ("anvil_dumpState", json!([]), json!("0x")),
            ("anvil_loadState", json!(["0x1234"]), json!(false)),
            ("evm_snapshot", json!([]), json!("0x2")),
        ];
        with_rpc(sequence, |rpc| async move {
            use super::super::session::DesktopState;
            use std::sync::{Arc, Mutex};
            let root = tempfile::tempdir().unwrap();
            let who = identity("", None, None);
            save_profile_state(root.path(), "", &who, "0x1234".into(), vec![("Old".into(), "0x42".into())], vec![], Some(SignerChoice::DevAccount { index: 0 })).unwrap();
            let path = profile_dir(root.path(), "").unwrap().join("state.json");
            let bytes = std::fs::read(&path).unwrap();
            let mut state = DesktopState::new(root.path().to_owned(), 1.0);
            state.rpc = rpc;
            state.network_identity = Some(who);
            state.notice = "Previous switch failed.".into();
            let shared = Arc::new(Mutex::new(state));
            assert!(save_running_profile(&shared).await.is_err());
            assert!(save_running_profile(&shared).await.is_err());
            let mut state = Arc::try_unwrap(shared).ok().unwrap().into_inner().unwrap();
            super::super::apply_saved_state(&mut state, root.path()).await;
            assert!(state.profile_state_held);
            assert!(state.deployments.is_empty() && state.documents.is_empty() && state.saved_signer().is_none());
            assert_eq!(state.notice, "Previous switch failed.");
            assert!(state.logs.iter().any(|line| line.contains("could not be loaded")));
            assert!(state.logs.iter().all(|line| !line.contains("Resumed profile")));
            assert_eq!(save_running_profile(&Arc::new(Mutex::new(state))).await, Ok(false));
            assert_eq!(std::fs::read(path).unwrap(), bytes);
        }).await;
    }

    #[test]
    fn fresh_archive_rollback_only_moves_the_original_into_an_absent_destination() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(archive_for_fresh(root.path(), "fork").unwrap(), None);
        assert!(archive_for_fresh(root.path(), "../invalid").is_err());
        let original = identity("fork", Some(1), Some(11));
        save_profile_state(root.path(), "fork", &original, "0x12".into(), vec![], vec![], None).unwrap();
        let archive = archive_for_fresh(root.path(), "fork").unwrap().unwrap();
        assert!(restore_archived_profile(root.path(), "other", &archive).is_err());
        assert!(restore_archived_profile(root.path(), "fork", "fork.discarded-../other").is_err());
        restore_archived_profile(root.path(), "fork", &archive).unwrap();
        assert!(matches!(load_profile_state(root.path(), "fork", &original, true), LoadOutcome::Restored { .. }));
        let archive = archive_for_fresh(root.path(), "fork").unwrap().unwrap();
        let target = checked_profile_dir(root.path(), "fork", true).unwrap();
        assert!(restore_archived_profile(root.path(), "fork", &archive).is_err());
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 0);
        assert!(target.with_file_name(archive).join("state.json").is_file());
    }

    #[test]
    fn invalid_profile_paths_cannot_create_or_archive_folders() {
        let root = tempfile::tempdir().unwrap();
        for id in ["../x", "a/b", "a\\b", "C:\\x", "/tmp/x", ".", "..", " ", " abc ", "nul", "com1", "a:b"] {
            let who = identity(id, Some(1), Some(5));
            assert!(save_profile_state(root.path(), id, &who, "0xab".into(), vec![], vec![], None).is_err());
            assert!(archive_profile_state(root.path(), id, "rejected").is_err());
        }
        assert!(!root.path().join(".chasm").exists());
        let dir = checked_profile_dir(root.path(), "fork", true).unwrap();
        assert!(archive_profile_state(root.path(), "fork", "../escape").is_err());
        assert!(dir.is_dir());
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn linked_folders_and_checkpoint_files_do_not_redirect_storage() {
        let outside = tempfile::tempdir().unwrap();
        let sentinel = outside.path().join("state.json");
        std::fs::write(&sentinel, "untouched").unwrap();
        for rel in [".chasm", ".chasm/networks", ".chasm/networks/fork", ".chasm/networks/fork/state.json"] {
            let root = tempfile::tempdir().unwrap();
            let link = root.path().join(rel);
            std::fs::create_dir_all(link.parent().unwrap()).unwrap();
            let file = rel.ends_with("state.json");
            let target = if file { sentinel.as_path() } else { outside.path() };
            #[cfg(unix)]
            std::os::unix::fs::symlink(target, &link).unwrap();
            #[cfg(windows)]
            {
                let result = if file { std::os::windows::fs::symlink_file(target, &link) } else { std::os::windows::fs::symlink_dir(target, &link) };
                if let Err(error) = result {
                    if error.raw_os_error() == Some(1314) { return; } // Windows may not grant symlink creation.
                    panic!("{error}");
                }
            }
            let who = identity("fork", Some(1), Some(5));
            assert!(save_profile_state(root.path(), "fork", &who, "0xab".into(), vec![], vec![], None).is_err());
            assert!(matches!(load_profile_state(root.path(), "fork", &who, true), LoadOutcome::Deferred { .. }));
            if !file { assert!(archive_profile_state(root.path(), "fork", "discarded").is_err()); }
            assert_eq!(std::fs::read(&sentinel).unwrap(), b"untouched");
            assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 1);
        }
    }

    #[test]
    fn local_tags_carry_the_session_so_restarts_never_collide() {
        let first = NetworkIdentity { anchor_hash: Some(format!("0x{}", "1a".repeat(32))), ..identity("", None, None) };
        let second = NetworkIdentity { anchor_hash: Some(format!("0x{}", "2b".repeat(32))), ..identity("", None, None) };
        assert_eq!(first.tag().slug(), "local-1a1a1a1a");
        assert_ne!(first.tag().slug(), second.tag().slug());
        // A fork's slug stays readable: its block already pins it.
        let fork = NetworkIdentity { anchor_hash: Some(format!("0x{}", "3c".repeat(32))), ..identity("mainnet", Some(1), Some(7)) };
        assert_eq!(fork.tag().slug(), "mainnet-7");
    }
}
