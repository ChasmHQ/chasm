//! Verified source for contracts on a forked chain, from Sourcify (no API
//! key). Each lookup is cached in `.chasm/sources/<chain>/<address>.json`,
//! including "not verified", so a trace is only fetched once.
use super::trace_debug::Program;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const API: &str = "https://sourcify.dev/server/v2/contract";
/// Lookups per trace: a long trace can touch many contracts.
pub const MAX_LOOKUPS: usize = 12;

fn cache_path(workspace: &Path, chain: u64, address: &str) -> PathBuf {
    workspace.join(".chasm").join("sources").join(chain.to_string()).join(format!("{}.json", address.to_ascii_lowercase()))
}

fn valid_address(address: &str) -> bool {
    address.len() == 42 && address.starts_with("0x") && address[2..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// The cached Sourcify answer for one address: the contract, or
/// `{"verified": false}`. Fetched when not cached.
pub async fn lookup(client: &reqwest::Client, workspace: &Path, chain: u64, address: &str) -> Result<Value, String> {
    if !valid_address(address) {
        return Err("Not an address.".into());
    }
    let path = cache_path(workspace, chain, address);
    if let Ok(bytes) = std::fs::read(&path) {
        if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
            return Ok(value);
        }
    }
    let url = format!("{API}/{chain}/{address}?fields=runtimeBytecode,sources,sourceIds,compilation,abi,storageLayout");
    let response = client.get(&url).send().await.map_err(|error| format!("Sourcify is unreachable: {error}"))?;
    let value = match response.status().as_u16() {
        200 => {
            let body: Value = response.json().await.map_err(|error| format!("Sourcify returned invalid JSON: {error}"))?;
            json!({
                "verified": true,
                "match": body["match"],
                "name": body["compilation"]["name"],
                "fullyQualifiedName": body["compilation"]["fullyQualifiedName"],
                "compilerVersion": body["compilation"]["compilerVersion"],
                "runtime": body["runtimeBytecode"]["recompiledBytecode"],
                "immutableReferences": body["runtimeBytecode"]["immutableReferences"],
                "sourceMap": body["runtimeBytecode"]["sourceMap"],
                "sources": body["sources"],
                "sourceIds": body["sourceIds"],
                "abi": body["abi"],
                "storageLayout": body["storageLayout"],
            })
        }
        404 => json!({"verified": false}),
        status => return Err(format!("Sourcify answered HTTP {status} for {address}.")),
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&path, serde_json::to_vec(&value).unwrap_or_default());
    Ok(value)
}

/// A debugger program for a verified contract: the file that declares the
/// contract is the listing; steps in imported files show as `no source`.
pub fn program(value: &Value) -> Option<Program> {
    if value["verified"] != true {
        return None;
    }
    let qualified = value["fullyQualifiedName"].as_str()?;
    let (file, name) = qualified.rsplit_once(':')?;
    let source = value["sources"][file]["content"].as_str()?.replace("\r\n", "\n");
    let runtime = ethers::utils::hex::decode(value["runtime"].as_str()?.trim_start_matches("0x")).ok()?;
    let abi = serde_json::from_value::<ethers::abi::Abi>(value["abi"].clone()).ok();
    Some(Program {
        name: name.to_string(),
        runtime,
        source_map: value["sourceMap"].as_str()?.to_string(),
        source_path: format!("sourcify/{file}"),
        source,
        source_id: value["sourceIds"][file]["id"].as_u64().map(|id| id as u32),
        layout: value["storageLayout"].as_object().map(|_| value["storageLayout"].clone()),
        abi,
        ast: None,
        // Sourcify's recompiled code is compared with immutables zeroed.
        variable: value["immutableReferences"].as_object().map(|_| super::trace_debug::variable_ranges(&serde_json::json!({"immutableReferences": value["immutableReferences"]}))).unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verified_answer_becomes_a_program_and_unverified_does_not() {
        let value = json!({
            "verified": true,
            "fullyQualifiedName": "src/Token.sol:Token",
            "runtime": "0x6080",
            "sourceMap": "0:10:1:-",
            "sources": {"src/Token.sol": {"content": "contract Token {}\r\n"}, "lib/A.sol": {"content": ""}},
            "sourceIds": {"src/Token.sol": {"id": 1}, "lib/A.sol": {"id": 0}},
            "abi": [],
            "storageLayout": {"storage": [], "types": {}},
        });
        let program = program(&value).unwrap();
        assert_eq!(program.name, "Token");
        assert_eq!(program.source_id, Some(1));
        assert_eq!(program.runtime, vec![0x60, 0x80]);
        assert_eq!(program.source, "contract Token {}\n");
        assert!(program.layout.is_some());
        assert!(super::program(&json!({"verified": false})).is_none());
        assert!(!valid_address("0x12"));
        let workspace = tempfile::tempdir().unwrap();
        assert!(cache_path(workspace.path(), 1, "0xABc").ends_with("1/0xabc.json") || cache_path(workspace.path(), 1, "0xABc").ends_with("1\\0xabc.json"));
    }
}
