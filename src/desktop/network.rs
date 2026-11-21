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
        NetworkTag { profile: self.profile_id.clone(), chain: self.source_chain_id, fork_block: self.fork_block }
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
}

impl NetworkTag {
    /// Parse back a tag from the `profile:chain:block` key that `key()` and
    /// saved entries use.
    pub fn from_key(key: &str) -> Option<Self> {
        let mut parts = key.split(':');
        let profile = parts.next()?.to_string();
        let chain = parts.next()?.parse::<u64>().ok();
        let fork_block = parts.next()?.parse::<u64>().ok();
        Some(Self { profile: if profile == "local" { String::new() } else { profile }, chain: chain.filter(|chain| *chain != 0), fork_block: fork_block.filter(|block| *block != 0) })
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
        match self.fork_block {
            Some(block) => format!("{profile}-{block}"),
            None => profile,
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
    /// Hex dump from `anvil_dumpState`, when it was taken.
    pub state: Option<String>,
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
    Rejected { reason: String },
}

const STATE_FORMAT: &str = "chasm-profile/1";

pub fn profile_dir(workspace: &Path, id: &str) -> PathBuf {
    let id = if id.trim().is_empty() { "local" } else { id.trim() };
    workspace.join(".chasm").join("networks").join(id)
}

/// Whether a profile has a saved node state.
pub fn has_saved_state(workspace: &Path, id: &str) -> bool {
    profile_dir(workspace, id).join("state.json").is_file()
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SavedStateFile {
    format: String,
    identity: NetworkIdentity,
    /// Hex dump from anvil_dumpState.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    state: Option<String>,
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

/// Write the three saved-state files. Failures leave the folder partial and
/// are reported; the switch still happens.
pub fn save_profile_state(workspace: &Path, id: &str, identity: &NetworkIdentity, state_hex: Option<&str>, deployments: &[(String, String)], tabs: &[(String, String)], signer: Option<&SignerChoice>) -> Result<(), String> {
    let dir = profile_dir(workspace, id);
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    fn write(name: &std::path::Path, value: &impl serde::Serialize) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
        std::fs::write(name, bytes).map_err(|error| error.to_string())
    }
    write(&dir.join("state.json"), &SavedStateFile { format: STATE_FORMAT.into(), identity: identity.clone(), state: state_hex.map(str::to_string) })?;
    write(&dir.join("deployments.json"), &SavedDeploymentsFile { format: STATE_FORMAT.into(), identity: identity.clone(), deployments: deployments.to_vec(), tabs: tabs.to_vec() })?;
    write(&dir.join("signer.json"), &SavedSignerFile { format: STATE_FORMAT.into(), identity: identity.clone(), signer: signer.cloned() })
}

/// Read and validate a profile's saved state against the node that just
/// booted. A saved state from another chain is rejected; the same chain at a
/// later fork block restores (the spike showed anvil_loadState restores the
/// touched accounts over a moved fork), with `moved_blocks` set for the note.
pub fn load_profile_state(workspace: &Path, id: &str, current: &NetworkIdentity) -> LoadOutcome {
    let dir = profile_dir(workspace, id);
    let read = |name: &str| -> Option<NetworkIdentity> {
        let bytes = std::fs::read(dir.join(name)).ok()?;
        let file: SavedStateFile = serde_json::from_slice(&bytes).ok()?;
        file.format.eq(STATE_FORMAT).then_some(file).map(|file| file.identity)
    };
    let Some(saved_identity) = read("state.json").or_else(|| read("deployments.json").or_else(|| read("signer.json"))) else {
        return LoadOutcome::None;
    };
    if saved_identity.source_chain_id != current.source_chain_id {
        let from = saved_identity.source_chain_id.map(super::toolchain::chain_name).unwrap_or_else(|| "the local chain".into());
        let to = current.source_chain_id.map(super::toolchain::chain_name).unwrap_or_else(|| "the local chain".into());
        return LoadOutcome::Rejected { reason: format!("Saved state for this profile is from {from}, but the node runs {to}. It starts clean; the saved files are kept.") };
    }
    if saved_identity.fork_block == current.fork_block && saved_identity.anchor_hash != current.anchor_hash {
        return LoadOutcome::Rejected { reason: "Saved state is anchored to a different block of the same chain (the chain reorganized or the RPC moved). It starts clean; the saved files are kept.".into() };
    }
    let moved_blocks = saved_identity.fork_block != current.fork_block;
    let state = std::fs::read(dir.join("state.json")).ok().and_then(|bytes| {
        let file: SavedStateFile = serde_json::from_slice(&bytes).ok()?;
        file.state
    });
    let (deployments, tabs) = std::fs::read(dir.join("deployments.json")).ok().and_then(|bytes| {
        let file: SavedDeploymentsFile = serde_json::from_slice(&bytes).ok()?;
        Some((file.deployments, file.tabs))
    }).unwrap_or_default();
    let signer = std::fs::read(dir.join("signer.json")).ok().and_then(|bytes| {
        let file: SavedSignerFile = serde_json::from_slice(&bytes).ok()?;
        file.signer
    });
    LoadOutcome::Restored { saved: SavedProfile { identity: saved_identity, state, deployments, tabs, signer }, moved_blocks }
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
    rpc_call(rpc, "anvil_dumpState", json!([])).await
}

/// Restore a dump taken by `dump_node_state` onto the running node. The head
/// does not move; only account state changes.
pub async fn load_node_state(rpc: &str, state_hex: &str) -> Result<(), String> {
    rpc_call(rpc, "anvil_loadState", json!([state_hex])).await.map(|_| ())
}

/// A snapshot id of the running node, so a restored point is revertable.
pub async fn snapshot(rpc: &str) -> Result<String, String> {
    rpc_call(rpc, "anvil_snapshot", json!([])).await
}

/// Impersonate an address on the running node, for a restored signer choice.
pub async fn impersonate(rpc: &str, address: &str) -> Result<(), String> {
    rpc_call(rpc, "anvil_impersonateAccount", json!([address])).await.map(|_| ())
}

async fn rpc_call(rpc: &str, method: &str, params: Value) -> Result<String, String> {
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(60)).build().map_err(|error| error.to_string())?;
    let value: Value = client
        .post(rpc)
        .json(&json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}))
        .send()
        .await
        .map_err(|error| format!("Could not reach the node: {error}"))?
        .json()
        .await
        .map_err(|error| format!("The node returned invalid JSON: {error}"))?;
    match value["result"].as_str() {
        Some(result) => Ok(result.to_string()),
        None => Err(format!("The node refused {method}: {}", value["error"]["message"].as_str().unwrap_or("no result"))),
    }
}

/// The fork record of the running node, from its own `anvil_nodeInfo`.
pub async fn node_info(rpc: &str) -> Option<Value> {
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(5)).build().ok()?;
    let body = json!({"jsonrpc":"2.0","id":1,"method":"anvil_nodeInfo","params":[]});
    let response = super::retry::send(|| client.post(rpc).json(&body)).await.ok()?;
    let value = response.json::<Value>().await.ok()?;
    Some(value)
}

/// `(fork block, anchor hash)` actually in use, from the node itself. A
/// `latest` fork pins the block that was head at boot; hash when reported.
pub fn fork_anchor(node_info: Option<&Value>) -> (Option<u64>, Option<String>) {
    let config = node_info.and_then(|info| info["result"]["forkConfig"].as_object());
    let block = config.and_then(|config| config.get("forkBlockNumber").and_then(Value::as_u64));
    let hash = config
        .and_then(|config| config.get("forkBlockHash").and_then(Value::as_str))
        .filter(|hash| hash.starts_with("0x") && hash.len() >= 10)
        .map(str::to_string);
    (block, hash)
}

/// The source chain id of a fork RPC, with bounded retries for rate limits.
/// The error text names the host, never a key that may sit in the URL.
pub async fn fetch_source_chain_id(url: &str) -> Result<u64, String> {
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(15)).build().map_err(|error| error.to_string())?;
    let body = json!({"jsonrpc":"2.0","id":1,"method":"eth_chainId","params":[]});
    let response = super::retry::send(|| client.post(url).json(&body)).await.map_err(|error| format!("no answer ({error})"))?;
    if response.status().as_u16() == 429 {
        return Err("still rate limited (HTTP 429) after retries; use a provider URL with a key".into());
    }
    let value: Value = response.json().await.map_err(|_| "the answer was not JSON-RPC".to_string())?;
    let text = value["result"].as_str().unwrap_or_default();
    u64::from_str_radix(text.trim_start_matches("0x"), 16)
        .map_err(|_| format!("the answer named chain id {text:?} instead of a quantity"))
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
    if from == to {
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
    let (from, _to) = read_switch(root)?;
    clear_switch(root);
    if from == failed_profile {
        return None;
    }
    let config = crate::config::ChasmConfig::load(root).ok()?;
    if config.network.active.trim() != failed_profile {
        return None;
    }
    if from.is_empty() || from == crate::config::NetworkProfile::LOCAL_ID {
        return Some(NetworkSettings::local());
    }
    config.profile(&from).and_then(|profile| profile.resolve_url().ok().map(|url| NetworkSettings {
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
    fn fork_anchor_reads_only_sane_node_info() {
        let info = serde_json::json!({"result":{"forkConfig":{"forkBlockNumber":19_250_001,"forkBlockHash":"0x9f2d4c8ab1"}}});
        assert_eq!(fork_anchor(Some(&info)), (Some(19_250_001), Some("0x9f2d4c8ab1".to_string())));
        assert_eq!(fork_anchor(Some(&serde_json::json!({"result":{}}))), (None, None));
        assert_eq!(fork_anchor(None), (None, None));
    }

    #[test]
    fn switch_markers_round_trip_and_clear() {
        let root = tempfile::tempdir().unwrap();
        assert!(read_switch(root.path()).is_none());
        write_switch(root.path(), "local", "local");
        assert!(read_switch(root.path()).is_none(), "same-profile switches write nothing");
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
        clear_switch(root.path());
    }

    fn identity(profile: &str, chain: Option<u64>, block: Option<u64>) -> NetworkIdentity {
        NetworkIdentity { profile_id: profile.into(), source_chain_id: chain, fork_block: block, anchor_hash: block.map(|_| "0xabc".into()), execution_chain_id: 31337 }
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
    fn profile_state_saves_loads_and_rejects_other_chains() {
        let root = tempfile::tempdir().unwrap();
        assert!(!has_saved_state(root.path(), "mainnet"));
        let saved = identity("mainnet", Some(1), Some(21_000_000));
        save_profile_state(root.path(), "mainnet", &saved, Some("0xdeadbeef"), &[("Bank".into(), "0x00aa".into())], &[("Bank".into(), "0x00aa".into())], Some(&SignerChoice::DevAccount { index: 0 })).unwrap();
        assert!(has_saved_state(root.path(), "mainnet"));
        // Same chain, same block: restored.
        match load_profile_state(root.path(), "mainnet", &saved) {
            LoadOutcome::Restored { saved, moved_blocks } => {
                assert!(!moved_blocks);
                assert_eq!(saved.state.as_deref(), Some("0xdeadbeef"));
                assert_eq!(saved.deployments, vec![("Bank".into(), "0x00aa".into())]);
                assert_eq!(saved.signer, Some(SignerChoice::DevAccount { index: 0 }));
            }
            other => panic!("{other:?}"),
        }
        // Same chain, later fork block: restored with the moved-block note.
        let later = identity("mainnet", Some(1), Some(21_000_500));
        match load_profile_state(root.path(), "mainnet", &later) {
            LoadOutcome::Restored { moved_blocks, .. } => assert!(moved_blocks),
            other => panic!("{other:?}"),
        }
        // Another chain: rejected, files kept.
        match load_profile_state(root.path(), "mainnet", &identity("sepolia", Some(11155111), None)) {
            LoadOutcome::Rejected { reason } => assert!(reason.contains("Ethereum (1)") && reason.contains("Sepolia (11155111)") && reason.contains("kept"), "{reason}"),
            other => panic!("{other:?}"),
        }
        assert!(has_saved_state(root.path(), "mainnet"), "a rejection never deletes the files");
        // Same block, different anchor: rejected as another chain point.
        save_profile_state(root.path(), "forky", &identity("forky", Some(1), Some(5)), None, &[], &[], None).unwrap();
        let reorganized = NetworkIdentity { anchor_hash: Some("0xeee".into()), ..identity("forky", Some(1), Some(5)) };
        assert!(matches!(load_profile_state(root.path(), "forky", &reorganized), LoadOutcome::Rejected { .. }));
        // No saved files at all: None.
        assert!(matches!(load_profile_state(root.path(), "ghost", &saved), LoadOutcome::None));
        // The local profile stores under its own folder.
        save_profile_state(root.path(), "", &identity("", None, None), None, &[], &[], None).unwrap();
        assert!(has_saved_state(root.path(), "local"));
    }
}
