//! Verified contracts opened by address, without building them: the ABI,
//! source and storage layout come from Sourcify (cached under
//! `.chasm/sources/<chain>/`). A proxy is imported with its implementation's
//! ABI and bound to the proxy address. Imports are listed in
//! `.chasm/imports.json` and reloaded from the cache when the project opens.
use super::session::{ContractItem, FunctionInfo};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Import {
    pub chain: u64,
    /// Address the tab calls (the proxy, for a proxy).
    pub address: String,
    /// Address whose verified source supplies the ABI (the implementation).
    pub source: String,
    /// How the two relate, for the Details pane.
    pub note: String,
}

/// One imported contract, ready for the Contracts list.
pub struct Imported {
    pub import: Import,
    pub contract: ContractItem,
    pub layout: Option<Value>,
    pub origin: String,
}

fn list_path(workspace: &Path) -> PathBuf {
    workspace.join(".chasm").join("imports.json")
}

pub fn saved(workspace: &Path) -> Vec<Import> {
    std::fs::read(list_path(workspace)).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
}

pub fn save(workspace: &Path, imports: &[Import]) -> Result<(), String> {
    let path = list_path(workspace);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    }
    std::fs::write(&path, serde_json::to_vec_pretty(imports).unwrap_or_default()).map_err(|error| error.to_string())
}

/// A Contracts entry from a cached Sourcify answer. The name carries the
/// address so it never collides with a workspace contract of the same name.
pub fn from_answer(import: &Import, answer: &Value) -> Result<Imported, String> {
    if answer["verified"] != true {
        return Err(format!("{} is not verified on Sourcify for chain {}.", import.source, import.chain));
    }
    let base = answer["name"].as_str().filter(|name| !name.is_empty()).ok_or("Sourcify gave no contract name.")?;
    let abi_json = answer["abi"].clone();
    let abi: ethers::abi::Abi = serde_json::from_value(abi_json.clone()).map_err(|error| format!("Sourcify's ABI for {} does not parse: {error}", import.source))?;
    let functions = abi_json
        .as_array()
        .into_iter()
        .flatten()
        .filter(|entry| entry["type"] == "function")
        .filter_map(|entry| {
            let params: Vec<(String, String)> = entry["inputs"].as_array()?.iter().map(|input| (input["name"].as_str().unwrap_or("").to_string(), type_label(input))).collect();
            Some(FunctionInfo {
                name: entry["name"].as_str()?.to_string(),
                inputs: params.iter().map(|(name, kind)| format!("{kind} {name}").trim().to_string()).collect::<Vec<_>>().join(", "),
                mutability: entry["stateMutability"].as_str().unwrap_or("").to_string(),
                params,
            })
        })
        .collect();
    let short = &import.address[import.address.len() - 4..];
    let name = format!("{base}@{short}");
    let origin = format!(
        "Sourcify ({} match, solc {}) for {} on chain {}{}",
        answer["match"].as_str().unwrap_or("verified"),
        answer["compilerVersion"].as_str().unwrap_or("?"),
        import.source,
        import.chain,
        if import.note.is_empty() { String::new() } else { format!("; {}", import.note) }
    );
    Ok(Imported {
        import: import.clone(),
        contract: ContractItem {
            abi,
            name,
            kind: "src".into(),
            functions,
            bytecode: String::new(),
            constructor_sig: String::new(),
            constructor: None,
            constructor_error: Some("Imported from a verified address; it is attached, not deployed from here.".into()),
        },
        layout: answer["storageLayout"].as_object().map(|_| answer["storageLayout"].clone()),
        origin,
    })
}

/// Solidity type of an ABI parameter, with tuples spelled out.
fn type_label(param: &Value) -> String {
    let kind = param["type"].as_str().unwrap_or("");
    match kind.strip_prefix("tuple") {
        Some(suffix) => format!("({}){suffix}", param["components"].as_array().into_iter().flatten().map(type_label).collect::<Vec<_>>().join(",")),
        None => kind.to_string(),
    }
}

/// Imports reloaded from the Sourcify cache only: no network on open.
pub fn reload(workspace: &Path) -> Vec<Imported> {
    saved(workspace)
        .into_iter()
        .filter_map(|import| {
            let path = workspace.join(".chasm").join("sources").join(import.chain.to_string()).join(format!("{}.json", import.source));
            let answer: Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
            from_answer(&import, &answer).ok()
        })
        .collect()
}

/// Looks the address up: proxy first (on the workspace node), then Sourcify
/// for the code that actually runs.
pub async fn fetch(workspace: &Path, rpc: &str, chain: u64, address: &str) -> Result<Imported, String> {
    let address = address.trim().parse::<ethers::types::Address>().map_err(|_| "Enter an exact 20-byte address.".to_string())?;
    let address = format!("{address:#x}");
    let proxy = super::proxy::detect(rpc, &address, "latest").await?;
    let (source, note) = match &proxy {
        Some(proxy) => (proxy.implementation.clone(), proxy.describe(&address)),
        None => (address.clone(), String::new()),
    };
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(30)).build().map_err(|error| error.to_string())?;
    let answer = super::sourcify::lookup(&client, workspace, chain, &source).await?;
    from_answer(&Import { chain, address, source, note }, &answer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn answer() -> Value {
        json!({
            "verified": true, "match": "exact_match", "name": "Vault", "compilerVersion": "0.8.20+commit.a1b79de6",
            "abi": [
                {"type":"function","name":"deposit","stateMutability":"nonpayable","inputs":[{"name":"amount","type":"uint256"}],"outputs":[]},
                {"type":"function","name":"setRoute","stateMutability":"nonpayable","inputs":[{"name":"route","type":"tuple[]","components":[{"name":"to","type":"address"},{"name":"fee","type":"uint24"}]}],"outputs":[]},
                {"type":"event","name":"Deposit","inputs":[],"anonymous":false}
            ],
            "storageLayout": {"storage": [], "types": {}},
        })
    }

    #[test]
    fn a_verified_answer_becomes_a_named_contract_with_its_origin() {
        let import = Import { chain: 1, address: "0x00000000000000000000000000000000000000aa".into(), source: "0x00000000000000000000000000000000000000bb".into(), note: "proxy".into() };
        let imported = from_answer(&import, &answer()).unwrap();
        assert_eq!(imported.contract.name, "Vault@00aa");
        assert_eq!(imported.contract.functions.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), vec!["deposit", "setRoute"]);
        assert_eq!(imported.contract.functions[1].params[0].1, "(address,uint24)[]");
        assert!(imported.layout.is_some());
        assert!(imported.origin.contains("exact_match") && imported.origin.contains("0x00000000000000000000000000000000000000bb") && imported.origin.contains("proxy"));
        let unverified = from_answer(&import, &json!({"verified": false})).err().unwrap();
        assert!(unverified.contains("not verified"));
    }

    #[test]
    fn imports_reload_from_the_cache_without_the_network() {
        let dir = tempfile::tempdir().unwrap();
        let import = Import { chain: 1, address: "0x00000000000000000000000000000000000000aa".into(), source: "0x00000000000000000000000000000000000000aa".into(), note: String::new() };
        save(dir.path(), &[import.clone()]).unwrap();
        assert!(reload(dir.path()).is_empty(), "nothing cached yet");
        let cache = dir.path().join(".chasm/sources/1");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("0x00000000000000000000000000000000000000aa.json"), answer().to_string()).unwrap();
        let reloaded = reload(dir.path());
        assert_eq!(reloaded.len(), 1);
        assert_eq!(reloaded[0].import, import);
    }
}
