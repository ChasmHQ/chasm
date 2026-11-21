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
    })).collect::<Vec<_>>())
}

fn host_of(url: &str) -> String {
    reqwest::Url::parse(url).ok().and_then(|url| url.host_str().map(str::to_string)).unwrap_or_else(|| "remote".into())
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
}
