use std::collections::BTreeMap;

use ethers::abi::{Abi, Function, ParamType, StateMutability};
use ethers::types::U256;
use ethers::utils::{hex, keccak256};
use serde_json::{json, Value};

use super::call_result;

const MAX_DEPTH: usize = 32;
const MAX_NODES: usize = 4096;
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_ABI_BYTES: usize = 256 * 1024;
const MAX_DECODE_BYTES: usize = 256 * 1024;
const MAX_DECODES: usize = 128;

struct Output {
    text: String,
    limit: usize,
}

impl Output {
    fn new(limit: usize) -> Self {
        Self {
            text: String::new(),
            limit,
        }
    }

    fn push(&mut self, value: &str) -> Result<(), String> {
        if value.len() > self.limit.saturating_sub(self.text.len()) {
            return Err("Trace output exceeds 1 MiB or the ABI signature limit.".into());
        }
        self.text.push_str(value);
        Ok(())
    }

    fn quoted(&mut self, value: &str) -> Result<(), String> {
        self.push("\"")?;
        for ch in value.chars() {
            match ch {
                '"' => self.push("\\\"")?,
                '\\' => self.push("\\\\")?,
                '\n' => self.push("\\n")?,
                '\r' => self.push("\\r")?,
                '\t' => self.push("\\t")?,
                ch if ch.is_control() => self.push(&format!("\\u{:04x}", ch as u32))?,
                ch => {
                    let mut bytes = [0; 4];
                    self.push(ch.encode_utf8(&mut bytes))?;
                }
            }
        }
        self.push("\"")
    }
}

struct FunctionInfo<'a> {
    signature: String,
    function: &'a Function,
}

struct AbiBudget {
    types: usize,
    text: usize,
    signatures: usize,
}

impl AbiBudget {
    fn text(&mut self, text: &str) -> Result<(), String> {
        if text.chars().any(char::is_control) || text.len() > self.text {
            return Err("Unsupported ABI: names exceed the limit or contain control characters.".into());
        }
        self.text -= text.len();
        Ok(())
    }

    fn kind(&mut self, kind: &ParamType, depth: usize) -> Result<(), String> {
        if depth > MAX_DEPTH || self.types == 0 {
            return Err("Unsupported ABI: type depth or element limit exceeded.".into());
        }
        self.types -= 1;
        match kind {
            ParamType::Uint(bits) | ParamType::Int(bits)
                if *bits < 8 || *bits > 256 || bits % 8 != 0 =>
            {
                Err("Unsupported ABI: invalid integer type.".into())
            }
            ParamType::FixedBytes(size) if *size == 0 || *size > 32 => {
                Err("Unsupported ABI: invalid fixed bytes type.".into())
            }
            ParamType::FixedArray(_, size) if *size == 0 || *size > MAX_NODES => {
                Err("Unsupported ABI: invalid fixed array type.".into())
            }
            ParamType::Array(inner) | ParamType::FixedArray(inner, _) => {
                self.kind(inner, depth + 1)
            }
            ParamType::Tuple(items) => {
                for item in items {
                    self.kind(item, depth + 1)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

fn type_text(out: &mut Output, kind: &ParamType) -> Result<(), String> {
    match kind {
        ParamType::Address => out.push("address"),
        ParamType::Bytes => out.push("bytes"),
        ParamType::FixedBytes(size) => out.push(&format!("bytes{size}")),
        ParamType::Int(bits) => out.push(&format!("int{bits}")),
        ParamType::Uint(bits) => out.push(&format!("uint{bits}")),
        ParamType::Bool => out.push("bool"),
        ParamType::String => out.push("string"),
        ParamType::Array(inner) => {
            type_text(out, inner)?;
            out.push("[]")
        }
        ParamType::FixedArray(inner, size) => {
            type_text(out, inner)?;
            out.push(&format!("[{size}]"))
        }
        ParamType::Tuple(items) => {
            out.push("(")?;
            for (index, item) in items.iter().enumerate() {
                if index != 0 {
                    out.push(",")?;
                }
                type_text(out, item)?;
            }
            out.push(")")
        }
    }
}

fn functions(abi: &Abi) -> Result<BTreeMap<[u8; 4], Option<FunctionInfo<'_>>>, String> {
    if abi.functions.len() > MAX_NODES || abi.errors.len() > MAX_NODES {
        return Err("Unsupported ABI: too many functions or errors.".into());
    }
    let mut budget = AbiBudget {
        types: MAX_NODES,
        text: MAX_ABI_BYTES,
        signatures: MAX_ABI_BYTES,
    };
    let mut index = BTreeMap::new();
    let mut count = 0usize;
    for function in abi.functions.values().flatten() {
        count += 1;
        if count > MAX_NODES || function.name.is_empty() {
            return Err("Unsupported ABI: too many functions or an empty function name.".into());
        }
        budget.text(&function.name)?;
        for param in function.inputs.iter().chain(&function.outputs) {
            budget.text(&param.name)?;
            budget.kind(&param.kind, 1)?;
        }
        let mut signature = Output::new(budget.signatures);
        signature.push(&function.name)?;
        signature.push("(")?;
        for (position, param) in function.inputs.iter().enumerate() {
            if position != 0 {
                signature.push(",")?;
            }
            type_text(&mut signature, &param.kind)?;
        }
        signature.push(")")?;
        budget.signatures -= signature.text.len();
        let hash = keccak256(signature.text.as_bytes());
        let selector = [hash[0], hash[1], hash[2], hash[3]];
        match index.entry(selector) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(Some(FunctionInfo {
                    signature: signature.text,
                    function,
                }));
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                entry.insert(None);
            }
        }
    }
    count = 0;
    for error in abi.errors.values().flatten() {
        count += 1;
        if count > MAX_NODES || error.name.is_empty() {
            return Err("Unsupported ABI: too many errors or an empty error name.".into());
        }
        budget.text(&error.name)?;
        for param in &error.inputs {
            budget.text(&param.name)?;
            budget.kind(&param.kind, 1)?;
        }
    }
    Ok(index)
}

fn address(text: &str) -> bool {
    let Some(digits) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) else {
        return false;
    };
    digits.len() == 40 && digits.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn quantity(value: &Value) -> Result<U256, String> {
    let text = match value {
        Value::String(text) => text.as_str(),
        Value::Number(number) => number.as_str(),
        _ => return Err("Malformed callTracer quantity.".into()),
    };
    if let Some(digits) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        if digits.is_empty() || digits.len() > 64 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("Malformed or oversized callTracer hex quantity.".into());
        }
        U256::from_str_radix(digits, 16)
            .map_err(|_| "Malformed or oversized callTracer hex quantity.".into())
    } else {
        if text.is_empty() || text.len() > 78 || !text.bytes().all(|b| b.is_ascii_digit()) {
            return Err("Malformed or oversized callTracer decimal quantity.".into());
        }
        U256::from_dec_str(text)
            .map_err(|_| "Malformed or oversized callTracer decimal quantity.".into())
    }
}

fn hex_data<'a>(value: &'a Value, out: &Output) -> Result<&'a str, String> {
    let text = value
        .as_str()
        .ok_or_else(|| "Malformed callTracer byte string.".to_owned())?;
    if text.len() > out.limit.saturating_sub(out.text.len()) {
        return Err("Trace output exceeds 1 MiB.".into());
    }
    let digits = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .ok_or_else(|| "Malformed callTracer byte string.".to_owned())?;
    if digits.len() % 2 != 0 || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Malformed callTracer byte string.".into());
    }
    Ok(text)
}

fn selector(text: &str) -> Option<[u8; 4]> {
    if text.len() < 10 {
        return None;
    }
    let bytes = hex::decode(&text[2..10]).ok()?;
    Some([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn field<'a>(value: &'a Value, name: &str) -> Result<Option<&'a str>, String> {
    match value.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(_) => Err(format!("Malformed callTracer {name} field.")),
    }
}

fn prefix(out: &mut Output, ancestors: &[bool], detail: bool) -> Result<(), String> {
    for (position, more) in ancestors.iter().enumerate() {
        if position + 1 == ancestors.len() && !detail {
            out.push(if *more { "├─ " } else { "└─ " })?;
        } else {
            out.push(if *more { "│  " } else { "   " })?;
        }
    }
    if detail {
        out.push("   ")?;
    }
    Ok(())
}

struct Renderer<'a> {
    out: Output,
    abi: &'a Abi,
    known_address: &'a str,
    functions: BTreeMap<[u8; 4], Option<FunctionInfo<'a>>>,
    nodes: usize,
    decodes: usize,
    error_function: Function,
}

impl Renderer<'_> {
    fn count_node(&mut self) -> Result<(), String> {
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            return Err("callTracer trace exceeds 4096 nodes.".into());
        }
        Ok(())
    }

    fn detail(&mut self, ancestors: &[bool], label: &str, value: &str) -> Result<(), String> {
        prefix(&mut self.out, ancestors, true)?;
        self.out.push(label)?;
        self.out.push(value)?;
        self.out.push("\n")
    }

    fn quoted_detail(&mut self, ancestors: &[bool], label: &str, value: &str) -> Result<(), String> {
        prefix(&mut self.out, ancestors, true)?;
        self.out.push(label)?;
        self.out.quoted(value)?;
        self.out.push("\n")
    }

    fn log(&mut self, log: &Value, ancestors: &[bool]) -> Result<(), String> {
        self.count_node()?;
        let object = log
            .as_object()
            .ok_or_else(|| "Malformed callTracer log.".to_owned())?;
        let emitter = object
            .get("address")
            .and_then(Value::as_str)
            .filter(|value| address(value))
            .ok_or_else(|| "Malformed callTracer log address.".to_owned())?;
        let topics = object
            .get("topics")
            .and_then(Value::as_array)
            .ok_or_else(|| "Malformed callTracer log topics.".to_owned())?;
        if topics.len() > 4 {
            return Err("Malformed callTracer log topics.".into());
        }
        self.detail(ancestors, "log address: ", emitter)?;
        for topic in topics {
            let topic = hex_data(topic, &self.out)?;
            if topic.len() != 66 {
                return Err("Malformed callTracer log topic.".into());
            }
            self.detail(ancestors, "log topic: ", topic)?;
        }
        let data = object
            .get("data")
            .ok_or_else(|| "Malformed callTracer log data.".to_owned())?;
        let data = hex_data(data, &self.out)?;
        self.detail(ancestors, "log data: ", data)
    }

    fn frame(&mut self, frame: &Value, ancestors: &mut Vec<bool>) -> Result<(), String> {
        if ancestors.len() + 1 > MAX_DEPTH {
            return Err("callTracer trace exceeds 32 levels.".into());
        }
        self.count_node()?;
        let object = frame
            .as_object()
            .ok_or_else(|| "Malformed callTracer frame: expected an object.".to_owned())?;
        if object.contains_key("structLogs") {
            return Err("Unsupported trace: opcode structLogs are not a callTracer tree.".into());
        }
        let kind = object
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| "Unsupported or malformed callTracer frame: missing call type.".to_owned())?;
        if !matches!(kind, "CALL" | "STATICCALL" | "DELEGATECALL" | "CALLCODE" | "CREATE" | "CREATE2" | "SELFDESTRUCT") {
            return Err("Unsupported callTracer frame type.".into());
        }
        let from = field(frame, "from")?
            .filter(|value| address(value))
            .ok_or_else(|| "Malformed callTracer frame: missing or invalid from address.".to_owned())?;
        let to = field(frame, "to")?;
        if to.is_some_and(|value| !address(value))
            || (to.is_none() && !matches!(kind, "CREATE" | "CREATE2"))
        {
            return Err("Malformed callTracer frame: missing or invalid target address.".into());
        }
        let known = to.is_some_and(|target| target.eq_ignore_ascii_case(self.known_address));
        let is_call = matches!(kind, "CALL" | "STATICCALL" | "DELEGATECALL" | "CALLCODE");
        let input = object.get("input").filter(|value| !value.is_null());
        let input = input.map(|value| hex_data(value, &self.out)).transpose()?;
        let input_selector = if is_call { input.and_then(selector) } else { None };
        prefix(&mut self.out, ancestors, false)?;
        self.out.push(kind)?;
        self.out.push(" from ")?;
        self.out.push(from)?;
        self.out.push(" to ")?;
        self.out.push(to.unwrap_or("<unavailable>"))?;
        for (key, label) in [("gas", " gas="), ("gasUsed", " gasUsed="), ("value", " value=")] {
            if let Some(value) = object.get(key).filter(|value| !value.is_null()) {
                self.out.push(label)?;
                self.out.push(&quantity(value)?.to_string())?;
            }
        }
        self.out.push("\n")?;
        if let Some(input) = input {
            if let Some(selector) = input_selector {
                prefix(&mut self.out, ancestors, true)?;
                match if known { self.functions.get(&selector) } else { None } {
                    Some(Some(info)) => {
                        self.out.push("function: ")?;
                        self.out.push(&info.signature)?;
                    }
                    Some(None) => self.out.push("ambiguous selector: ")?,
                    None => self.out.push("unknown selector: ")?,
                }
                self.out.push(" 0x")?;
                self.out.push(&hex::encode(selector))?;
                self.out.push("\n")?;
            } else if is_call {
                self.detail(ancestors, "function: ", "no 4-byte selector")?;
            }
            self.detail(ancestors, if is_call { "input: " } else { "init/input: " }, input)?;
        }
        let error = field(frame, "error")?;
        if let Some(error) = error {
            self.quoted_detail(ancestors, "error: ", error)?;
        }
        if let Some(reason) = field(frame, "revertReason")? {
            self.quoted_detail(ancestors, "revert reason: ", reason)?;
        }
        if let Some(value) = object.get("output").filter(|value| !value.is_null()) {
            let output = hex_data(value, &self.out)?;
            self.detail(ancestors, if error.is_some() { "revert/output: " } else { "output: " }, output)?;
            let has_match = known && input_selector
                .and_then(|selector| self.functions.get(&selector))
                .is_some_and(Option::is_some);
            if known && is_call && output.len() > 2 && (error.is_some() || has_match) {
                if self.decodes >= MAX_DECODES || output.len() > 2 + MAX_DECODE_BYTES * 2 {
                    self.detail(ancestors, "ABI decode: ", "skipped (decode limit)")?;
                } else {
                    self.decodes += 1;
                    let function = input_selector
                        .and_then(|selector| self.functions.get(&selector))
                        .and_then(Option::as_ref)
                        .map(|info| info.function)
                        .unwrap_or(&self.error_function);
                    let raw = if error.is_some() {
                        json!({"error": {"data": output}})
                    } else {
                        Value::String(output.to_owned())
                    };
                    let decoded = call_result::decode_call(function, self.abi, &raw);
                    self.quoted_detail(ancestors, "ABI interpretation: ", &decoded)?;
                }
            }
        }
        if let Some(logs) = object.get("logs").filter(|value| !value.is_null()) {
            let logs = logs
                .as_array()
                .ok_or_else(|| "Malformed callTracer logs.".to_owned())?;
            for log in logs {
                self.log(log, ancestors)?;
            }
        }
        if let Some(calls) = object.get("calls").filter(|value| !value.is_null()) {
            let calls = calls
                .as_array()
                .ok_or_else(|| "Malformed callTracer child calls.".to_owned())?;
            for (index, call) in calls.iter().enumerate() {
                ancestors.push(index + 1 < calls.len());
                self.frame(call, ancestors)?;
                ancestors.pop();
            }
        }
        Ok(())
    }
}

pub fn render(tree: &Value, abi: &Abi, known_address: &str) -> Result<String, String> {
    let object = tree
        .as_object()
        .ok_or_else(|| "Unsupported or malformed trace: expected a callTracer tree object.".to_owned())?;
    if object.contains_key("structLogs") {
        return Err("Unsupported trace: opcode structLogs are not a callTracer tree.".into());
    }
    if !object.contains_key("type") {
        return Err("Unsupported or malformed trace: missing callTracer type.".into());
    }
    if !address(known_address) {
        return Err("Invalid known contract address for trace rendering.".into());
    }
    #[allow(deprecated)]
    let error_function = Function {
        name: String::new(),
        inputs: Vec::new(),
        outputs: Vec::new(),
        constant: None,
        state_mutability: StateMutability::NonPayable,
    };
    let mut renderer = Renderer {
        out: Output::new(MAX_OUTPUT_BYTES),
        abi,
        known_address,
        functions: functions(abi)?,
        nodes: 0,
        decodes: 0,
        error_function,
    };
    renderer.frame(tree, &mut Vec::new())?;
    Ok(renderer.out.text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ethers::abi::{encode, Token};

    const KNOWN: &str = "0x1111111111111111111111111111111111111111";
    const OTHER: &str = "0x2222222222222222222222222222222222222222";
    const FROM: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn abi() -> Abi {
        serde_json::from_value(json!([
            {"type":"function","name":"balanceOf","stateMutability":"view",
             "inputs":[{"name":"owner","type":"address"}],
             "outputs":[{"name":"balance","type":"uint256"}]},
            {"type":"error","name":"Denied","inputs":[{"name":"amount","type":"uint256"}]}
        ])).unwrap()
    }

    fn call(target: &str, input: &str) -> Value {
        json!({"type":"CALL", "from":FROM, "to":target, "input":input})
    }

    #[test]
    fn rejects_scalar_struct_logs_and_malformed_frames() {
        assert!(render(&json!(1), &abi(), KNOWN).unwrap_err().contains("callTracer"));
        assert!(render(&json!({"structLogs": []}), &abi(), KNOWN).unwrap_err().contains("structLogs"));
        assert!(render(&json!({"type":"CALL"}), &abi(), KNOWN).unwrap_err().contains("from"));
        assert!(render(&json!({"type":"CALL", "from":FROM, "to":KNOWN, "calls":[null]}), &abi(), KNOWN).unwrap_err().contains("frame"));
    }

    #[test]
    fn only_decodes_selector_and_output_for_the_known_target() {
        let output = format!("0x{}", hex::encode(encode(&[Token::Uint(42.into())])));
        let mut root = call(KNOWN, "0x70a08231");
        root["output"] = json!(output);
        let children = json!([
            {"type":"STATICCALL", "from":KNOWN, "to":OTHER, "input":"0x70a08231", "output":root["output"].clone()},
            {"type":"CALL", "from":KNOWN, "to":KNOWN, "input":"0xdeadbeef"},
            {"type":"CREATE", "from":KNOWN, "input":"0x70a08231"}
        ]);
        root["calls"] = children;
        let text = render(&root, &abi(), KNOWN).unwrap();
        assert_eq!(text.matches("function: balanceOf(address)").count(), 1);
        assert_eq!(text.matches("unknown selector:").count(), 2);
        assert_eq!(text.matches("ABI interpretation:").count(), 1);
        assert!(text.contains("balance: 42"));
        assert!(text.contains("STATICCALL"));
        assert!(text.contains("CREATE"));
    }

    #[test]
    fn shows_raw_and_abi_decoded_reverts_only_for_known_target() {
        let abi = abi();
        let error = abi.errors.values().flatten().next().unwrap();
        let raw = format!("0x{}", hex::encode(error.encode(&[Token::Uint(9.into())]).unwrap()));
        let root = json!({"type":"CALL", "from":FROM, "to":KNOWN, "input":"0xdeadbeef",
            "error":"execution reverted", "output":raw,
            "calls":[{"type":"CALL", "from":KNOWN, "to":OTHER, "input":"0xdeadbeef", "error":"execution reverted", "output":raw}]});
        let text = render(&root, &abi, KNOWN).unwrap();
        assert_eq!(text.matches("Denied(amount: 9)").count(), 1);
        assert_eq!(text.matches("revert/output:").count(), 2);
        assert_eq!(text.matches(&raw).count(), 2);
    }

    #[test]
    fn gas_and_value_are_exact_u256() {
        let max = format!("0x{}", "f".repeat(64));
        let root = json!({"type":"CALL", "from":FROM, "to":KNOWN, "input":"0x", "gas":max, "gasUsed":"0x20000000000001", "value":max});
        let text = render(&root, &abi(), KNOWN).unwrap();
        let decimal = "115792089237316195423570985008687907853269984665640564039457584007913129639935";
        assert!(text.contains(&format!("gas={decimal}")));
        assert!(text.contains(&format!("value={decimal}")));
        assert!(text.contains("gasUsed=9007199254740993"));
    }

    #[test]
    fn limits_depth_nodes_and_output_bytes() {
        let mut nested = call(KNOWN, "0x");
        for _ in 1..MAX_DEPTH {
            nested = json!({"type":"CALL", "from":FROM, "to":KNOWN, "calls":[nested]});
        }
        assert!(render(&nested, &Abi::default(), KNOWN).is_ok());
        let too_deep = json!({"type":"CALL", "from":FROM, "to":KNOWN, "calls":[nested]});
        assert!(render(&too_deep, &Abi::default(), KNOWN).unwrap_err().contains("32 levels"));
        let mut root = call(KNOWN, "0x");
        root["calls"] = Value::Array(vec![call(KNOWN, "0x"); MAX_NODES - 1]);
        assert!(render(&root, &Abi::default(), KNOWN).is_ok());
        root["calls"].as_array_mut().unwrap().push(call(KNOWN, "0x"));
        assert!(render(&root, &Abi::default(), KNOWN).unwrap_err().contains("4096 nodes"));
        let huge = format!("0x{}", "ab".repeat(MAX_OUTPUT_BYTES / 2));
        assert!(render(&call(KNOWN, &huge), &Abi::default(), KNOWN).unwrap_err().contains("1 MiB"));
    }

    #[test]
    fn ambiguous_abi_selector_is_not_guessed() {
        let abi: Abi = serde_json::from_value(json!([
            {"type":"function","name":"burn","inputs":[{"name":"x","type":"uint256"}],"outputs":[]},
            {"type":"function","name":"collate_propagate_storage","inputs":[{"name":"x","type":"bytes16"}],"outputs":[]}
        ])).unwrap();
        let text = render(&call(KNOWN, "0x42966c68"), &abi, KNOWN).unwrap();
        assert!(text.contains("ambiguous selector"));
        assert!(!text.contains("function: burn"));
    }
}
