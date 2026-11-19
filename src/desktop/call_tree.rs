//! The external call tree of a recorded trace. Each CALL, STATICCALL,
//! DELEGATECALL, CALLCODE, CREATE or CREATE2 that entered code is one frame,
//! with its target, selector, value, gas, outcome and step range. Frames are
//! named and decoded with every ABI Chasm knows: the matched program's, an
//! ABI attached to the address in a contract tab, then any workspace ABI.
use super::trace_debug::Session;
use ethers::abi::{Abi, Function, FunctionExt};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub depth: u64,
    /// CALL, STATICCALL, DELEGATECALL, CALLCODE, CREATE, CREATE2, or TX.
    pub kind: String,
    /// Account whose code runs (for DELEGATECALL, the library).
    pub address: String,
    pub input: Vec<u8>,
    pub value: Option<ethers::types::U256>,
    pub gas: u64,
    /// First and last step of the frame, inclusive.
    pub first: usize,
    pub last: usize,
    /// "returned", "reverted", "stopped", "out of gas" or "running" (cut trace).
    pub outcome: String,
    /// Index of the parent frame, None for the top frame.
    pub parent: Option<usize>,
}

/// ABIs for decoding: by address first (attached or matched), then by name.
#[derive(Clone, Debug, Default)]
pub struct Abis {
    pub by_address: HashMap<String, (String, Abi)>,
    pub named: Vec<(String, Abi)>,
}

impl Abis {
    /// Contract name and function for a selector at an address. Without an
    /// ABI for the address, a selector several contracts declare is not
    /// attributed to any of them; see `candidates`.
    pub fn function(&self, address: &str, selector: &[u8]) -> Option<(String, Function)> {
        let find = |abi: &Abi| abi.functions().find(|function: &&Function| function.selector() == selector).cloned();
        if let Some((name, abi)) = self.by_address.get(&address.to_ascii_lowercase()) {
            if let Some(function) = find(abi) {
                return Some((name.clone(), function));
            }
        }
        let mut found = self.named.iter().filter_map(|(name, abi)| find(abi).map(|function| (name.clone(), function)));
        let first = found.next()?;
        found.next().is_none().then_some(first)
    }

    /// Every known contract declaring a selector, as `Contract.function`.
    pub fn candidates(&self, selector: &[u8]) -> Vec<String> {
        self.named
            .iter()
            .filter_map(|(name, abi)| abi.functions().find(|function: &&Function| function.selector() == selector).map(|function| format!("{name}.{}", function.name)))
            .collect()
    }
}

fn word(text: &str) -> [u8; 32] {
    let digits = format!("{:0>64}", text.trim_start_matches("0x"));
    let mut out = [0u8; 32];
    if let Ok(bytes) = ethers::utils::hex::decode(&digits[digits.len() - 64..]) {
        out.copy_from_slice(&bytes);
    }
    out
}

fn number(text: &str) -> ethers::types::U256 {
    ethers::types::U256::from_big_endian(&word(text))
}

fn memory_slice(memory: &[String], offset: ethers::types::U256, length: ethers::types::U256) -> Vec<u8> {
    if offset > ethers::types::U256::from(1u64 << 24) || length > ethers::types::U256::from(64 * 1024) {
        return Vec::new();
    }
    let (offset, length) = (offset.as_usize(), length.as_usize());
    let flat: Vec<u8> = memory.iter().flat_map(|text| word(text)).collect();
    (offset..offset + length).map(|at| flat.get(at).copied().unwrap_or(0)).collect()
}

/// Builds the frame list in step order (parents before children).
pub fn frames(session: &Session) -> Vec<Frame> {
    let steps = &session.steps;
    if steps.is_empty() {
        return Vec::new();
    }
    let mut frames = vec![Frame {
        depth: steps[0].depth,
        kind: "TX".into(),
        address: steps[0].address.clone(),
        input: session.calldata.clone(),
        value: None,
        gas: steps[0].gas,
        first: 0,
        last: steps.len() - 1,
        outcome: "running".into(),
        parent: None,
    }];
    let mut open: Vec<usize> = vec![0];
    for index in 0..steps.len() {
        let step = &steps[index];
        // Leaving frames: the depth dropped below theirs.
        while open.len() > 1 && frames[*open.last().unwrap()].depth > step.depth {
            open.pop();
        }
        let next = steps.get(index + 1);
        let enters = next.is_some_and(|next| next.depth > step.depth);
        if enters {
            let stack = &step.stack;
            let top = |from_top: usize| stack.get(stack.len().wrapping_sub(1 + from_top)).map(String::as_str).unwrap_or("0x0");
            let (input, value) = match step.op.as_str() {
                "CALL" | "CALLCODE" => (memory_slice(&step.memory, number(top(3)), number(top(4))), Some(number(top(2)))),
                "DELEGATECALL" | "STATICCALL" => (memory_slice(&step.memory, number(top(2)), number(top(3))), None),
                "CREATE" | "CREATE2" => (Vec::new(), Some(number(top(0)))),
                _ => (Vec::new(), None),
            };
            let parent = *open.last().unwrap();
            frames.push(Frame {
                depth: next.unwrap().depth,
                kind: step.op.clone(),
                address: next.unwrap().address.clone(),
                input,
                value,
                gas: next.unwrap().gas,
                first: index + 1,
                last: index + 1,
                outcome: "running".into(),
                parent: Some(parent),
            });
            open.push(frames.len() - 1);
            continue;
        }
        let current = *open.last().unwrap();
        frames[current].last = index;
        let leaves = next.is_none_or(|next| next.depth < step.depth);
        if leaves {
            frames[current].outcome = match step.op.as_str() {
                "RETURN" => "returned",
                "REVERT" => "reverted",
                "STOP" | "SELFDESTRUCT" => "stopped",
                "INVALID" => "invalid opcode",
                _ if next.is_some() => "out of gas",
                _ => "running",
            }
            .into();
        }
    }
    // A frame that ended where its child ended still owns the child's steps.
    for at in (1..frames.len()).rev() {
        if let Some(parent) = frames[at].parent {
            frames[parent].last = frames[parent].last.max(frames[at].last);
        }
    }
    if session.truncated {
        frames[0].outcome = "cut at the step limit".into();
    } else {
        // The transaction's own result is what the top frame did.
        frames[0].outcome = if session.failed { "reverted" } else { "returned" }.into();
    }
    frames
}

/// The frame running at `step`: the deepest frame whose range holds it.
pub fn frame_at(frames: &[Frame], step: usize) -> Option<usize> {
    frames.iter().enumerate().filter(|(_, frame)| frame.first <= step && step <= frame.last).max_by_key(|(_, frame)| frame.depth).map(|(index, _)| index)
}

/// `Contract.function` for a frame, decoded with the known ABIs.
pub fn name(session: &Session, frame: &Frame, abis: &Abis) -> String {
    let program = session.mapped.get(&frame.address).map(|index| session.programs[*index].name.clone());
    let short = if frame.address.len() > 12 { format!("{}…{}", &frame.address[..6], &frame.address[frame.address.len() - 4..]) } else { frame.address.clone() };
    if frame.kind.starts_with("CREATE") {
        return format!("new {}", program.unwrap_or_else(|| "contract".into()));
    }
    let selector = frame.input.get(..4);
    let function = selector.and_then(|selector| abis.function(&frame.address, selector));
    match (program, function, selector) {
        (Some(name), Some((_, function)), _) => format!("{name}.{}", function.name),
        (None, Some((abi_name, function)), _) => format!("{abi_name}.{} at {short}", function.name),
        (Some(name), None, Some(selector)) => format!("{name}.0x{}", ethers::utils::hex::encode(selector)),
        (Some(name), None, None) => format!("{name} (no calldata)"),
        (None, None, Some(selector)) => {
            let candidates = abis.candidates(selector);
            let hint = if candidates.is_empty() { String::new() } else { format!(" (one of {}; attach the address to choose)", candidates.join(", ")) };
            format!("{short}.0x{}{hint}", ethers::utils::hex::encode(selector))
        }
        (None, None, None) => format!("{short} (no calldata)"),
    }
}

/// Decoded arguments of a frame's call: (signature, [(name, value)]).
pub fn arguments(frame: &Frame, abis: &Abis) -> Option<(String, Vec<(String, String)>)> {
    let selector = frame.input.get(..4)?;
    let (_, function) = abis.function(&frame.address, selector)?;
    let tokens = function.decode_input(&frame.input[4..]).ok()?;
    let args = function
        .inputs
        .iter()
        .zip(tokens)
        .map(|(param, token)| (if param.name.is_empty() { param.kind.to_string() } else { param.name.clone() }, super::trace_debug::token_text(&token)))
        .collect();
    Some((function.abi_signature(), args))
}

/// One line per frame, indented by depth, for the Calls tab and console.
pub fn describe(session: &Session, frame: &Frame, abis: &Abis) -> String {
    let value = frame.value.filter(|value| !value.is_zero()).map(|value| format!(" value {value}")).unwrap_or_default();
    let kind = if frame.kind == "TX" || frame.kind == "CALL" { String::new() } else { format!("{} ", frame.kind.to_ascii_lowercase()) };
    format!("{kind}{}{value} → {} (steps {}–{})", name(session, frame, abis), frame.outcome, frame.first + 1, frame.last + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn session() -> Session {
        // TX at depth 1 STATICCALLs 0xbb with transfer(...) selector, which reverts; then the TX reverts.
        let mut call_memory = vec!["a9059cbb00000000000000000000000000000000000000000000000000000000".to_string()];
        call_memory.push("0".repeat(64));
        call_memory.push("0".repeat(64));
        let logs = vec![
            json!({"pc":0,"op":"PUSH1","gas":1000,"gasCost":3,"depth":1,"stack":[],"memory":[]}),
            // STATICCALL stack (bottom..top): retLen retOff argsLen argsOff to gas
            json!({"pc":2,"op":"STATICCALL","gas":997,"gasCost":100,"depth":1,"stack":["0x0","0x0","0x44","0x0","0xbb","0x384"],"memory":call_memory}),
            json!({"pc":0,"op":"PUSH1","gas":900,"gasCost":3,"depth":2,"stack":[],"memory":[]}),
            json!({"pc":2,"op":"REVERT","gas":897,"gasCost":0,"depth":2,"stack":["0x0","0x0"],"memory":[]}),
            json!({"pc":3,"op":"REVERT","gas":800,"gasCost":0,"depth":1,"stack":["0x0","0x0"],"memory":[]}),
        ];
        let trace = json!({"failed":true,"gas":21000,"returnValue":"","structLogs":logs});
        super::super::trace_debug::from_trace("t".into(), "0x00000000000000000000000000000000000000aa", &trace, Vec::new(), &HashMap::new()).unwrap()
    }

    #[test]
    fn frames_have_kind_input_outcome_and_ranges() {
        let session = session();
        let frames = frames(&session);
        assert_eq!(frames.len(), 2);
        assert_eq!((frames[0].kind.as_str(), frames[0].outcome.as_str(), frames[0].first, frames[0].last), ("TX", "reverted", 0, 4));
        assert_eq!((frames[1].kind.as_str(), frames[1].outcome.as_str(), frames[1].first, frames[1].last), ("STATICCALL", "reverted", 2, 3));
        assert_eq!(frames[1].address, "0x00000000000000000000000000000000000000bb");
        assert_eq!(&frames[1].input[..4], &[0xa9, 0x05, 0x9c, 0xbb]);
        assert_eq!(frame_at(&frames, 3), Some(1));
        assert_eq!(frame_at(&frames, 4), Some(0));
    }

    #[test]
    fn an_attached_abi_names_and_decodes_an_unmatched_frame() {
        let session = session();
        let frames = frames(&session);
        let abi: Abi = serde_json::from_str(r#"[{"type":"function","name":"transfer","stateMutability":"nonpayable","inputs":[{"name":"to","type":"address"},{"name":"amount","type":"uint256"}],"outputs":[]}]"#).unwrap();
        let mut abis = Abis::default();
        assert!(name(&session, &frames[1], &abis).ends_with(".0xa9059cbb"));
        abis.by_address.insert("0x00000000000000000000000000000000000000bb".into(), ("Token".into(), abi));
        assert_eq!(name(&session, &frames[1], &abis), "Token.transfer at 0x0000…00bb");
        let (signature, args) = arguments(&frames[1], &abis).unwrap();
        assert_eq!(signature, "transfer(address,uint256)");
        assert_eq!(args[1], ("amount".into(), "0".into()));
        assert!(describe(&session, &frames[1], &abis).starts_with("staticcall Token.transfer"));
    }
}

#[cfg(test)]
mod ambiguity_tests {
    use super::*;

    #[test]
    fn a_selector_two_contracts_declare_is_not_guessed() {
        let transfer: Abi = serde_json::from_str(r#"[{"type":"function","name":"transfer","stateMutability":"nonpayable","inputs":[{"name":"to","type":"address"},{"name":"amount","type":"uint256"}],"outputs":[]}]"#).unwrap();
        let abis = Abis { by_address: HashMap::new(), named: vec![("A".into(), transfer.clone()), ("B".into(), transfer.clone())] };
        let selector = [0xa9, 0x05, 0x9c, 0xbb];
        assert!(abis.function("0xbb", &selector).is_none());
        assert_eq!(abis.candidates(&selector), vec!["A.transfer", "B.transfer"]);
        let mut attached = abis.clone();
        attached.by_address.insert("0xbb".into(), ("B".into(), transfer));
        assert_eq!(attached.function("0xBB", &selector).unwrap().0, "B");
    }
}
