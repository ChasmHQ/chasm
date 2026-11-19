//! Why a traced execution reverted: the frame where the failure started, its
//! function, and its revert data decoded against every ABI Chasm knows. The
//! outer frames usually only bubble that data up, so the deepest frame that
//! reverted on its own is the one to read.
use super::trace_debug::Session;
use ethers::abi::{Abi, AbiDecode, Function, FunctionExt};

/// One reverting frame: where it is in the trace and what it returned.
#[derive(Clone, Debug, PartialEq)]
pub struct Failure {
    /// Step of the REVERT (or the failing opcode) inside the frame.
    pub step: usize,
    pub depth: u64,
    pub address: String,
    /// `Contract.function` when an ABI names the selector.
    pub frame: String,
    /// The revert decoded, or how the frame failed without data.
    pub reason: String,
}

/// Returns the first failure in step order whose frame failed on its own
/// (not by forwarding a callee's revert), or None if nothing reverted.
pub fn explain(session: &Session, abis: &[(String, Abi)]) -> Option<Failure> {
    let steps = &session.steps;
    for index in 0..steps.len() {
        let step = &steps[index];
        let next_depth = steps.get(index + 1).map(|next| next.depth);
        let leaves_frame = next_depth.is_none_or(|depth| depth < step.depth);
        let failed = match step.op.as_str() {
            "REVERT" => true,
            "INVALID" => true,
            // Out of gas: the frame ends on an opcode that is not a normal exit.
            op => leaves_frame && !matches!(op, "STOP" | "RETURN" | "SELFDESTRUCT") && step.gas < step.gas_cost,
        };
        if !failed {
            continue;
        }
        let data = revert_data(step);
        // A frame that re-raises exactly what its callee returned is not the cause.
        if forwards_callee(session, index, &data) {
            continue;
        }
        let selector = frame_selector(session, index);
        return Some(Failure {
            step: index,
            depth: step.depth,
            address: step.address.clone(),
            frame: frame_name(session, index, selector.as_deref(), abis),
            reason: match step.op.as_str() {
                "REVERT" => decode_reason(&data, abis),
                "INVALID" => "hit an INVALID opcode (assert or a designated invalid instruction)".into(),
                _ => format!("ran out of gas at {} (needed {}, had {})", step.op, step.gas_cost, step.gas),
            },
        });
    }
    creation_too_large(session)
}

/// A creation whose constructor returned runtime code over EIP-170's limit:
/// nothing reverted, the node rejected the returned code.
fn creation_too_large(session: &Session) -> Option<Failure> {
    let index = session.steps.len().checked_sub(1)?;
    let last = &session.steps[index];
    if !session.target.is_empty() || last.depth != 1 || last.op != "RETURN" || last.stack.len() < 2 {
        return None;
    }
    let length = ethers::types::U256::from_big_endian(&word(&last.stack[last.stack.len() - 2]));
    let limit = super::contract_service::MAX_RUNTIME_BYTES;
    (length > ethers::types::U256::from(limit)).then(|| Failure {
        step: index,
        depth: 1,
        address: String::new(),
        frame: "the constructor".into(),
        reason: format!("it returned {length} bytes of runtime code, over the {limit}-byte limit (EIP-170). Enable the optimizer or split the contract"),
    })
}

/// One line for the trace status.
pub fn summary(failure: &Failure) -> String {
    let at = if failure.depth > 1 { format!(" (call depth {})", failure.depth) } else { String::new() };
    format!("Reverted in {}{at} at step {}: {}.", failure.frame, failure.step + 1, failure.reason.trim_end_matches('.'))
}

fn word(text: &str) -> [u8; 32] {
    let digits = format!("{:0>64}", text.trim_start_matches("0x"));
    let mut out = [0u8; 32];
    if let Ok(bytes) = ethers::utils::hex::decode(&digits[digits.len() - 64..]) {
        out.copy_from_slice(&bytes);
    }
    out
}

fn memory_bytes(memory: &[String], offset: usize, length: usize) -> Vec<u8> {
    let flat: Vec<u8> = memory.iter().flat_map(|text| word(text)).collect();
    (offset..offset.saturating_add(length)).map(|at| flat.get(at).copied().unwrap_or(0)).collect()
}

/// REVERT's data: memory[offset .. offset+length] at that step.
fn revert_data(step: &super::trace_debug::Step) -> Vec<u8> {
    if step.op != "REVERT" || step.stack.len() < 2 {
        return Vec::new();
    }
    let top = |from_top: usize| ethers::types::U256::from_big_endian(&word(&step.stack[step.stack.len() - 1 - from_top]));
    let (offset, length) = (top(0), top(1));
    if offset > ethers::types::U256::from(1u64 << 24) || length > ethers::types::U256::from(64 * 1024) {
        return Vec::new();
    }
    memory_bytes(&step.memory, offset.as_usize(), length.as_usize())
}

/// True when the frame's revert data is exactly what a callee just returned
/// (Solidity bubbles a failed call's data up unchanged).
fn forwards_callee(session: &Session, index: usize, data: &[u8]) -> bool {
    let depth = session.steps[index].depth;
    // Walk back to the last step of a deeper frame, if the frame called one.
    let Some(callee_end) = (0..index).rev().take_while(|at| session.steps[*at].depth >= depth).find(|at| session.steps[*at].depth == depth + 1) else {
        return false;
    };
    let callee = &session.steps[callee_end];
    callee.op == "REVERT" && revert_data(callee) == data && !data.is_empty()
}

/// Selector of the call that entered the frame running at `index`.
fn frame_selector(session: &Session, index: usize) -> Option<Vec<u8>> {
    let depth = session.steps[index].depth;
    let entry = (0..=index).rev().take_while(|at| session.steps[*at].depth >= depth).last()?;
    if entry == 0 {
        return session.calldata.get(..4).map(<[u8]>::to_vec);
    }
    let call = &session.steps[entry - 1];
    let from_top = match call.op.as_str() {
        "CALL" | "CALLCODE" => 3,
        "DELEGATECALL" | "STATICCALL" => 2,
        _ => return None,
    };
    let word = word(call.stack.get(call.stack.len().checked_sub(1 + from_top)?)?);
    let offset = ethers::types::U256::from_big_endian(&word);
    if offset > ethers::types::U256::from(1u64 << 24) {
        return None;
    }
    Some(memory_bytes(&call.memory, offset.as_usize(), 4))
}

fn frame_name(session: &Session, index: usize, selector: Option<&[u8]>, abis: &[(String, Abi)]) -> String {
    let address = &session.steps[index].address;
    let contract = session
        .mapped
        .get(address)
        .map(|program| session.programs[*program].name.clone());
    let function = selector.and_then(|selector| {
        // The running contract's own ABI first, then any other known ABI.
        let own = contract.as_ref().and_then(|name| abis.iter().find(|(abi_name, _)| abi_name == name));
        own.into_iter().chain(abis.iter()).find_map(|(name, abi)| {
            abi.functions().find(|function: &&Function| function.selector() == selector).map(|function| (name.clone(), function.name.clone()))
        })
    });
    let short = if address.len() > 12 { format!("{}…{}", &address[..6], &address[address.len() - 4..]) } else { address.clone() };
    match (contract, function, selector) {
        (Some(name), Some((_, function)), _) => format!("{name}.{function}"),
        (None, Some((abi_name, function)), _) => format!("{abi_name}.{function} at {short}"),
        (Some(name), None, Some(selector)) if selector.len() == 4 => format!("{name}.0x{}", ethers::utils::hex::encode(selector)),
        (Some(name), None, _) => name,
        (None, None, Some(selector)) if selector.len() == 4 => format!("{short}.0x{}", ethers::utils::hex::encode(selector)),
        (None, None, _) => short,
    }
}

/// Revert data as text: Error(string), Panic(code) or a custom error from
/// any known ABI. Empty data is a bare revert.
pub fn decode_reason(data: &[u8], abis: &[(String, Abi)]) -> String {
    if data.is_empty() {
        return "a bare revert (no data: a require without a message, a failed low-level call, or revert())".into();
    }
    if data.len() < 4 {
        return format!("unrecognized revert data 0x{}", ethers::utils::hex::encode(data));
    }
    let (selector, body) = data.split_at(4);
    if selector == [0x08, 0xc3, 0x79, 0xa0] {
        if let Ok(text) = String::decode(body) {
            return format!("Error(\"{text}\")");
        }
    }
    if selector == [0x4e, 0x48, 0x7b, 0x71] {
        if let Ok(code) = ethers::types::U256::decode(body) {
            return format!("Panic({code:#x}): {}", panic_text(code.low_u64()));
        }
    }
    let mut found = Vec::new();
    for (name, abi) in abis {
        for error in abi.errors.values().flatten() {
            let signature = error.signature();
            if signature.as_bytes()[..4] == *selector && !found.iter().any(|(_, seen): &(String, ethers::types::H256)| *seen == signature) {
                let kinds: Vec<_> = error.inputs.iter().map(|param| param.kind.clone()).collect();
                let args = ethers::abi::decode(&kinds, body)
                    .map(|tokens| {
                        error.inputs.iter().zip(tokens).map(|(param, token)| if param.name.is_empty() { token_text(&token) } else { format!("{}: {}", param.name, token_text(&token)) }).collect::<Vec<_>>().join(", ")
                    })
                    .unwrap_or_else(|_| "undecodable arguments".into());
                found.push((format!("{}({args}) from {name}", error.name), signature));
            }
        }
    }
    match found.len() {
        0 => format!("custom error 0x{} (no known ABI declares it)", ethers::utils::hex::encode(selector)),
        _ => found.into_iter().map(|(text, _)| text).collect::<Vec<_>>().join(" or "),
    }
}

/// Token text with 0x addresses (checksummed) and 0x bytes.
fn token_text(token: &ethers::abi::Token) -> String {
    use ethers::abi::Token;
    match token {
        Token::Address(address) => ethers::utils::to_checksum(address, None),
        Token::Bytes(bytes) | Token::FixedBytes(bytes) => format!("0x{}", ethers::utils::hex::encode(bytes)),
        Token::Array(items) | Token::FixedArray(items) => format!("[{}]", items.iter().map(token_text).collect::<Vec<_>>().join(", ")),
        Token::Tuple(items) => format!("({})", items.iter().map(token_text).collect::<Vec<_>>().join(", ")),
        other => other.to_string(),
    }
}

fn panic_text(code: u64) -> &'static str {
    match code {
        0x01 => "assert failed",
        0x11 => "arithmetic overflow or underflow",
        0x12 => "division or modulo by zero",
        0x21 => "invalid enum value",
        0x22 => "bad storage byte array",
        0x31 => "pop on an empty array",
        0x32 => "array index out of bounds",
        0x41 => "out of memory",
        0x51 => "call to an uninitialized function pointer",
        _ => "unknown panic code",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    fn abi(text: &str) -> Abi {
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn decodes_standard_and_custom_errors_from_any_abi() {
        let token = abi(r#"[{"type":"error","name":"ERC20InsufficientBalance","inputs":[{"name":"sender","type":"address"},{"name":"balance","type":"uint256"},{"name":"needed","type":"uint256"}]}]"#);
        let error = &token.errors["ERC20InsufficientBalance"][0];
        let mut data = error.signature().as_bytes()[..4].to_vec();
        data.extend(ethers::abi::encode(&[
            ethers::abi::Token::Address(ethers::types::Address::repeat_byte(0x11)),
            ethers::abi::Token::Uint(5.into()),
            ethers::abi::Token::Uint(9.into()),
        ]));
        let text = decode_reason(&data, &[("USDC".into(), token)]);
        assert!(text.starts_with("ERC20InsufficientBalance(sender: 0x1111111111111111111111111111111111111111"), "{text}");
        assert!(text.contains("balance: 5") && text.contains("needed: 9") && text.ends_with("from USDC"), "{text}");
        let mut panic = vec![0x4e, 0x48, 0x7b, 0x71];
        panic.extend(ethers::abi::encode(&[ethers::abi::Token::Uint(0x11.into())]));
        assert_eq!(decode_reason(&panic, &[]), "Panic(0x11): arithmetic overflow or underflow");
        assert!(decode_reason(&[], &[]).starts_with("a bare revert"));
        assert!(decode_reason(&[1, 2, 3, 4], &[]).contains("no known ABI"));
    }

    #[test]
    fn the_inner_frame_that_reverted_first_is_the_cause() {
        // depth 1 calls depth 2; depth 2 reverts with Error("no"), depth 1 re-raises it.
        let mut data = vec![0x08, 0xc3, 0x79, 0xa0];
        data.extend(ethers::abi::encode(&[ethers::abi::Token::String("no".into())]));
        let memory: Vec<String> = data.chunks(32).map(|chunk| { let mut w = chunk.to_vec(); w.resize(32, 0); ethers::utils::hex::encode(w) }).collect();
        let length = format!("{:#x}", data.len());
        let logs = vec![
            json!({"pc":0,"op":"CALL","gas":1000,"gasCost":100,"depth":1,"stack":[],"memory":[]}),
            json!({"pc":0,"op":"PUSH1","gas":800,"gasCost":3,"depth":2,"stack":[],"memory":[]}),
            json!({"pc":2,"op":"REVERT","gas":797,"gasCost":0,"depth":2,"stack":[length.clone(),"0x0"],"memory":memory.clone()}),
            json!({"pc":1,"op":"REVERT","gas":700,"gasCost":0,"depth":1,"stack":[length,"0x0"],"memory":memory}),
        ];
        let trace = json!({"failed":true,"gas":21000,"returnValue":"","structLogs":logs});
        let session = super::super::trace_debug::from_trace("t".into(), "0x00000000000000000000000000000000000000aa", &trace, Vec::new(), &HashMap::new()).unwrap();
        let failure = explain(&session, &[]).unwrap();
        assert_eq!((failure.step, failure.depth), (2, 2));
        assert_eq!(failure.reason, "Error(\"no\")");
        assert!(summary(&failure).starts_with("Reverted in "), "{}", summary(&failure));
    }

    #[test]
    fn an_oversized_creation_names_the_size_limit() {
        let logs = vec![
            json!({"pc":0,"op":"PUSH2","gas":1000,"gasCost":3,"depth":1,"stack":[],"memory":[]}),
            json!({"pc":3,"op":"RETURN","gas":990,"gasCost":0,"depth":1,"stack":["0x61a8","0x0"],"memory":[]}),
        ];
        let trace = json!({"failed":true,"gas":30000000,"returnValue":"","structLogs":logs});
        let session = super::super::trace_debug::from_trace("t".into(), "", &trace, Vec::new(), &HashMap::new()).unwrap();
        let failure = explain(&session, &[]).unwrap();
        assert!(failure.reason.contains("25000 bytes") && failure.reason.contains("EIP-170"), "{}", failure.reason);
    }
}
