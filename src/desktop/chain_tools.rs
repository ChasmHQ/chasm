//! Local chain tools for the owned Anvil node: snapshots for the timeline and
//! cheatcodes (time, blocks, balance, nonce, storage, code). Every method here
//! changes only the local development chain.
use serde_json::{json, Value};

/// Anvil methods this module may call.
const ALLOWED: [&str; 10] = [
    "evm_snapshot",
    "evm_revert",
    "eth_blockNumber",
    "evm_increaseTime",
    "evm_setNextBlockTimestamp",
    "anvil_mine",
    "anvil_setBalance",
    "anvil_setNonce",
    "anvil_setStorageAt",
    "anvil_setCode",
];

pub async fn call(rpc: &str, method: &str, params: Value) -> Result<Value, String> {
    if !ALLOWED.contains(&method) {
        return Err(format!("{method} is not a chain tool."));
    }
    if !rpc.starts_with("http://127.0.0.1:") {
        return Err("Chain tools only run against the local node.".into());
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .no_proxy()
        .build()
        .map_err(|error| error.to_string())?;
    let value: Value = client
        .post(rpc)
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

/// Snapshot id and the block it was taken at.
pub async fn snapshot(rpc: &str) -> Result<(String, u64), String> {
    let id = call(rpc, "evm_snapshot", json!([])).await?;
    let id = id.as_str().ok_or("The node returned no snapshot id.")?.to_string();
    let block = call(rpc, "eth_blockNumber", json!([])).await?;
    let block = block.as_str().and_then(|text| u64::from_str_radix(text.trim_start_matches("0x"), 16).ok()).unwrap_or(0);
    Ok((id, block))
}

/// Anvil answers `true` when it rolled back; `false` for an unknown or
/// already-consumed snapshot.
pub async fn revert(rpc: &str, id: &str) -> Result<bool, String> {
    Ok(call(rpc, "evm_revert", json!([id])).await? == json!(true))
}

/// Snapshot ids are increasing hex quantities; reverting to one discards
/// every id after it.
pub fn snapshot_order(id: &str) -> u128 {
    u128::from_str_radix(id.trim_start_matches("0x"), 16).unwrap_or(0)
}

/// One cheatcode as entered in the form.
#[derive(Clone, Debug, Default)]
pub struct Cheat {
    /// time, timestamp, mine, deal, nonce, storage, code
    pub action: String,
    pub address: String,
    /// Seconds, timestamp, block count, ether amount, nonce, storage value or bytecode.
    pub value: String,
    pub slot: String,
}

/// Validated RPC calls for a cheat, and a label for the timeline.
pub fn plan(cheat: &Cheat) -> Result<(Vec<(&'static str, Value)>, String), String> {
    let address = || -> Result<String, String> {
        let text = cheat.address.trim();
        text.parse::<ethers::types::Address>().map_err(|_| "Enter a 20-byte 0x address.".to_string())?;
        Ok(text.to_ascii_lowercase())
    };
    let number = |text: &str, what: &str| -> Result<u64, String> {
        let text = text.trim();
        let parsed = match text.strip_prefix("0x") {
            Some(hex) => u64::from_str_radix(hex, 16).ok(),
            None => text.parse::<u64>().ok(),
        };
        parsed.ok_or_else(|| format!("Enter {what} as a whole number."))
    };
    let word = |text: &str, what: &str| -> Result<String, String> {
        let text = text.trim();
        let value = match text.strip_prefix("0x") {
            Some(hex) => ethers::types::U256::from_str_radix(hex, 16).ok(),
            None => ethers::types::U256::from_dec_str(text).ok(),
        }
        .ok_or_else(|| format!("Enter {what} as a decimal or 0x number."))?;
        Ok(format!("0x{:064x}", value))
    };
    Ok(match cheat.action.as_str() {
        "time" => {
            let seconds = number(&cheat.value, "the seconds")?;
            if seconds == 0 || seconds > 10 * 365 * 86_400 {
                return Err("Advance by 1 second to 10 years.".into());
            }
            (vec![("evm_increaseTime", json!([seconds])), ("anvil_mine", json!(["0x1"]))], format!("advance time {}", duration(seconds)))
        }
        "timestamp" => {
            let at = number(&cheat.value, "the Unix timestamp")?;
            (vec![("evm_setNextBlockTimestamp", json!([at])), ("anvil_mine", json!(["0x1"]))], format!("set timestamp {at}"))
        }
        "mine" => {
            let blocks = number(&cheat.value, "the block count")?;
            if blocks == 0 || blocks > 100_000 {
                return Err("Mine 1 to 100000 blocks.".into());
            }
            (vec![("anvil_mine", json!([format!("{blocks:#x}")]))], format!("mine {blocks} blocks"))
        }
        "deal" => {
            let target = address()?;
            let wei = super::request_form::to_wei(&cheat.value, 2)?;
            let wei = ethers::types::U256::from_dec_str(&wei).map_err(|_| "Amount is too large.")?;
            (vec![("anvil_setBalance", json!([target, format!("{wei:#x}")]))], format!("deal {} ETH to {}", cheat.value.trim(), short(&target)))
        }
        "nonce" => {
            let target = address()?;
            let nonce = number(&cheat.value, "the nonce")?;
            (vec![("anvil_setNonce", json!([target, format!("{nonce:#x}")]))], format!("set nonce {nonce} on {}", short(&target)))
        }
        "storage" => {
            let target = address()?;
            let slot = word(&cheat.slot, "the slot")?;
            let value = word(&cheat.value, "the value")?;
            (vec![("anvil_setStorageAt", json!([target, slot, value]))], format!("set slot {} on {}", cheat.slot.trim(), short(&target)))
        }
        "code" => {
            let target = address()?;
            let code = cheat.value.trim();
            let digits = code.strip_prefix("0x").ok_or("Bytecode starts with 0x.")?;
            if digits.len() % 2 != 0 || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) || digits.len() > 2 * 24_576 * 2 {
                return Err("Enter even-length 0x hex bytecode, at most 48 KiB.".into());
            }
            (vec![("anvil_setCode", json!([target, code]))], format!("set code ({} bytes) on {}", digits.len() / 2, short(&target)))
        }
        other => return Err(format!("Unknown cheat {other}.")),
    })
}

fn short(address: &str) -> String {
    if address.len() > 12 { format!("{}…{}", &address[..6], &address[address.len() - 4..]) } else { address.to_string() }
}

fn duration(seconds: u64) -> String {
    match seconds {
        s if s % 86_400 == 0 => format!("{} days", s / 86_400),
        s if s % 3600 == 0 => format!("{} hours", s / 3600),
        s if s % 60 == 0 => format!("{} minutes", s / 60),
        s => format!("{s} seconds"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cheats_validate_input_and_build_anvil_calls() {
        let cheat = |action: &str, address: &str, value: &str, slot: &str| Cheat { action: action.into(), address: address.into(), value: value.into(), slot: slot.into() };
        let (calls, label) = plan(&cheat("time", "", "86400", "")).unwrap();
        assert_eq!(calls[0], ("evm_increaseTime", json!([86400])));
        assert_eq!(label, "advance time 1 days");
        let account = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8";
        let (calls, _) = plan(&cheat("deal", account, "1.5", "")).unwrap();
        assert_eq!(calls[0].1[1], "0x14d1120d7b160000");
        let (calls, _) = plan(&cheat("storage", account, "42", "0x1")).unwrap();
        assert_eq!(calls[0].1[1], format!("0x{:064x}", 1));
        assert_eq!(calls[0].1[2], format!("0x{:064x}", 42));
        assert!(plan(&cheat("deal", "0x12", "1", "")).is_err());
        assert!(plan(&cheat("mine", "", "0", "")).is_err());
        assert!(plan(&cheat("code", account, "60", "")).is_err());
        assert!(plan(&cheat("rm", "", "", "")).is_err());
        assert_eq!(snapshot_order("0x1a"), 26);
    }
}
