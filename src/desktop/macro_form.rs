//! Form-based macro step editing. Steps are edited in the selected draft's
//! JSON, so the JSON editor and the form always show the same definition.
//! Contract calls chosen from workspace contracts become raw call/send steps
//! with ABI-encoded calldata; they run without a Foundry build.
use ethers::abi::Function;
use serde_json::{json, Map, Value};

/// Step kinds the Add step menu offers, in menu order.
pub const KINDS: [(&str, &str); 7] = [
    ("call", "Call a contract function"),
    ("raw_send", "Send raw calldata"),
    ("raw_call", "Read with raw calldata"),
    ("storage_read", "Read a storage slot"),
    ("snapshot", "Take a snapshot"),
    ("revert", "Revert to a snapshot"),
    ("assert", "Check a value"),
];

pub const SIGNERS: [&str; 4] = ["anvil_account0", "anvil_account1", "anvil_account2", "unlocked_wallet_ref"];
pub const COMPARATORS: [&str; 7] = ["eq", "not_eq", "zero", "nonzero", "true", "false", "hash_eq"];

/// One editable field of the selected step.
#[derive(Clone, Debug, PartialEq)]
pub struct FormField {
    pub key: String,
    pub label: String,
    pub value: String,
    pub hint: String,
    /// "text", "signer", "comparator" or "reference".
    pub kind: String,
}

fn parse(draft: &str) -> Result<Map<String, Value>, String> {
    match serde_json::from_str::<Value>(draft) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err("The draft is not a JSON object.".into()),
        Err(error) => Err(format!("Fix the JSON first: {error}")),
    }
}

fn steps(map: &Map<String, Value>) -> Vec<Value> {
    map.get("steps").and_then(Value::as_array).cloned().unwrap_or_default()
}

fn write(mut map: Map<String, Value>, steps: Vec<Value>) -> String {
    map.insert("steps".into(), Value::Array(steps));
    serde_json::to_string_pretty(&Value::Object(map)).unwrap_or_default()
}

fn unique_id(steps: &[Value], base: &str) -> String {
    let taken = |id: &str| steps.iter().any(|step| step["id"] == id);
    if !taken(base) {
        return base.to_string();
    }
    (2..).map(|n| format!("{base}_{n}")).find(|id| !taken(id)).unwrap()
}

/// Insert a new step of `kind` after `after` (or at the end) and return the
/// new draft and the new step's index.
pub fn add_step(draft: &str, kind: &str, after: Option<usize>) -> Result<(String, usize), String> {
    let map = parse(draft)?;
    let mut list = steps(&map);
    if list.len() >= 128 {
        return Err("A macro can hold at most 128 steps.".into());
    }
    let last_snapshot = list.iter().rev().find(|step| step["kind"]["type"] == "snapshot").and_then(|step| step["id"].as_str()).map(str::to_string);
    let last_deploy_or_target = list.iter().rev().find_map(|step| step["kind"]["target"].as_str().map(str::to_string)).unwrap_or_default();
    let (base, body) = match kind {
        "call" | "raw_send" => ("send", json!({"type":"raw_send","target":last_deploy_or_target,"data":"0x","signer":"anvil_account0","value":"0","gas_limit":"300000"})),
        "raw_call" => ("read", json!({"type":"raw_call","target":last_deploy_or_target,"data":"0x","signer":"anvil_account0","value":"0","gas_limit":"300000"})),
        "storage_read" => ("slot", json!({"type":"storage_read","target":last_deploy_or_target,"slot":"0"})),
        "snapshot" => ("snapshot", json!({"type":"snapshot","probes":[]})),
        "revert" => ("restore", json!({"type":"revert","snapshot": last_snapshot.map(|id| format!("${{step.{id}.snapshotId}}")).unwrap_or_default()})),
        "assert" => ("check", json!({"type":"assert","left":"","comparator":"nonzero"})),
        _ => return Err("Unknown step kind.".into()),
    };
    let id = unique_id(&list, base);
    let at = after.map(|index| (index + 1).min(list.len())).unwrap_or(list.len());
    list.insert(at, json!({"id": id, "enabled": true, "kind": body}));
    Ok((write(map, list), at))
}

pub fn remove_step(draft: &str, index: usize) -> Result<String, String> {
    let map = parse(draft)?;
    let mut list = steps(&map);
    if index >= list.len() {
        return Err("No such step.".into());
    }
    list.remove(index);
    Ok(write(map, list))
}

/// Move a step up (-1) or down (+1); returns the new draft and index.
pub fn move_step(draft: &str, index: usize, delta: i32) -> Result<(String, usize), String> {
    let map = parse(draft)?;
    let mut list = steps(&map);
    let target = index as i64 + delta as i64;
    if index >= list.len() || target < 0 || target as usize >= list.len() {
        return Err("The step cannot move further.".into());
    }
    list.swap(index, target as usize);
    Ok((write(map, list), target as usize))
}

pub fn toggle_step(draft: &str, index: usize) -> Result<String, String> {
    let map = parse(draft)?;
    let mut list = steps(&map);
    let step = list.get_mut(index).ok_or("No such step.")?;
    let enabled = step["enabled"].as_bool().unwrap_or(true);
    step["enabled"] = json!(!enabled);
    Ok(write(map, list))
}

/// Set one field of a step: `id` renames the step, everything else is a kind field.
pub fn set_field(draft: &str, index: usize, key: &str, value: &str) -> Result<String, String> {
    let map = parse(draft)?;
    let mut list = steps(&map);
    let step = list.get_mut(index).ok_or("No such step.")?;
    if key == "id" {
        step["id"] = json!(value);
    } else if key == "millis" {
        step["kind"][key] = json!(value.parse::<u64>().unwrap_or(0));
    } else if key == "right" {
        if value.is_empty() {
            if let Some(kind) = step["kind"].as_object_mut() {
                kind.remove("right");
            }
        } else {
            step["kind"]["right"] = json!(value);
        }
    } else {
        step["kind"][key] = json!(value);
    }
    Ok(write(map, list))
}

/// Encode a workspace function call into the step's calldata and mode.
/// `send` chooses raw_send (a transaction) over raw_call (a read).
pub fn set_call(draft: &str, index: usize, function: &Function, arguments: &str, send: bool) -> Result<String, String> {
    let values = super::request_form::split(function, arguments);
    let mut budget = 4096;
    let tokens = function
        .inputs
        .iter()
        .zip(&values)
        .enumerate()
        .map(|(position, (param, text))| {
            super::deployment::parse_field(&param.kind, text, &mut budget).map_err(|error| format!("{}: {error}", if param.name.is_empty() { format!("argument {}", position + 1) } else { param.name.clone() }))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let data = function.encode_input(&tokens).map_err(|_| "Could not encode the arguments.".to_string())?;
    let map = parse(draft)?;
    let mut list = steps(&map);
    let step = list.get_mut(index).ok_or("No such step.")?;
    step["kind"]["type"] = json!(if send { "raw_send" } else { "raw_call" });
    step["kind"]["data"] = json!(format!("0x{}", ethers::utils::hex::encode(data)));
    if step["kind"]["gas_limit"].as_str().unwrap_or("").is_empty() {
        step["kind"]["gas_limit"] = json!("300000");
    }
    for key in ["signer", "value"] {
        if step["kind"].get(key).is_none() {
            step["kind"][key] = json!(if key == "signer" { "anvil_account0" } else { "0" });
        }
    }
    Ok(write(map, list))
}

/// The selected step's editable fields, from the draft.
pub fn fields(draft: &str, index: usize) -> Vec<FormField> {
    let Ok(map) = parse(draft) else { return Vec::new() };
    let list = steps(&map);
    let Some(step) = list.get(index) else { return Vec::new() };
    let kind = &step["kind"];
    let text = |key: &str| kind.get(key).map(|value| match value { Value::String(text) => text.clone(), Value::Null => String::new(), other => other.to_string() }).unwrap_or_default();
    let field = |key: &str, label: &str, hint: &str, form: &str| FormField { key: key.into(), label: label.into(), value: if key == "id" { step["id"].as_str().unwrap_or("").into() } else { text(key) }, hint: hint.into(), kind: form.into() };
    let mut out = vec![field("id", "Step name", "Letters, digits and _; later steps refer to it as ${step.NAME.field}.", "text")];
    match kind["type"].as_str().unwrap_or("") {
        "raw_send" | "raw_call" => {
            out.push(field("target", "Contract address", "0x address or a reference such as ${step.deploy.contractAddress}.", "reference"));
            out.push(field("data", "Calldata", "Use Encode call below to build it from a workspace function.", "text"));
            out.push(field("signer", "Sender", "", "signer"));
            if kind["type"] == "raw_send" {
                out.push(field("value", "Value (wei)", "Decimal wei sent with the call.", "text"));
            }
            out.push(field("gas_limit", "Gas limit", "Exact gas for the transaction.", "text"));
        }
        "storage_read" => {
            out.push(field("target", "Contract address", "0x address or a reference.", "reference"));
            out.push(field("slot", "Slot", "Decimal or 0x slot number.", "text"));
        }
        "revert" => out.push(field("snapshot", "Snapshot", "Reference to an earlier snapshot step, e.g. ${step.snapshot.snapshotId}.", "reference")),
        "assert" => {
            out.push(field("left", "Value to check", "A reference such as ${step.read.returnData}.", "reference"));
            out.push(field("comparator", "Comparison", "", "comparator"));
            if !matches!(kind["comparator"].as_str(), Some("zero" | "nonzero" | "true" | "false")) {
                out.push(field("right", "Expected", "Value or reference to compare with.", "reference"));
            }
        }
        "delay" => out.push(field("millis", "Milliseconds", "", "text")),
        "abi_call" | "deploy_artifact" => {
            out.push(field("target", "Target", "Foundry artifact step; edit details in JSON.", "reference"));
            out.push(field("signature", "Function", "", "text"));
        }
        _ => {}
    }
    out
}

/// `${...}` references available to the step at `index`: outputs of earlier steps and accounts.
pub fn references(draft: &str, index: usize) -> Vec<String> {
    let Ok(map) = parse(draft) else { return Vec::new() };
    let mut out = Vec::new();
    for step in steps(&map).iter().take(index) {
        let id = step["id"].as_str().unwrap_or_default();
        let fields: &[&str] = match step["kind"]["type"].as_str().unwrap_or("") {
            "deploy_artifact" => &["contractAddress", "txHash"],
            "raw_send" => &["txHash", "blockNumber", "gasUsed"],
            "raw_call" => &["returnData"],
            "storage_read" => &["storageValue"],
            "snapshot" => &["snapshotId"],
            "assert" | "revert" => &["passed"],
            "abi_call" => &["returnData", "txHash"],
            _ => &[],
        };
        out.extend(fields.iter().map(|field| format!("${{step.{id}.{field}}}")));
    }
    out.extend((0..3).map(|account| format!("${{account.{account}.address}}")));
    out
}

pub fn summary(step: &Value) -> String {
    let kind = &step["kind"];
    let short = |text: &str| if text.len() > 18 { format!("{}…{}", &text[..8], &text[text.len() - 4..]) } else { text.to_string() };
    match kind["type"].as_str().unwrap_or("") {
        "raw_send" | "raw_call" => {
            let data = kind["data"].as_str().unwrap_or("0x");
            format!("{} {}", short(kind["target"].as_str().unwrap_or("")), if data.len() >= 10 { &data[..10] } else { data })
        }
        "storage_read" => format!("{} slot {}", short(kind["target"].as_str().unwrap_or("")), kind["slot"].as_str().unwrap_or("")),
        "revert" => kind["snapshot"].as_str().unwrap_or("").to_string(),
        "assert" => format!("{} {}", kind["left"].as_str().unwrap_or(""), kind["comparator"].as_str().unwrap_or("")),
        _ => step["id"].as_str().unwrap_or("").to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ethers::abi::{Param, ParamType, StateMutability};

    const EMPTY: &str = r#"{"schemaVersion":1,"id":"macro_x","name":"M","description":"","steps":[]}"#;

    #[test]
    fn steps_are_added_after_selection_named_uniquely_and_linked() {
        let (draft, at) = add_step(EMPTY, "snapshot", None).unwrap();
        assert_eq!(at, 0);
        let (draft, at) = add_step(&draft, "call", Some(0)).unwrap();
        assert_eq!(at, 1);
        let (draft, at) = add_step(&draft, "call", Some(1)).unwrap();
        let (draft, _) = add_step(&draft, "revert", Some(at)).unwrap();
        let value: Value = serde_json::from_str(&draft).unwrap();
        let ids: Vec<_> = value["steps"].as_array().unwrap().iter().map(|step| step["id"].as_str().unwrap().to_string()).collect();
        assert_eq!(ids, ["snapshot", "send", "send_2", "restore"]);
        assert_eq!(value["steps"][3]["kind"]["snapshot"], "${step.snapshot.snapshotId}");
        let (moved, at) = move_step(&draft, 2, -1).unwrap();
        assert_eq!(at, 1);
        assert!(moved.find("send_2").unwrap() < moved.find("\"send\"").unwrap());
        assert!(remove_step(&draft, 9).is_err());
        assert!(add_step("not json", "call", None).is_err());
    }

    #[test]
    fn call_encoding_and_fields_follow_the_step_kind() {
        let (draft, _) = add_step(EMPTY, "call", None).unwrap();
        #[allow(deprecated)]
        let function = Function {
            name: "withdraw".into(),
            inputs: vec![Param { name: "_amount".into(), kind: ParamType::Uint(256), internal_type: None }],
            outputs: vec![],
            constant: None,
            state_mutability: StateMutability::NonPayable,
        };
        let draft = set_field(&draft, 0, "target", "0x5fbdb2315678afecb367f032d93f642f64180aa3").unwrap();
        let draft = set_call(&draft, 0, &function, r#"["1000000000000000000"]"#, true).unwrap();
        let value: Value = serde_json::from_str(&draft).unwrap();
        assert_eq!(value["steps"][0]["kind"]["data"], "0x2e1a7d4d0000000000000000000000000000000000000000000000000de0b6b3a7640000");
        assert!(set_call(&draft, 0, &function, r#"["oops"]"#, true).is_err());
        let keys: Vec<_> = fields(&draft, 0).into_iter().map(|field| field.key).collect();
        assert_eq!(keys, ["id", "target", "data", "signer", "value", "gas_limit"]);
        let (draft, _) = add_step(&draft, "assert", None).unwrap();
        assert!(references(&draft, 1).is_empty() || references(&draft, 1).iter().all(|item| item.starts_with("${")));
        assert!(references(&draft, 1).contains(&"${account.0.address}".to_string()));
        let draft = set_field(&draft, 1, "comparator", "eq").unwrap();
        assert!(fields(&draft, 1).iter().any(|field| field.key == "right"));
    }
}
