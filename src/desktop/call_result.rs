use std::fmt::{self, Write as _};

use ethers::abi::{self, Abi, Function, ParamType, Token};
use ethers::types::U256;
use ethers::utils::hex;
use serde_json::Value;

const MAX_PAYLOAD_BYTES: usize = 256 * 1024;
const MAX_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_DEPTH: usize = 32;
const MAX_ELEMENTS: usize = 4096;
const MAX_LOGS: usize = 100;
const MAX_EVENT_SUMMARY: usize = 512;

#[derive(Clone, Debug)]
pub struct ResultContext {
    pub contract: String,
    pub address: String,
    pub signature: String,
    pub request: Value,
    pub workspace_epoch: u64,
    pub network_epoch: u64,
    pub source: Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TraceState {
    Empty,
    Loading,
    Ready(String),
    Unavailable(String),
}

#[derive(Clone, Debug)]
pub struct CallResult {
    pub context: ResultContext,
    pub raw: Value,
    pub abi: Abi,
    pub simple: String,
    pub trace: TraceState,
}

impl CallResult {
    pub fn from_call(
        context: ResultContext,
        function: &Function,
        contract_abi: &Abi,
        raw: Value,
    ) -> Self {
        let simple = decode_call(function, contract_abi, &raw);
        Self {
            context,
            raw,
            abi: contract_abi.clone(),
            simple,
            trace: TraceState::Empty,
        }
    }

    pub fn from_receipt(context: ResultContext, contract_abi: &Abi, raw: Value) -> Self {
        let simple = decode_receipt(contract_abi, &raw).to_string();
        Self {
            context,
            raw,
            abi: contract_abi.clone(),
            simple,
            trace: TraceState::Empty,
        }
    }

}

/// Accepts either an eth_call result/error or its JSON-RPC envelope.
pub fn decode_call(function: &Function, contract_abi: &Abi, raw: &Value) -> String {
    if let Some(error) = rpc_error(raw) {
        return format_rpc_error(error, contract_abi);
    }
    let result = rpc_result(raw);
    let Some(text) = result.as_str() else {
        return "Cannot decode call result: expected a hex byte string. See Raw for the response."
            .into();
    };
    let bytes = match parse_bytes(text, MAX_PAYLOAD_BYTES) {
        Ok(bytes) => bytes,
        Err(reason) => return with_raw("Cannot decode call result", reason, text),
    };
    let kinds: Vec<_> = function.outputs.iter().map(|param| &param.kind).collect();
    match decode_tokens(&kinds, &bytes) {
        Ok(tokens) => {
            if tokens.is_empty() {
                return "Call completed with no return values.".into();
            }
            let mut out = Output::new(MAX_OUTPUT_BYTES);
            for (index, ((param, token), kind)) in
                function.outputs.iter().zip(&tokens).zip(&kinds).enumerate()
            {
                if index != 0 {
                    out.push("\n");
                }
                if param.name.is_empty() {
                    let _ = write!(out, "Return {}: ", index + 1);
                } else {
                    out.push(&param.name);
                    out.push(": ");
                }
                format_token(&mut out, token, kind, 0);
                if out.full() {
                    break;
                }
            }
            out.finish()
        }
        Err(reason) => with_raw("Cannot decode call result", &reason, text),
    }
}

fn rpc_result(raw: &Value) -> &Value {
    if raw.is_object() && raw.get("result").is_some() {
        &raw["result"]
    } else {
        raw
    }
}

fn rpc_error(raw: &Value) -> Option<&Value> {
    if let Some(error) = raw.get("error") {
        if !error.is_null() {
            return Some(error);
        }
    }
    if raw.get("code").and_then(Value::as_i64).is_some() && raw.get("message").is_some() {
        return Some(raw);
    }
    None
}

/// Decoded revert of an RPC `error` object (Error(string), Panic(uint256) or a
/// custom error from the ABI), or None when the error carries no revert data.
pub fn revert_reason(error: &Value, contract_abi: &Abi) -> Option<String> {
    let data = error.get("data").and_then(find_error_data)?;
    let bytes = parse_bytes(data, MAX_PAYLOAD_BYTES).ok()?;
    let mut out = Output::new(MAX_OUTPUT_BYTES);
    format_revert(&mut out, contract_abi, &bytes, data);
    Some(out.finish())
}

fn format_rpc_error(error: &Value, contract_abi: &Abi) -> String {
    let mut out = Output::new(MAX_OUTPUT_BYTES);
    out.push("RPC error");
    if let Some(code) = error.get("code").and_then(Value::as_i64) {
        let _ = write!(out, " (code {code})");
    }
    out.push(".");
    if let Some(data) = error.get("data").and_then(find_error_data) {
        match parse_bytes(data, MAX_PAYLOAD_BYTES) {
            Ok(bytes) => {
                out.push(" ");
                format_revert(&mut out, contract_abi, &bytes, data);
            }
            Err(reason) => {
                out.push(" Cannot decode error data: ");
                out.push(reason);
                out.push(" See Raw for the response.");
            }
        }
    }
    out.finish()
}

// Restrict traversal to RPC error-data wrappers; never display the provider message.
fn find_error_data(value: &Value) -> Option<&str> {
    fn walk<'a>(value: &'a Value, depth: usize, remaining: &mut usize) -> Option<&'a str> {
        if depth > 8 || *remaining == 0 {
            return None;
        }
        *remaining -= 1;
        if let Some(text) = value.as_str() {
            return if text.starts_with("0x") || text.starts_with("0X") {
                Some(text)
            } else {
                None
            };
        }
        if let Some(object) = value.as_object() {
            for key in ["data", "return", "result", "originalError", "error"] {
                if let Some(found) = object.get(key).and_then(|v| walk(v, depth + 1, remaining)) {
                    return Some(found);
                }
            }
            // Some nodes key per-transaction error objects by transaction hash.
            for (key, child) in object {
                if key.starts_with("0x") && child.is_object() {
                    if let Some(found) = walk(child, depth + 1, remaining) {
                        return Some(found);
                    }
                }
            }
        }
        None
    }
    walk(value, 0, &mut 256)
}

fn format_revert(out: &mut Output, contract_abi: &Abi, bytes: &[u8], raw: &str) {
    if bytes.len() < 4 {
        out.push("Error data is too short for a revert selector. Raw data: ");
        out.push(raw);
        return;
    }
    let selector = &bytes[..4];
    let body = &bytes[4..];
    if selector == [0x08, 0xc3, 0x79, 0xa0] {
        match decode_tokens(&[&ParamType::String], body) {
            Ok(tokens) => {
                out.push("Revert Error(");
                format_token(out, &tokens[0], &ParamType::String, 0);
                out.push(").");
            }
            Err(reason) => append_decode_failure(out, "Error(string)", &reason, raw),
        }
        return;
    }
    if selector == [0x4e, 0x48, 0x7b, 0x71] {
        match decode_tokens(&[&ParamType::Uint(256)], body) {
            Ok(tokens) => {
                out.push("Revert Panic(");
                format_token(out, &tokens[0], &ParamType::Uint(256), 0);
                out.push(").");
            }
            Err(reason) => append_decode_failure(out, "Panic(uint256)", &reason, raw),
        }
        return;
    }
    let mut matched = None;
    for error in contract_abi.errors.values().flatten().take(MAX_ELEMENTS) {
        if error.inputs.len() > MAX_ELEMENTS || error.name.len() > MAX_OUTPUT_BYTES {
            continue;
        }
        let kinds: Vec<_> = error.inputs.iter().map(|param| &param.kind).collect();
        if validate_kinds(&kinds).is_err() {
            continue;
        }
        if &error.signature().as_bytes()[..4] == selector {
            if matched.is_some() {
                out.push("Ambiguous custom error selector. Raw data: ");
                out.push(raw);
                return;
            }
            matched = Some(error);
        }
    }
    if let Some(error) = matched {
        let kinds: Vec<_> = error.inputs.iter().map(|param| &param.kind).collect();
        match decode_tokens(&kinds, body) {
            Ok(tokens) => {
                out.push("Revert ");
                out.push(&error.name);
                out.push("(");
                for (index, ((param, token), kind)) in
                    error.inputs.iter().zip(&tokens).zip(&kinds).enumerate()
                {
                    if index != 0 {
                        out.push(", ");
                    }
                    if !param.name.is_empty() {
                        out.push(&param.name);
                        out.push(": ");
                    }
                    format_token(out, token, kind, 0);
                    if out.full() {
                        break;
                    }
                }
                out.push(").");
            }
            Err(reason) => append_decode_failure(out, "custom error", &reason, raw),
        }
    } else {
        out.push("Unrecognized error data. Raw data: ");
        out.push(raw);
    }
}

fn append_decode_failure(out: &mut Output, kind: &str, reason: &str, raw: &str) {
    out.push("Cannot decode ");
    out.push(kind);
    out.push(": ");
    out.push(reason);
    out.push(" Raw data: ");
    out.push(raw);
}

fn with_raw(prefix: &str, reason: &str, raw: &str) -> String {
    let mut out = Output::new(MAX_OUTPUT_BYTES);
    out.push(prefix);
    out.push(": ");
    out.push(reason);
    out.push(" Raw data: ");
    out.push(raw);
    out.finish()
}

fn parse_bytes(text: &str, limit: usize) -> Result<Vec<u8>, &'static str> {
    let digits = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .ok_or("Hex bytes must start with 0x.")?;
    if digits.len() / 2 > limit {
        return Err("Hex payload exceeds 256 KiB.");
    }
    if digits.len() % 2 != 0 {
        return Err("Hex bytes require an even number of digits.");
    }
    if !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Hex bytes contain an invalid digit.");
    }
    hex::decode(digits).map_err(|_| "Hex bytes contain an invalid digit.")
}

// Validate the shape and resource use before ethabi allocates or follows offsets.
fn validate_kinds(kinds: &[&ParamType]) -> Result<(), String> {
    fn visit(kind: &ParamType, depth: usize, remaining: &mut usize) -> Result<(), String> {
        if depth > MAX_DEPTH {
            return Err("ABI type nesting exceeds 32 levels.".into());
        }
        if *remaining == 0 {
            return Err("ABI types exceed 4096 elements.".into());
        }
        *remaining -= 1;
        match kind {
            ParamType::Uint(bits) | ParamType::Int(bits)
                if *bits < 8 || *bits > 256 || bits % 8 != 0 =>
            {
                Err("Invalid ABI integer width.".into())
            }
            ParamType::FixedBytes(size) if *size == 0 || *size > 32 => {
                Err("Invalid ABI fixed bytes width.".into())
            }
            ParamType::FixedArray(_, size) if *size == 0 || *size > MAX_ELEMENTS => {
                Err("Invalid or oversized ABI fixed array.".into())
            }
            ParamType::Array(inner) | ParamType::FixedArray(inner, _) => {
                visit(inner, depth + 1, remaining)
            }
            ParamType::Tuple(items) => {
                for item in items {
                    visit(item, depth + 1, remaining)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
    let mut remaining = MAX_ELEMENTS;
    for kind in kinds {
        visit(kind, 0, &mut remaining)?;
    }
    Ok(())
}

struct Budget {
    elements: usize,
    bytes: usize,
}

impl Budget {
    fn take(&mut self, bytes: usize) -> Result<(), String> {
        if self.elements == 0 || bytes > self.bytes {
            return Err("Decoded value exceeds the element or payload budget.".into());
        }
        self.elements -= 1;
        self.bytes -= bytes;
        Ok(())
    }
}

fn word(data: &[u8], offset: usize) -> Result<&[u8], String> {
    let end = offset.checked_add(32).ok_or("ABI offset overflow.")?;
    data.get(offset..end)
        .ok_or_else(|| "ABI data is shorter than expected.".into())
}

fn word_usize(data: &[u8], offset: usize) -> Result<usize, String> {
    let value = U256::from_big_endian(word(data, offset)?);
    if value > U256::from(usize::MAX) {
        return Err("ABI offset or length is too large.".into());
    }
    Ok(value.as_usize())
}

fn target(data: &[u8], offset: usize) -> Result<&[u8], String> {
    let position = word_usize(data, offset)?;
    if position % 32 != 0 {
        return Err("ABI offset is not word-aligned.".into());
    }
    data.get(position..)
        .ok_or_else(|| "ABI offset is outside the payload.".into())
}

fn check_value(
    kind: &ParamType,
    data: &[u8],
    offset: usize,
    depth: usize,
    budget: &mut Budget,
) -> Result<usize, String> {
    if depth > MAX_DEPTH {
        return Err("Decoded value nesting exceeds 32 levels.".into());
    }
    budget.take(0)?;
    let next = || {
        offset
            .checked_add(32)
            .ok_or_else(|| "ABI offset overflow.".to_owned())
    };
    match kind {
        ParamType::Address => {
            if word(data, offset)?[..12].iter().any(|byte| *byte != 0) {
                return Err("Invalid address padding.".into());
            }
            next()
        }
        ParamType::Bool => {
            let value = word(data, offset)?;
            if value[..31].iter().any(|byte| *byte != 0) || value[31] > 1 {
                return Err("Invalid ABI boolean.".into());
            }
            next()
        }
        ParamType::Uint(bits) | ParamType::Int(bits) => {
            let value = U256::from_big_endian(word(data, offset)?);
            let mask = U256::MAX >> (256 - bits);
            let sign = matches!(kind, ParamType::Int(_)) && value.bit(bits - 1);
            if (sign && (value | mask) != U256::MAX) || (!sign && (value & !mask) != U256::zero()) {
                return Err("Integer is not correctly extended to its ABI width.".into());
            }
            next()
        }
        ParamType::FixedBytes(size) => {
            if word(data, offset)?[*size..].iter().any(|byte| *byte != 0) {
                return Err("Invalid fixed bytes padding.".into());
            }
            next()
        }
        ParamType::Bytes | ParamType::String => {
            let tail = target(data, offset)?;
            let length = word_usize(tail, 0)?;
            if length > budget.bytes {
                return Err("Decoded byte values exceed 256 KiB.".into());
            }
            budget.bytes -= length;
            let padded = length.checked_add(31).ok_or("ABI length overflow.")? / 32 * 32;
            let end = 32usize.checked_add(padded).ok_or("ABI length overflow.")?;
            let padded_bytes = tail
                .get(32..end)
                .ok_or("ABI byte value is shorter than expected.")?;
            if padded_bytes[length..].iter().any(|byte| *byte != 0) {
                return Err("Invalid byte value padding.".into());
            }
            if matches!(kind, ParamType::String)
                && std::str::from_utf8(&padded_bytes[..length]).is_err()
            {
                return Err("ABI string is not valid UTF-8.".into());
            }
            next()
        }
        ParamType::Array(inner) => {
            let tail = target(data, offset)?;
            let length = word_usize(tail, 0)?;
            if length > budget.elements {
                return Err("Decoded array exceeds 4096 elements.".into());
            }
            let values = tail
                .get(32..)
                .ok_or("ABI array is shorter than expected.")?;
            let mut position = 0;
            for _ in 0..length {
                position = check_value(inner, values, position, depth + 1, budget)?;
            }
            next()
        }
        ParamType::FixedArray(inner, length) => {
            if *length > budget.elements {
                return Err("Decoded array exceeds 4096 elements.".into());
            }
            let dynamic = kind.is_dynamic();
            let values = if dynamic { target(data, offset)? } else { data };
            let mut position = if dynamic { 0 } else { offset };
            for _ in 0..*length {
                position = check_value(inner, values, position, depth + 1, budget)?;
            }
            if dynamic {
                next()
            } else {
                Ok(position)
            }
        }
        ParamType::Tuple(items) => {
            if items.len() > budget.elements {
                return Err("Decoded tuple exceeds 4096 elements.".into());
            }
            let dynamic = kind.is_dynamic();
            let values = if dynamic { target(data, offset)? } else { data };
            let mut position = if dynamic { 0 } else { offset };
            for item in items {
                position = check_value(item, values, position, depth + 1, budget)?;
            }
            if dynamic {
                next()
            } else {
                Ok(position)
            }
        }
    }
}

fn decode_tokens(kinds: &[&ParamType], bytes: &[u8]) -> Result<Vec<Token>, String> {
    if bytes.len() > MAX_PAYLOAD_BYTES {
        return Err("ABI payload exceeds 256 KiB.".into());
    }
    validate_kinds(kinds)?;
    let mut budget = Budget {
        elements: MAX_ELEMENTS,
        bytes: MAX_PAYLOAD_BYTES,
    };
    let mut position = 0;
    for kind in kinds {
        position = check_value(kind, bytes, position, 0, &mut budget)?;
    }
    let owned: Vec<_> = kinds.iter().map(|kind| (*kind).clone()).collect();
    let tokens =
        abi::decode(&owned, bytes).map_err(|error| format!("ABI decode failed: {error}."))?;
    if abi::encode(&tokens) != bytes {
        return Err("ABI data is not a canonical encoding of the declared types.".into());
    }
    Ok(tokens)
}

struct Output {
    text: String,
    limit: usize,
    truncated: bool,
}

impl Output {
    fn new(limit: usize) -> Self {
        Self {
            text: String::new(),
            limit,
            truncated: false,
        }
    }
    fn full(&self) -> bool {
        self.truncated
    }
    fn push(&mut self, value: &str) {
        if self.truncated {
            return;
        }
        let available = self.limit.saturating_sub(self.text.len());
        if value.len() <= available {
            self.text.push_str(value);
            return;
        }
        let mut end = available.min(value.len());
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        self.text.push_str(&value[..end]);
        self.truncated = true;
    }
    fn quoted(&mut self, value: &str) {
        self.push("\"");
        for ch in value.chars() {
            match ch {
                '"' => self.push("\\\""),
                '\\' => self.push("\\\\"),
                '\n' => self.push("\\n"),
                '\r' => self.push("\\r"),
                '\t' => self.push("\\t"),
                '\u{08}' => self.push("\\b"),
                '\u{0c}' => self.push("\\f"),
                c if c <= '\u{1f}' => {
                    let _ = write!(self, "\\u{:04x}", c as u32);
                }
                c => {
                    let mut buf = [0; 4];
                    self.push(c.encode_utf8(&mut buf));
                }
            }
            if self.full() {
                break;
            }
        }
        self.push("\"");
    }
    fn finish(mut self) -> String {
        if self.truncated {
            const MARKER: &str = "... [truncated; see Raw]";
            let target = self.limit.saturating_sub(MARKER.len());
            let mut end = target.min(self.text.len());
            while !self.text.is_char_boundary(end) {
                end -= 1;
            }
            self.text.truncate(end);
            if self.limit >= MARKER.len() {
                self.text.push_str(MARKER);
            }
        }
        self.text
    }
}

impl fmt::Write for Output {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        self.push(value);
        Ok(())
    }
}

fn format_token(out: &mut Output, token: &Token, kind: &ParamType, depth: usize) {
    if depth > MAX_DEPTH {
        out.push("[depth limit]");
        return;
    }
    match (token, kind) {
        (Token::Uint(value), ParamType::Uint(_)) => {
            let _ = write!(out, "{value}");
        }
        (Token::Int(value), ParamType::Int(bits)) => {
            if value.bit(bits - 1) {
                let magnitude = value.overflowing_neg().0;
                let _ = write!(out, "-{magnitude}");
            } else {
                let _ = write!(out, "{value}");
            }
        }
        (Token::Bool(value), ParamType::Bool) => out.push(if *value { "true" } else { "false" }),
        (Token::String(value), ParamType::String) => out.quoted(value),
        (Token::Address(value), ParamType::Address) => out.quoted(&format!("{value:#x}")),
        (Token::Bytes(bytes), ParamType::Bytes)
        | (Token::FixedBytes(bytes), ParamType::FixedBytes(_)) => {
            out.push("\"0x");
            for byte in bytes {
                let _ = write!(out, "{byte:02x}");
                if out.full() {
                    break;
                }
            }
            out.push("\"");
        }
        (Token::Array(values), ParamType::Array(inner))
        | (Token::FixedArray(values), ParamType::FixedArray(inner, _)) => {
            out.push("[");
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    out.push(", ");
                }
                format_token(out, value, inner, depth + 1);
                if out.full() {
                    break;
                }
            }
            out.push("]");
        }
        (Token::Tuple(values), ParamType::Tuple(kinds)) => {
            out.push("(");
            for (index, (value, kind)) in values.iter().zip(kinds).enumerate() {
                if index != 0 {
                    out.push(", ");
                }
                format_token(out, value, kind, depth + 1);
                if out.full() {
                    break;
                }
            }
            out.push(")");
        }
        _ => out.push("[ABI type mismatch]"),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiptStatus {
    Success,
    Reverted,
    PendingOrUnavailable,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedEvent {
    pub log_index: usize,
    pub summary: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceiptSimple {
    pub status: ReceiptStatus,
    pub transaction_hash: Option<String>,
    pub block_number: Option<String>,
    pub gas_used: Option<String>,
    pub log_count: Option<usize>,
    pub decoded_events: Vec<DecodedEvent>,
    pub unmatched_logs: usize,
    pub unprocessed_logs: usize,
    pub issues: Vec<String>,
}

impl fmt::Display for ReceiptSimple {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = Output::new(MAX_OUTPUT_BYTES);
        out.push(match self.status {
            ReceiptStatus::Success => "Transaction status: Success",
            ReceiptStatus::Reverted => "Transaction status: Reverted",
            ReceiptStatus::PendingOrUnavailable => "Receipt unavailable: transaction may be pending, or a mined receipt may be incomplete.",
            ReceiptStatus::Unknown => "Transaction status: Unknown",
        });
        if let Some(hash) = &self.transaction_hash {
            out.push("\nTransaction hash: ");
            out.push(hash);
        }
        if let Some(block) = &self.block_number {
            out.push("\nBlock: ");
            out.push(block);
        }
        if let Some(gas) = &self.gas_used {
            out.push("\nGas used: ");
            out.push(gas);
        }
        if let Some(count) = self.log_count {
            let _ = write!(
                out,
                "\nLogs: {count} ({} decoded, {} unmatched, {} not processed)",
                self.decoded_events.len(),
                self.unmatched_logs,
                self.unprocessed_logs
            );
        }
        for event in &self.decoded_events {
            let _ = write!(out, "\nLog {}: ", event.log_index);
            out.push(&event.summary);
            if out.full() {
                break;
            }
        }
        for issue in &self.issues {
            out.push("\n");
            out.push(issue);
        }
        f.write_str(&out.finish())
    }
}

/// A receipt describes transaction execution, never the function's return values.
pub fn decode_receipt(contract_abi: &Abi, raw: &Value) -> ReceiptSimple {
    let mut receipt = ReceiptSimple {
        status: ReceiptStatus::Unknown,
        transaction_hash: None,
        block_number: None,
        gas_used: None,
        log_count: None,
        decoded_events: Vec::new(),
        unmatched_logs: 0,
        unprocessed_logs: 0,
        issues: Vec::new(),
    };
    if rpc_error(raw).is_some() {
        receipt
            .issues
            .push("Receipt request failed. See Raw for the response.".into());
        return receipt;
    }
    let value = rpc_result(raw);
    if value.is_null() {
        receipt.status = ReceiptStatus::PendingOrUnavailable;
        return receipt;
    }
    let Some(object) = value.as_object() else {
        receipt
            .issues
            .push("Invalid receipt response. See Raw for the response.".into());
        return receipt;
    };
    receipt.status = match object.get("status").and_then(quantity) {
        Some(value) if value == U256::one() => ReceiptStatus::Success,
        Some(value) if value == U256::zero() => ReceiptStatus::Reverted,
        _ => ReceiptStatus::Unknown,
    };
    if receipt.status == ReceiptStatus::Unknown {
        receipt
            .issues
            .push("Receipt status is missing or invalid.".into());
    }
    receipt.transaction_hash = object
        .get("transactionHash")
        .and_then(|value| value.as_str())
        .and_then(valid_hash);
    receipt.block_number = object
        .get("blockNumber")
        .and_then(quantity)
        .map(|value| value.to_string());
    receipt.gas_used = object
        .get("gasUsed")
        .and_then(quantity)
        .map(|value| value.to_string());
    for (key, valid) in [
        ("transactionHash", receipt.transaction_hash.is_some()),
        ("blockNumber", receipt.block_number.is_some()),
        ("gasUsed", receipt.gas_used.is_some()),
    ] {
        if !valid {
            receipt
                .issues
                .push(format!("Receipt {key} is missing or invalid."));
        }
    }
    if let Some(logs) = object.get("logs").and_then(Value::as_array) {
        receipt.log_count = Some(logs.len());
        receipt.unprocessed_logs = logs.len().saturating_sub(MAX_LOGS);
        for (index, log) in logs.iter().take(MAX_LOGS).enumerate() {
            match decode_event(contract_abi, log) {
                Some(summary) => receipt.decoded_events.push(DecodedEvent {
                    log_index: index,
                    summary,
                }),
                None => receipt.unmatched_logs += 1,
            }
        }
    } else {
        receipt
            .issues
            .push("Receipt logs are missing or invalid.".into());
    }
    receipt
}

fn quantity(value: &Value) -> Option<U256> {
    let digits = value.as_str()?.strip_prefix("0x")?;
    if digits.is_empty()
        || digits.len() > 64
        || (digits.len() > 1 && digits.starts_with('0'))
        || !digits.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return None;
    }
    U256::from_str_radix(digits, 16).ok()
}

fn valid_hash(text: &str) -> Option<String> {
    let digits = text.strip_prefix("0x")?;
    if digits.len() != 64 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(text.to_owned())
}

fn decode_event(contract_abi: &Abi, log: &Value) -> Option<String> {
    let topics = log.get("topics")?.as_array()?;
    if topics.is_empty() || topics.len() > 4 {
        return None;
    }
    let topic_bytes: Vec<Vec<u8>> = topics
        .iter()
        .map(|topic| {
            let bytes = parse_bytes(topic.as_str()?, 32).ok()?;
            if bytes.len() == 32 {
                Some(bytes)
            } else {
                None
            }
        })
        .collect::<Option<_>>()?;
    let data = parse_bytes(log.get("data")?.as_str()?, MAX_PAYLOAD_BYTES).ok()?;
    let mut matched = None;
    for event in contract_abi.events.values().flatten().take(MAX_ELEMENTS) {
        if event.anonymous
            || event.inputs.len() > MAX_ELEMENTS
            || event.name.len() > MAX_OUTPUT_BYTES
        {
            continue;
        }
        let kinds: Vec<_> = event.inputs.iter().map(|param| &param.kind).collect();
        if validate_kinds(&kinds).is_err() {
            continue;
        }
        if event.signature().as_bytes() == topic_bytes[0].as_slice() {
            if matched.is_some() {
                return None;
            }
            matched = Some(event);
        }
    }
    let event = matched?;
    if event.inputs.iter().filter(|param| param.indexed).count() + 1 != topic_bytes.len() {
        return None;
    }
    let data_kinds: Vec<_> = event
        .inputs
        .iter()
        .filter(|param| !param.indexed)
        .map(|param| &param.kind)
        .collect();
    let data_tokens = decode_tokens(&data_kinds, &data).ok()?;
    let mut values = data_tokens.iter();
    let mut topic_index = 1;
    let mut out = Output::new(MAX_EVENT_SUMMARY);
    out.push(&event.name);
    out.push("(");
    for (index, param) in event.inputs.iter().enumerate() {
        if index != 0 {
            out.push(", ");
        }
        if !param.name.is_empty() {
            out.push(&param.name);
            out.push(": ");
        }
        if param.indexed {
            let hashed = matches!(
                param.kind,
                ParamType::String
                    | ParamType::Bytes
                    | ParamType::Array(_)
                    | ParamType::FixedArray(_, _)
                    | ParamType::Tuple(_)
            );
            let kind = if hashed {
                &ParamType::FixedBytes(32)
            } else {
                &param.kind
            };
            let tokens = decode_tokens(&[kind], &topic_bytes[topic_index]).ok()?;
            topic_index += 1;
            format_token(&mut out, &tokens[0], kind, 0);
            if hashed {
                out.push(" [indexed hash]");
            }
        } else {
            format_token(&mut out, values.next()?, &param.kind, 0);
        }
    }
    out.push(")");
    Some(out.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ethers::abi::{encode, Param, StateMutability};
    use serde_json::json;

    fn function(kinds: Vec<ParamType>) -> Function {
        #[allow(deprecated)]
        Function {
            name: "read".into(),
            inputs: Vec::new(),
            outputs: kinds
                .into_iter()
                .enumerate()
                .map(|(i, kind)| Param {
                    name: format!("v{i}"),
                    kind,
                    internal_type: None,
                })
                .collect(),
            constant: None,
            state_mutability: StateMutability::View,
        }
    }

    fn encoded(tokens: Vec<Token>) -> Value {
        json!(format!("0x{}", hex::encode(encode(&tokens))))
    }

    #[test]
    fn exact_uint256_and_signed_width() {
        let f = function(vec![
            ParamType::Uint(256),
            ParamType::Int(8),
            ParamType::Int(256),
        ]);
        let raw = encoded(vec![
            Token::Uint(U256::MAX),
            Token::Int(U256::MAX),
            Token::Int(U256::one() << 255),
        ]);
        let result = decode_call(&f, &Abi::default(), &raw);
        assert!(result.contains(
            "115792089237316195423570985008687907853269984665640564039457584007913129639935"
        ));
        assert!(result.contains("v1: -1"));
        assert!(result.contains(
            "-57896044618658097711785492504343953926634992332820282019728792003956564819968"
        ));
        let invalid = encoded(vec![
            Token::Uint(U256::MAX),
            Token::Int(U256::from(255)),
            Token::Int(U256::zero()),
        ]);
        assert!(decode_call(&f, &Abi::default(), &invalid).contains("not correctly extended"));
    }

    #[test]
    fn string_array_tuple_and_quotes() {
        let kind = ParamType::Tuple(vec![
            ParamType::String,
            ParamType::Array(Box::new(ParamType::Uint(256))),
        ]);
        let raw = encoded(vec![Token::Tuple(vec![
            Token::String("hi \"there\"\n".into()),
            Token::Array(vec![Token::Uint(1.into()), Token::Uint(2.into())]),
        ])]);
        assert_eq!(
            decode_call(&function(vec![kind]), &Abi::default(), &raw),
            "v0: (\"hi \\\"there\\\"\\n\", [1, 2])"
        );
    }

    #[test]
    fn malformed_bytes_and_budgets_are_reported() {
        let f = function(vec![ParamType::Uint(256)]);
        assert!(decode_call(&f, &Abi::default(), &json!("0x0")).contains("even number"));
        assert!(decode_call(&f, &Abi::default(), &json!("0xzz")).contains("invalid digit"));
        assert!(decode_call(&f, &Abi::default(), &json!("0x01")).contains("shorter"));
        let huge = format!("0x{}", "00".repeat(MAX_PAYLOAD_BYTES + 1));
        let result = decode_call(&f, &Abi::default(), &json!(huge));
        assert!(result.contains("exceeds 256 KiB"));
        assert!(result.len() <= MAX_OUTPUT_BYTES);
    }

    #[test]
    fn standard_custom_and_nested_rpc_errors() {
        let abi: Abi = serde_json::from_value(json!([{ "type":"error", "name":"Denied", "inputs":[{"name":"amount","type":"uint256"}] }])).unwrap();
        let mut revert = vec![0x08, 0xc3, 0x79, 0xa0];
        revert.extend(encode(&[Token::String("no access".into())]));
        let rpc = json!({"error":{"code":3,"message":"secret provider URL", "data":{"originalError":{"data": format!("0x{}", hex::encode(revert))}}}});
        let result = decode_call(&function(vec![]), &abi, &rpc);
        assert!(result.contains("Error(\"no access\")"));
        assert!(!result.contains("secret"));
        let mut panic = vec![0x4e, 0x48, 0x7b, 0x71];
        panic.extend(encode(&[Token::Uint(17.into())]));
        let rpc =
            json!({"code":3,"message":"reverted", "data":format!("0x{}", hex::encode(panic))});
        assert!(decode_call(&function(vec![]), &abi, &rpc).contains("Panic(17)"));
        let custom = abi.errors.get("Denied").unwrap()[0]
            .encode(&[Token::Uint(9.into())])
            .unwrap();
        let rpc = json!({"error":{"code":3,"message":"reverted", "data":format!("0x{}", hex::encode(custom))}});
        assert!(decode_call(&function(vec![]), &abi, &rpc).contains("Denied(amount: 9)"));
    }

    #[test]
    fn receipt_is_not_a_call_return_and_unknown_logs_are_preserved() {
        let raw = json!({"transactionHash":format!("0x{}", "11".repeat(32)), "blockNumber":"0x10", "gasUsed":"0x5208", "status":"0x1", "logs":[{"topics":[format!("0x{}", "22".repeat(32))],"data":"0x"}]});
        let result = decode_receipt(&Abi::default(), &raw);
        assert_eq!(result.status, ReceiptStatus::Success);
        assert_eq!(result.block_number.as_deref(), Some("16"));
        assert_eq!(result.gas_used.as_deref(), Some("21000"));
        assert_eq!(result.log_count, Some(1));
        assert_eq!(result.unmatched_logs, 1);
        assert!(!result.to_string().contains("Return"));
        assert_eq!(
            decode_receipt(&Abi::default(), &Value::Null).status,
            ReceiptStatus::PendingOrUnavailable
        );
    }

    #[test]
    fn known_event_and_indexed_hash_are_decoded() {
        let abi: Abi = serde_json::from_value(json!([{ "type":"event", "name":"Changed", "anonymous":false, "inputs":[{"name":"who","type":"address","indexed":true},{"name":"value","type":"uint256","indexed":false},{"name":"tag","type":"string","indexed":true}] }])).unwrap();
        let event = &abi.events["Changed"][0];
        let address = format!("0x{}{}", "00".repeat(12), "11".repeat(20));
        let raw = json!({"status":"0x0", "logs":[{"topics":[format!("{:#x}", event.signature()),address,format!("0x{}", "22".repeat(32))],"data":format!("0x{}",hex::encode(encode(&[Token::Uint(42.into())])))}]});
        let result = decode_receipt(&abi, &raw);
        assert_eq!(result.status, ReceiptStatus::Reverted);
        assert_eq!(result.decoded_events.len(), 1);
        assert!(result.decoded_events[0].summary.contains("value: 42"));
        assert!(result.decoded_events[0].summary.contains("[indexed hash]"));
    }
}
