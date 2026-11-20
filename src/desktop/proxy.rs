//! Which code an address runs when it is a proxy. Checked when an address is
//! attached or imported, so the implementation's ABI is chosen knowingly:
//! EIP-1967 implementation and beacon slots, the older OpenZeppelin slot, and
//! EIP-1167 minimal clones. Calls still go to the attached address.
use serde_json::{json, Value};

/// keccak256("eip1967.proxy.implementation") - 1
pub const IMPLEMENTATION_SLOT: &str = "0x360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc";
/// keccak256("eip1967.proxy.beacon") - 1
pub const BEACON_SLOT: &str = "0xa3f0ad74e5423aebfd80d3ef4346578335a9a72aeaee59ff6cb3582b35133d50";
/// keccak256("org.zeppelinos.proxy.implementation"), before EIP-1967
pub const LEGACY_SLOT: &str = "0x7050c9e0f4ca769c69bd3a8ef740bc37934f8e2c036e5a723fd8ee048ed3f8c3";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proxy {
    /// "EIP-1967", "EIP-1967 beacon", "OpenZeppelin (legacy)" or "EIP-1167 clone".
    pub kind: &'static str,
    pub implementation: String,
    pub beacon: Option<String>,
}

impl Proxy {
    pub fn describe(&self, address: &str) -> String {
        let via = self.beacon.as_ref().map(|beacon| format!(" through beacon {beacon}")).unwrap_or_default();
        format!("{address} is a {} proxy{via}. It runs the code of {}. Calls go to {address}; use the implementation's ABI.", self.kind, self.implementation)
    }
}

/// Address in the low 20 bytes of a storage word, or None when zero.
fn word_address(word: &str) -> Option<String> {
    let digits = word.trim_start_matches("0x");
    if digits.len() < 40 || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let address = &digits[digits.len() - 40..];
    (!address.bytes().all(|byte| byte == b'0')).then(|| format!("0x{}", address.to_ascii_lowercase()))
}

/// The implementation of an EIP-1167 minimal clone, from its runtime code.
pub fn clone_target(code: &[u8]) -> Option<String> {
    const PREFIX: [u8; 10] = [0x36, 0x3d, 0x3d, 0x37, 0x3d, 0x3d, 0x3d, 0x36, 0x3d, 0x73];
    const SUFFIX: [u8; 15] = [0x5a, 0xf4, 0x3d, 0x82, 0x80, 0x3e, 0x90, 0x3d, 0x91, 0x60, 0x2b, 0x57, 0xfd, 0x5b, 0xf3];
    if code.len() != 45 || code[..10] != PREFIX || code[30..] != SUFFIX {
        return None;
    }
    Some(format!("0x{}", ethers::utils::hex::encode(&code[10..30])))
}

async fn rpc(client: &reqwest::Client, url: &str, method: &str, params: Value) -> Result<Value, String> {
    let value: Value = client
        .post(url)
        .json(&json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}))
        .send()
        .await
        .map_err(|error| format!("Could not reach the node: {error}"))?
        .json()
        .await
        .map_err(|error| format!("The node returned invalid JSON: {error}"))?;
    if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
        return Err(format!("The node refused {method}: {}", error["message"].as_str().unwrap_or("unknown error")));
    }
    Ok(value["result"].clone())
}

/// Reads the proxy slots and code of `address` at `block`. None when the
/// address is not a recognised proxy.
pub async fn detect(url: &str, address: &str, block: &str) -> Result<Option<Proxy>, String> {
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(10)).no_proxy().build().map_err(|error| error.to_string())?;
    let slot = |slot: &'static str| {
        let client = client.clone();
        async move { rpc(&client, url, "eth_getStorageAt", json!([address, slot, block])).await.map(|word| word.as_str().and_then(word_address)) }
    };
    if let Some(implementation) = slot(IMPLEMENTATION_SLOT).await? {
        return Ok(Some(Proxy { kind: "EIP-1967", implementation, beacon: None }));
    }
    if let Some(beacon) = slot(BEACON_SLOT).await? {
        // UpgradeableBeacon.implementation()
        let answer = rpc(&client, url, "eth_call", json!([{"to": beacon, "data": "0x5c60da1b"}, block])).await?;
        let implementation = answer.as_str().and_then(word_address).ok_or_else(|| format!("Beacon {beacon} did not return an implementation address."))?;
        return Ok(Some(Proxy { kind: "EIP-1967 beacon", implementation, beacon: Some(beacon) }));
    }
    if let Some(implementation) = slot(LEGACY_SLOT).await? {
        return Ok(Some(Proxy { kind: "OpenZeppelin (legacy)", implementation, beacon: None }));
    }
    let code = rpc(&client, url, "eth_getCode", json!([address, block])).await?;
    let code = code.as_str().and_then(|text| ethers::utils::hex::decode(text.trim_start_matches("0x")).ok()).unwrap_or_default();
    Ok(clone_target(&code).map(|implementation| Proxy { kind: "EIP-1167 clone", implementation, beacon: None }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_are_the_published_constants() {
        let minus_one = |label: &str| {
            let hash = ethers::types::U256::from_big_endian(&ethers::utils::keccak256(label.as_bytes())) - 1;
            format!("{hash:#066x}")
        };
        assert_eq!(IMPLEMENTATION_SLOT, minus_one("eip1967.proxy.implementation"));
        assert_eq!(BEACON_SLOT, minus_one("eip1967.proxy.beacon"));
        assert_eq!(LEGACY_SLOT, format!("0x{}", ethers::utils::hex::encode(ethers::utils::keccak256("org.zeppelinos.proxy.implementation"))));
    }

    #[test]
    fn words_and_clones_give_addresses() {
        assert_eq!(word_address("0x000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48").as_deref(), Some("0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"));
        assert_eq!(word_address(&format!("0x{}", "0".repeat(64))), None);
        assert_eq!(word_address("0x"), None);
        let target = "bebebebebebebebebebebebebebebebebebebebe";
        let code = ethers::utils::hex::decode(format!("363d3d373d3d3d363d73{target}5af43d82803e903d91602b57fd5bf3")).unwrap();
        assert_eq!(clone_target(&code).as_deref(), Some(format!("0x{target}").as_str()));
        assert_eq!(clone_target(&code[..44]), None);
        let proxy = Proxy { kind: "EIP-1967", implementation: "0xbb".into(), beacon: None };
        assert!(proxy.describe("0xaa").contains("Calls go to 0xaa"));
    }
}
