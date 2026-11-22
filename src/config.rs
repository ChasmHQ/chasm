use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Struct untuk .chasm/chasm.toml — format kompatibel dengan foundry.toml
#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct ChasmConfig {
    #[serde(default)]
    pub remappings: Vec<String>,
    /// `[compiler]`: a locked solc version, or none to pick one per pragma.
    #[serde(default, skip_serializing_if = "CompilerConfig::is_auto")]
    pub compiler: CompilerConfig,
    /// `[network]`: which saved profile is active. The old fork fields here
    /// are migrated into a profile on load and never written back.
    #[serde(default, skip_serializing_if = "NetworkConfig::is_default")]
    pub network: NetworkConfig,
    /// `[[networks]]`: named fork targets. The built-in `local` profile is
    /// not listed; it is what an empty active id means.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub networks: Vec<NetworkProfile>,
    /// `[build]`: where remappings and solc settings come from.
    #[serde(default, skip_serializing_if = "BuildConfig::is_default")]
    pub build: BuildConfig,
    /// Keys Chasm does not manage (for example foundry.toml settings) are
    /// kept verbatim so saving remappings never drops the user's settings.
    #[serde(flatten)]
    pub other: toml::Table,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone, PartialEq)]
pub struct CompilerConfig {
    /// Exact solc version such as "0.8.24". Empty means Auto: each source
    /// gets the newest installed version its pragma allows, downloading one
    /// when none fits.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub solc: String,
    /// solc settings, as in foundry.toml. Deployed code only matches its
    /// source when these match how it was built.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub optimizer: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub optimizer_runs: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via_ir: Option<bool>,
    /// For example "cancun"; empty uses the solc default.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub evm_version: String,
    /// "ipfs" (solc default), "bzzr1" or "none".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub bytecode_hash: String,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone, PartialEq)]
pub struct BuildConfig {
    /// auto (default), foundry or chasm. See `build_config`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<crate::build_config::Source>,
    /// Foundry projects whose `out/` artifacts join the debugger's programs
    /// without recompiling. See `desktop::foundry_import`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub imports: Vec<String>,
}

impl BuildConfig {
    pub fn is_default(&self) -> bool {
        self.source.is_none() && self.imports.is_empty()
    }
}

/// The chain id the workspace node reports. `Local` keeps 31337, so a signed
/// transaction can never be valid on the forked network. `Source` runs the
/// node with the forked chain's own id (EIP-712 domains and `block.chainid`
/// behave as on the real chain); sending is then restricted to Anvil dev keys
/// and impersonation, because a keystore signature would be replayable there.
#[derive(Debug, Serialize, Deserialize, Default, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ChainIdMode {
    #[default]
    Local,
    Source,
}

impl ChainIdMode {
    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Source => "source",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "local" | "" => Some(Self::Local),
            "source" => Some(Self::Source),
            _ => None,
        }
    }
}

/// One named fork target in `[[networks]]`. `local` is built in: it is the
/// empty active id, never listed and never removed.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NetworkProfile {
    /// Short stable id ("mainnet"); referenced by `network.active`.
    pub id: String,
    /// Display name ("Ethereum mainnet"); the id is shown when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// HTTPS RPC of the chain to fork; empty runs a fresh local chain.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fork_url: String,
    /// Env var that holds the RPC URL instead, so a provider key stays out of
    /// this file. Shown only as a name; UI and API never print its value.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fork_url_env: String,
    /// Block to fork at; none forks the latest block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork_block: Option<u64>,
    #[serde(default, skip_serializing_if = "ChainIdMode::is_local")]
    pub chain_id_mode: ChainIdMode,
    /// Empty leaves Anvil's hardfork selection in place.
    #[serde(default, skip_serializing_if = "String::is_empty", deserialize_with = "deserialize_hardfork")]
    pub hardfork: String,
    /// None selects Optimism mode from the source chain id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub optimism: Option<bool>,
}

fn deserialize_hardfork<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

pub(crate) fn validate_hardfork(hardfork: &str, optimism: Option<bool>) -> Result<(), String> {
    // Anvil 1.2.1 selects a different hardfork parser with --optimism.
    let op_hardfork = match hardfork.to_ascii_lowercase().as_str() {
        "" => return Ok(()),
        "frontier" | "homestead" | "dao" | "tangerine" | "spuriousdragon" | "byzantium"
        | "constantinople" | "petersburg" | "istanbul" | "muirglacier" | "berlin" | "london"
        | "arrowglacier" | "grayglacier" | "paris" | "merge" | "shanghai" | "cancun" | "prague"
        | "latest" => false,
        "bedrock" | "regolith" | "canyon" | "ecotone" | "fjord" | "granite" | "holocene"
        | "isthmus" => true,
        _ => return Err(format!("Unknown Anvil hardfork {hardfork:?}. Use an Anvil 1.2.1 hardfork name such as cancun or holocene.")),
    };
    match optimism {
        Some(false) if op_hardfork => Err(format!("Anvil hardfork {hardfork:?} requires Optimism mode.")),
        Some(true) if !op_hardfork => Err(format!("Anvil hardfork {hardfork:?} is not available in Optimism mode.")),
        _ => Ok(()),
    }
}

impl NetworkProfile {
    pub const LOCAL_ID: &str = "local";

    /// Whether this profile runs a fresh local chain (no RPC at all).
    pub fn is_local(&self) -> bool {
        self.fork_url.is_empty() && self.fork_url_env.is_empty()
    }

    /// Display title: the name, else the id.
    pub fn title(&self) -> &str {
        if self.name.trim().is_empty() { &self.id } else { &self.name }
    }

    /// The RPC URL, read from the env var when `fork_url_env` is set. The
    /// error names the variable, never its value.
    pub fn resolve_url(&self) -> Result<String, String> {
        if !self.fork_url.is_empty() {
            return Ok(self.fork_url.clone());
        }
        if !self.fork_url_env.is_empty() {
            let value = std::env::var(&self.fork_url_env)
                .map_err(|_| format!("Environment variable {} (fork_url_env of profile {}) is not set.", self.fork_url_env, self.id))?;
            let value = value.trim().to_string();
            if value.is_empty() {
                return Err(format!("Environment variable {} (fork_url_env of profile {}) is empty.", self.fork_url_env, self.id));
            }
            return Ok(value);
        }
        Ok(String::new())
    }

    /// Check values before they are saved or booted, so a typo names the field.
    pub fn validate(&self) -> Result<(), String> {
        let id = &self.id;
        if id.is_empty() {
            return Err("A profile needs an id.".into());
        }
        if id.len() > 40 || !id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_') {
            return Err(format!("Profile id {:?} must be up to 40 lowercase letters, digits, - or _.", id));
        }
        if id == Self::LOCAL_ID {
            return Err("\"local\" is the built-in profile and cannot be redefined.".into());
        }
        if self.name.len() > 80 {
            return Err("Profile name is limited to 80 characters.".into());
        }
        if !self.fork_url.is_empty() && !self.fork_url_env.is_empty() {
            return Err(format!("Profile {}: set either fork_url or fork_url_env, not both.", id));
        }
        if !self.fork_url.is_empty() {
            Self::check_url(&self.fork_url)?;
        }
        if !self.fork_url_env.is_empty() && (self.fork_url_env.contains(|c: char| c.is_whitespace()) || self.fork_url_env.len() > 128) {
            return Err(format!("Profile {}: fork_url_env must be a plain environment variable name.", id));
        }
        validate_hardfork(&self.hardfork, self.optimism).map_err(|error| format!("Profile {id}: {error}"))?;
        if self.is_local() && (!self.hardfork.is_empty() || self.optimism.is_some()) {
            return Err(format!("Profile {id}: hardfork and optimism require a fork RPC."));
        }
        Ok(())
    }

    /// http(s) URL with a host; provider keys often sit in the path, so only
    /// the host is echoed back.
    pub fn check_url(url: &str) -> Result<(), String> {
        let parsed = reqwest::Url::parse(url).map_err(|_| "Enter an RPC URL such as https://ethereum-rpc.publicnode.com.".to_string())?;
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            return Err("The fork RPC must be an http(s) URL.".into());
        }
        Ok(())
    }
}

/// The network the node actually runs, resolved from the active profile at
/// boot. Kept in `DesktopState`; the config file only holds profiles.
#[derive(Debug, Serialize, Deserialize, Default, Clone, PartialEq)]
pub struct NetworkSettings {
    /// Active profile id, or the built-in local chain when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub profile_id: String,
    /// Resolved RPC URL (env var applied); empty for a local chain.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fork_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork_block: Option<u64>,
    #[serde(default)]
    pub chain_id_mode: ChainIdMode,
    #[serde(default, skip_serializing_if = "String::is_empty", deserialize_with = "deserialize_hardfork")]
    pub hardfork: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub optimism: Option<bool>,
}

impl NetworkSettings {
    pub fn local() -> Self {
        Self::default()
    }

    pub fn is_local(&self) -> bool {
        self.fork_url.is_empty()
    }

    pub fn execution_chain_id(&self, source_chain_id: Option<u64>) -> u64 {
        match self.chain_id_mode {
            ChainIdMode::Local => 31337,
            // A Source profile without a readable source id is refused at boot.
            ChainIdMode::Source => source_chain_id.unwrap_or(31337),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Default, Clone, PartialEq)]
pub struct NetworkConfig {
    /// Id of the active profile in `[[networks]]`; empty runs a local chain.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub active: String,
    /// Old single-network setting, migrated into a profile on load. Kept in
    /// the struct so an old file parses; never written back.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fork_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork_block: Option<u64>,
}

impl NetworkConfig {
    pub fn is_default(&self) -> bool {
        self.active.is_empty() && self.fork_url.is_empty() && self.fork_block.is_none()
    }

    /// The fork profile the active id names, if any.
    pub fn active_profile<'a>(&self, networks: &'a [NetworkProfile]) -> Option<&'a NetworkProfile> {
        let id = self.active.trim();
        if id.is_empty() || id == NetworkProfile::LOCAL_ID {
            return None;
        }
        networks.iter().find(|profile| profile.id == id)
    }
}

impl CompilerConfig {
    /// Nothing set: Auto solc with solc's default settings.
    pub fn is_auto(&self) -> bool {
        self == &Self::default()
    }

    /// Check values before they reach solc, so a typo names the key.
    pub fn validate(&self) -> Result<(), String> {
        if !self.evm_version.is_empty() && self.evm_version.parse::<foundry_compilers::artifacts::EvmVersion>().is_err() {
            return Err(format!("[compiler] evm_version {:?} is not an EVM version such as cancun or shanghai.", self.evm_version));
        }
        if !self.bytecode_hash.is_empty() && !matches!(self.bytecode_hash.as_str(), "ipfs" | "bzzr1" | "none") {
            return Err(format!("[compiler] bytecode_hash {:?} must be ipfs, bzzr1 or none.", self.bytecode_hash));
        }
        if self.optimizer_runs.is_some_and(|runs| runs > u32::MAX as usize) {
            return Err("[compiler] optimizer_runs is too large.".into());
        }
        Ok(())
    }

    /// One line for the UI, e.g. "optimizer 200 runs, via-IR, cancun, no metadata hash".
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        match (self.optimizer, self.optimizer_runs) {
            (Some(true), Some(runs)) => parts.push(format!("optimizer {runs} runs")),
            (Some(true), None) => parts.push("optimizer 200 runs".into()),
            _ => parts.push("optimizer off".into()),
        }
        if self.via_ir == Some(true) {
            parts.push("via-IR".into());
        }
        if !self.evm_version.is_empty() {
            parts.push(self.evm_version.clone());
        }
        if self.bytecode_hash == "none" {
            parts.push("no metadata hash".into());
        } else if !self.bytecode_hash.is_empty() {
            parts.push(format!("{} metadata hash", self.bytecode_hash));
        }
        parts.join(", ")
    }
}

impl ChasmConfig {
    /// Baca config dari {root_dir}/.chasm/chasm.toml
    /// Jika file tidak ada, return default (empty). A file that exists but
    /// cannot be read or parsed is an error, never an empty config: callers
    /// that save afterwards would otherwise overwrite the user's file.
    pub fn load(root_dir: &Path) -> Result<Self> {
        let config_path = root_dir.join(".chasm").join("chasm.toml");
        if config_path.exists() {
            let content = std::fs::read_to_string(&config_path)?;
            let mut config: ChasmConfig = toml::from_str(&content).map_err(|error| {
                anyhow::anyhow!("{} is not valid TOML: {error}", config_path.display())
            })?;
            config.migrate_network();
            let mut ids = std::collections::HashSet::new();
            for profile in &config.networks {
                profile.validate().map_err(anyhow::Error::msg)?;
                anyhow::ensure!(ids.insert(&profile.id), "Duplicate network profile id {:?}.", profile.id);
            }
            Ok(config)
        } else {
            Ok(ChasmConfig::default())
        }
    }

    /// Move the old single-network setting into a `[[networks]]` profile, so
    /// projects saved before network profiles keep their fork. The old fields
    /// are cleared so a save never writes them back next to the profile.
    fn migrate_network(&mut self) {
        if self.networks.is_empty() && !self.network.fork_url.is_empty() {
            self.networks.push(NetworkProfile {
                id: "fork-1".into(),
                name: String::new(),
                fork_url: std::mem::take(&mut self.network.fork_url),
                fork_url_env: String::new(),
                fork_block: self.network.fork_block.take(),
                chain_id_mode: ChainIdMode::Local,
                hardfork: String::new(),
                optimism: None,
            });
            self.network.active = "fork-1".into();
        }
    }

    /// The network the project boots with: the active profile with its URL
    /// resolved (env var applied), or the built-in local chain. An active id
    /// with no profile is refused, never silently replaced by local.
    pub fn resolve_network(&self) -> Result<NetworkSettings, String> {
        let active = self.network.active.trim();
        let Some(profile) = self.network.active_profile(&self.networks) else {
            if active.is_empty() || active == NetworkProfile::LOCAL_ID {
                return Ok(NetworkSettings::local());
            }
            return Err(format!("Active network profile {active:?} has no [[networks]] entry. Fix [network] active or add the profile."));
        };
        let url = profile.resolve_url()?;
        Ok(NetworkSettings {
            profile_id: profile.id.clone(),
            fork_url: url,
            fork_block: profile.fork_block,
            chain_id_mode: profile.chain_id_mode,
            hardfork: profile.hardfork.clone(),
            optimism: profile.optimism,
        })
    }

    /// The saved profile with this id, if any.
    pub fn profile(&self, id: &str) -> Option<&NetworkProfile> {
        self.networks.iter().find(|profile| profile.id == id)
    }

    /// Insert or replace a profile, keeping list order by id. Call
    /// `NetworkProfile::validate` first; this trusts its input.
    pub fn upsert_profile(&mut self, profile: NetworkProfile) {
        match self.networks.iter_mut().find(|saved| saved.id == profile.id) {
            Some(saved) => *saved = profile,
            None => self.networks.push(profile),
        }
    }

    /// Remove a profile; refuse the built-in local one and the active id.
    pub fn remove_profile(&mut self, id: &str) -> Result<(), String> {
        if id == NetworkProfile::LOCAL_ID {
            return Err("\"local\" is built in and cannot be removed.".into());
        }
        if self.network.active.trim() == id {
            return Err(format!("Profile {id} is active. Switch to another network first."));
        }
        let before = self.networks.len();
        self.networks.retain(|profile| profile.id != id);
        if self.networks.len() == before {
            return Err(format!("No profile with id {id}."));
        }
        Ok(())
    }

    /// Tulis config ke {root_dir}/.chasm/chasm.toml (buat direktori jika belum ada).
    /// The file is replaced atomically so a crash cannot leave it truncated.
    pub fn save(&self, root_dir: &Path) -> Result<()> {
        let chasm_dir = root_dir.join(".chasm");
        std::fs::create_dir_all(&chasm_dir)?;
        let config_path = chasm_dir.join("chasm.toml");
        let content = toml::to_string_pretty(self)?;
        let mut file = tempfile::NamedTempFile::new_in(&chasm_dir)?;
        std::io::Write::write_all(&mut file, content.as_bytes())?;
        file.as_file().sync_all()?;
        file.persist(&config_path).map_err(|error| error.error)?;
        Ok(())
    }

    /// Load, edit and save in one step. A file that cannot be parsed is left
    /// untouched and reported, instead of being replaced by the edit alone.
    pub fn update(root_dir: &Path, edit: impl FnOnce(&mut Self)) -> Result<()> {
        let mut config = Self::load(root_dir)?;
        edit(&mut config);
        config.save(root_dir)
    }

    /// Tambahkan remapping jika belum ada (hindari duplikat)
    pub fn add_remapping(&mut self, remapping: String) {
        // Cek berdasarkan prefix (bagian sebelum '=')
        let prefix = remapping.split('=').next().unwrap_or("").to_string();
        let already_exists = self
            .remappings
            .iter()
            .any(|r| r.split('=').next().unwrap_or("") == prefix);
        if !already_exists {
            self.remappings.push(remapping);
        }
    }

    #[cfg(test)]
    /// Ganti semua remapping OZ dengan yang baru (untuk switch versi)
    pub fn replace_oz_remapping(&mut self, new_remapping: String) {
        self.remappings.retain(|r| !r.contains("@openzeppelin"));
        self.remappings.push(new_remapping);
    }
}

/// Scan root_dir untuk instalasi openzeppelin-contracts.
/// Urutan pengecekan:
///   1. {root}/lib/openzeppelin-contracts
///   2. {root}/node_modules/@openzeppelin/contracts
///   3. Rekursif WalkDir max depth 4, cari folder "openzeppelin-contracts"
/// Mengembalikan path ke direktori root OZ (bukan /contracts subdir)
pub fn scan_openzeppelin(root_dir: &Path) -> Option<PathBuf> {
    // 1. Cek lib/openzeppelin-contracts (foundry-style)
    let lib_path = root_dir.join("lib").join("openzeppelin-contracts");
    if lib_path.exists() && lib_path.is_dir() {
        return Some(lib_path);
    }

    // 2. Cek node_modules/@openzeppelin/contracts (npm-style)
    let npm_path = root_dir
        .join("node_modules")
        .join("@openzeppelin")
        .join("contracts");
    if npm_path.exists() && npm_path.is_dir() {
        return Some(npm_path);
    }

    // 3. Rekursif scan max depth 4, cari folder bernama "openzeppelin-contracts"
    // Skip direktori yang besar/tidak relevan untuk menghindari scan lambat
    const SKIP_DIRS: &[&str] = &["node_modules", "target", ".git", ".chasm", "dist", "out", "cache", ".foundry"];

    for entry in WalkDir::new(root_dir)
        .max_depth(4)
        .into_iter()
        .filter_entry(|e| {
            // Skip direktori yang diketahui tidak relevan
            if e.file_type().is_dir() {
                let name = e.file_name().to_string_lossy();
                return !SKIP_DIRS.iter().any(|skip| *skip == name.as_ref());
            }
            true
        })
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_dir()
            && entry.file_name().to_string_lossy() == "openzeppelin-contracts"
        {
            return Some(entry.path().to_path_buf());
        }
    }

    None
}

/// Generate remapping string from OZ path.
/// Supports two layouts:
///   - Foundry-style: oz_path/contracts/ contains Solidity sources
///   - npm-style: oz_path/ contains Solidity sources directly
///
/// Always produces an absolute path so foundry-compilers passes it correctly
/// to solc regardless of working directory. Relative paths cause solc to ignore
/// the remapping silently.
pub fn generate_remapping(oz_path: &Path, _root_dir: &Path) -> String {
    // Determine source location
    let contracts_subdir = oz_path.join("contracts");
    let source_path = if contracts_subdir.exists() {
        contracts_subdir
    } else {
        oz_path.to_path_buf()
    };

    // Canonicalize to resolve symlinks (e.g. /var -> /private/var on macOS).
    // Fall back to the original path if canonicalize fails (e.g. path doesn't exist yet).
    let source_path = source_path.canonicalize().unwrap_or(source_path);

    // Always use absolute path — foundry-compilers/solc does not reliably resolve
    // relative remapping paths relative to the project root.
    // Strip Windows UNC prefix (\\?\) BEFORE replacing backslashes — if we replace
    // first, "\\?\" becomes "//?/" and the strip_prefix no longer matches.
    let raw = source_path.to_string_lossy();
    let raw = raw.strip_prefix(r"\\?\").unwrap_or(&raw);
    let abs = raw.replace('\\', "/");

    // Ensure trailing slash
    let abs = if abs.ends_with('/') { abs } else { format!("{}/", abs) };

    format!("@openzeppelin/contracts/={}", abs)
}


/// Path ke direktori global chasm lib: ~/.chasm/lib/
pub fn global_lib_dir() -> PathBuf {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".chasm").join("lib")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, text: &str) -> PathBuf {
        let path = root.join(".chasm").join("chasm.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn saving_remappings_keeps_settings_chasm_does_not_manage() {
        let root = tempfile::tempdir().unwrap();
        let path = write(
            root.path(),
            "remappings = [\"a/=lib/a/\"]\nsolc_version = \"0.8.24\"\n\n[profile.default]\noptimizer = true\n",
        );
        ChasmConfig::update(root.path(), |config| config.replace_oz_remapping("@openzeppelin/contracts/=oz/".into())).unwrap();
        let saved: toml::Table = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved["solc_version"].as_str(), Some("0.8.24"));
        assert_eq!(saved["profile"]["default"]["optimizer"].as_bool(), Some(true));
        let remappings = saved["remappings"].as_array().unwrap();
        assert_eq!(remappings.len(), 2);
    }

    #[test]
    fn an_unparseable_config_is_reported_and_never_overwritten() {
        let root = tempfile::tempdir().unwrap();
        let broken = "remappings = [\"a/=lib/a/\"\n";
        let path = write(root.path(), broken);
        assert!(ChasmConfig::load(root.path()).is_err());
        assert!(ChasmConfig::update(root.path(), |config| config.add_remapping("b/=b/".into())).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), broken);
        assert!(ChasmConfig::load(&root.path().join("missing")).unwrap().remappings.is_empty());
    }

    fn profile(id: &str, url: &str, block: Option<u64>) -> NetworkProfile {
        NetworkProfile {
            id: id.into(), name: format!("Profile {id}"), fork_url: url.into(), fork_url_env: String::new(),
            fork_block: block, chain_id_mode: ChainIdMode::Local, hardfork: String::new(), optimism: None,
        }
    }

    #[test]
    fn the_old_network_setting_migrates_into_a_profile() {
        let root = tempfile::tempdir().unwrap();
        let path = write(root.path(), "[network]\nfork_url = \"https://rpc.example/key\"\nfork_block = 21000000\n");
        let config = ChasmConfig::load(root.path()).unwrap();
        assert_eq!(config.network.active, "fork-1");
        assert!(config.network.fork_url.is_empty(), "the old field is cleared after migration");
        let settings = config.resolve_network().unwrap();
        assert_eq!(settings.profile_id, "fork-1");
        assert_eq!(settings.fork_url, "https://rpc.example/key");
        assert_eq!(settings.fork_block, Some(21_000_000));
        assert_eq!(settings.chain_id_mode, ChainIdMode::Local);
        assert!(settings.hardfork.is_empty());
        assert_eq!(settings.optimism, None);
        // Saving rewrites the old format; loading it again keeps the fork.
        config.save(root.path()).unwrap();
        let saved: toml::Table = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(saved.get("networks").is_some(), "{}", saved);
        assert_eq!(saved["network"]["active"].as_str(), Some("fork-1"));
        assert!(saved["network"].get("fork_url").is_none(), "the legacy field is not written back: {}", saved);
        let reloaded = ChasmConfig::load(root.path()).unwrap();
        assert_eq!(reloaded.resolve_network().unwrap(), settings);
    }

    #[test]
    fn a_config_without_network_boots_local() {
        let root = tempfile::tempdir().unwrap();
        let config = ChasmConfig::load(root.path()).unwrap();
        let settings = config.resolve_network().unwrap();
        assert!(settings.is_local() && settings.profile_id.is_empty());
        assert_eq!(settings.execution_chain_id(None), 31337);
        assert!(config.network.active_profile(&config.networks).is_none());
    }

    #[test]
    fn an_active_id_without_a_profile_is_refused() {
        let config = ChasmConfig { network: NetworkConfig { active: "mainnet".into(), ..Default::default() }, networks: vec![profile("sepolia", "https://s.example", None)], ..Default::default() };
        assert_eq!(config.resolve_network().unwrap_err(), "Active network profile \"mainnet\" has no [[networks]] entry. Fix [network] active or add the profile.");
    }

    #[test]
    fn disk_profiles_must_have_distinct_safe_directory_names() {
        let root = tempfile::tempdir().unwrap();
        for id in ["../other", "trailing ", "local"] {
            write(root.path(), &format!("[[networks]]\nid = {id:?}\n"));
            assert!(ChasmConfig::load(root.path()).is_err());
        }
        write(root.path(), "[[networks]]\nid = 'one'\n[[networks]]\nid = 'one'\n");
        assert!(ChasmConfig::load(root.path()).unwrap_err().to_string().contains("Duplicate"));
    }

    #[test]
    fn profiles_are_validated_field_by_field() {
        assert!(profile("mainnet", "https://rpc.example", None).validate().is_ok());
        assert!(profile("ok_id-2", "", None).validate().is_ok(), "a profile may also be local");
        assert!(profile("Local", "https://rpc.example", None).validate().is_err(), "uppercase ids are refused");
        assert!(profile("local", "https://rpc.example", None).validate().is_err());
        assert!(profile("x y", "https://rpc.example", None).validate().is_err());
        assert!(profile("", "https://rpc.example", None).validate().is_err());
        assert!(profile("mainnet", "ftp://rpc.example", None).validate().is_err());
        let both = NetworkProfile { fork_url_env: "MY_RPC".into(), ..profile("mainnet", "https://rpc.example", None) };
        assert!(both.validate().is_err(), "fork_url and fork_url_env exclude each other");
    }

    #[test]
    fn fork_url_env_resolves_from_the_environment_and_never_prints_it() {
        let name = "CHASM_TEST_FORK_URL_ENV";
        std::env::set_var(name, "https://rpc.example/secret-path");
        let profile = NetworkProfile { fork_url: String::new(), fork_url_env: name.into(), ..profile("mainnet", "", None) };
        assert_eq!(profile.resolve_url().unwrap(), "https://rpc.example/secret-path");
        std::env::remove_var(name);
        assert!(profile.resolve_url().unwrap_err().contains(name));
        assert!(!profile.resolve_url().unwrap_err().contains("secret-path"), "the value never reaches the error");
    }

    #[test]
    fn chain_id_modes_round_trip_through_toml() {
        let root = tempfile::tempdir().unwrap();
        let path = write(root.path(), "[[networks]]\nid = \"mainnet\"\nname = \"Ethereum mainnet\"\nfork_url = \"https://rpc.example\"\nchain_id_mode = \"source\"\n[network]\nactive = \"mainnet\"\n");
        let config = ChasmConfig::load(root.path()).unwrap();
        let settings = config.resolve_network().unwrap();
        assert_eq!(settings.chain_id_mode, ChainIdMode::Source);
        assert_eq!(settings.execution_chain_id(Some(1)), 1);
        config.save(root.path()).unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("chain_id_mode = \"source\""), "{saved}");
        assert_eq!(ChainIdMode::parse("source"), Some(ChainIdMode::Source));
        assert_eq!(ChainIdMode::parse("nope"), None);
    }

    #[test]
    fn fork_options_round_trip_through_toml() {
        let root = tempfile::tempdir().unwrap();
        let path = write(root.path(), "[network]\nactive = 'base'\n\n[[networks]]\nid = 'base'\nfork_url = 'https://rpc.example'\nhardfork = 'isthmus'\noptimism = true\n\n[[networks]]\nid = 'ethereum'\nfork_url = 'https://rpc.example'\nhardfork = 'cancun'\noptimism = false\n");
        let config = ChasmConfig::load(root.path()).unwrap();
        let settings = config.resolve_network().unwrap();
        assert_eq!(settings.hardfork, "isthmus");
        assert_eq!(settings.optimism, Some(true));
        config.save(root.path()).unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("optimism = false"));
        let reloaded = ChasmConfig::load(root.path()).unwrap();
        assert_eq!(reloaded.networks, config.networks);
        assert_eq!(reloaded.resolve_network().unwrap(), settings);
    }

    #[test]
    fn invalid_toml_fork_options_are_refused() {
        let root = tempfile::tempdir().unwrap();
        for (options, error) in [
            ("hardfork = 'unknown'", "hardfork"),
            ("hardfork = true", "hardfork"),
            ("optimism = 'true'", "optimism"),
            ("hardfork = 'cancun'\noptimism = true", "Optimism mode"),
            ("hardfork = 'isthmus'\noptimism = false", "Optimism mode"),
        ] {
            write(root.path(), &format!("[[networks]]\nid = 'fork'\nfork_url = 'https://rpc.example'\n{options}\n"));
            assert!(ChasmConfig::load(root.path()).unwrap_err().to_string().contains(error), "{options}");
        }
        write(root.path(), "[[networks]]\nid = 'fresh'\noptimism = true\n");
        assert!(ChasmConfig::load(root.path()).unwrap_err().to_string().contains("require a fork RPC"));
    }

    #[test]
    fn profiles_upsert_and_cannot_be_removed_while_active() {
        let mut config = ChasmConfig::default();
        config.upsert_profile(profile("mainnet", "https://a.example", None));
        config.upsert_profile(profile("sepolia", "https://b.example", None));
        config.upsert_profile(profile("mainnet", "https://c.example", Some(9)));
        assert_eq!(config.networks.len(), 2, "upsert replaces, never duplicates");
        assert_eq!(config.profile("mainnet").unwrap().fork_block, Some(9));
        config.network.active = "mainnet".into();
        assert_eq!(config.remove_profile("mainnet").unwrap_err(), "Profile mainnet is active. Switch to another network first.");
        config.network.active = "sepolia".into();
        config.remove_profile("mainnet").unwrap();
        assert!(config.profile("mainnet").is_none());
        assert_eq!(config.remove_profile("local").unwrap_err(), "\"local\" is built in and cannot be removed.");
        assert_eq!(config.remove_profile("ghost").unwrap_err(), "No profile with id ghost.");
    }
}
