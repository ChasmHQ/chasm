use ethers::abi::{Constructor, Param, ParamType, Token};
use ethers::types::{Address, U256};
use ethers::utils::hex;
use serde_json::Value;

const MAX_INPUT_BYTES: usize = 64 * 1024;
const MAX_DEPTH: usize = 32;
const MAX_ELEMENTS: usize = 4096;

#[derive(Clone, Debug)]
pub struct ConstructorSpec {
    pub inputs: Vec<Param>,
    pub payable: bool,
}

impl ConstructorSpec {
    pub fn from_abi(abi: &Value) -> Result<Option<Self>, String> {
        let entries = abi.as_array().ok_or("ABI must be a JSON array.")?;
        let mut constructor = None;
        for entry in entries {
            let entry_type = entry
                .get("type")
                .and_then(Value::as_str)
                .ok_or("Every ABI entry must have a string type.")?;
            match entry_type {
                "constructor" => {}
                "function" | "event" | "error" | "fallback" | "receive" => continue,
                _ => return Err(format!("Unrecognized ABI entry type: {entry_type}.")),
            }
            if constructor.is_some() {
                return Err("ABI contains more than one constructor.".into());
            }
            let inputs = entry
                .get("inputs")
                .and_then(Value::as_array)
                .ok_or("Constructor inputs must be a JSON array.")?;
            let mut remaining = MAX_ELEMENTS;
            let inputs = inputs
                .iter()
                .enumerate()
                .map(|(index, input)| {
                    read_abi_param(input, 0, &mut remaining, true)
                        .map_err(|error| format!("Constructor input {}: {error}", index + 1))
                })
                .collect::<Result<Vec<_>, _>>()?;
            if let Some(outputs) = entry.get("outputs") {
                if !outputs.as_array().map_or(false, Vec::is_empty) {
                    return Err("A constructor cannot declare outputs.".into());
                }
            }
            constructor = Some(Self {
                inputs,
                payable: constructor_payable(entry)?,
            });
        }
        Ok(constructor)
    }
}

fn constructor_payable(entry: &Value) -> Result<bool, String> {
    let legacy = entry
        .get("payable")
        .map(|value| value.as_bool().ok_or("Constructor payable must be a boolean."))
        .transpose()?;
    let payable = match entry.get("stateMutability") {
        Some(Value::String(state)) if state == "payable" => true,
        Some(Value::String(state)) if state == "nonpayable" => false,
        Some(_) => {
            return Err("Constructor stateMutability must be payable or nonpayable.".into())
        }
        // Older ABIs may omit stateMutability. Without a payable flag, allow only zero wei.
        None => return Ok(legacy.unwrap_or(false)),
    };
    if legacy.map_or(false, |flag| flag != payable) {
        return Err("Constructor payable and stateMutability disagree.".into());
    }
    Ok(payable)
}

fn read_abi_param(
    value: &Value,
    depth: usize,
    remaining: &mut usize,
    require_name: bool,
) -> Result<Param, String> {
    if !value.is_object() {
        return Err("An ABI parameter must be an object.".into());
    }
    let name = match value.get("name") {
        Some(Value::String(name)) => name.clone(),
        None if !require_name => String::new(),
        _ => return Err("An ABI input name must be a string (it may be empty).".into()),
    };
    let internal_type = value
        .get("internalType")
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or("ABI internalType must be a string.")
        })
        .transpose()?;
    let type_name = value
        .get("type")
        .and_then(Value::as_str)
        .ok_or("An ABI parameter must have a string type.")?;
    if type_name.len() > MAX_INPUT_BYTES {
        return Err("ABI type exceeds 64 KiB.".into());
    }
    let base_end = type_name.find('[').unwrap_or(type_name.len());
    let (base, mut suffix) = type_name.split_at(base_end);
    let mut dimensions = Vec::new();
    while !suffix.is_empty() {
        if !suffix.starts_with('[') {
            return Err("Invalid ABI array type.".into());
        }
        let close = suffix.find(']').ok_or("Unclosed ABI array dimension.")?;
        let length = &suffix[1..close];
        dimensions.push(if length.is_empty() {
            None
        } else {
            let length = abi_size(length)?;
            if length == 0 {
                return Err("An ABI fixed array must have a positive length.".into());
            }
            Some(length)
        });
        if depth + dimensions.len() > MAX_DEPTH {
            return Err("ABI nesting exceeds 32 levels.".into());
        }
        suffix = &suffix[close + 1..];
    }
    take_elements(remaining, dimensions.len() + 1)?;
    let base_depth = depth + dimensions.len();
    let mut kind = match base {
        "address" => ParamType::Address,
        "bool" => ParamType::Bool,
        "string" => ParamType::String,
        "bytes" => ParamType::Bytes,
        "tuple" => {
            if base_depth >= MAX_DEPTH {
                return Err("ABI nesting exceeds 32 levels.".into());
            }
            let components = value
                .get("components")
                .and_then(Value::as_array)
                .ok_or("An ABI tuple must have an array of components.")?;
            let components = components
                .iter()
                .enumerate()
                .map(|(index, component)| {
                    read_abi_param(component, base_depth + 1, remaining, false)
                        .map(|param| param.kind)
                        .map_err(|error| format!("Tuple element {}: {error}", index + 1))
                })
                .collect::<Result<Vec<_>, _>>()?;
            ParamType::Tuple(components)
        }
        name if name.starts_with("uint") || name.starts_with("int") => {
            let unsigned = name.starts_with("uint");
            let width = &name[if unsigned { 4 } else { 3 }..];
            let bits = if width.is_empty() { 256 } else { abi_size(width)? };
            if !valid_integer_width(bits) {
                return Err("ABI integers must have a width of 8 to 256 bits, in steps of 8.".into());
            }
            if unsigned { ParamType::Uint(bits) } else { ParamType::Int(bits) }
        }
        name if name.starts_with("bytes") => {
            let length = abi_size(&name[5..])?;
            if !(1..=32).contains(&length) {
                return Err("ABI fixed bytes must have a length of 1 to 32 bytes.".into());
            }
            ParamType::FixedBytes(length)
        }
        _ => return Err(format!("Unsupported ABI parameter type: {base}.")),
    };
    if base != "tuple" && value.get("components").is_some() {
        return Err("Only tuple types may declare components.".into());
    }
    for dimension in dimensions {
        kind = match dimension {
            Some(length) => ParamType::FixedArray(Box::new(kind), length),
            None => ParamType::Array(Box::new(kind)),
        };
    }
    Ok(Param { name, kind, internal_type })
}

fn abi_size(text: &str) -> Result<usize, String> {
    if text.is_empty()
        || !text.bytes().all(|byte| byte.is_ascii_digit())
        || (text.len() > 1 && text.starts_with('0'))
    {
        return Err("ABI widths and lengths must be canonical decimal integers.".into());
    }
    text.parse().map_err(|_| "ABI width or length is too large.".into())
}

#[derive(Clone, Debug, Default)]
pub struct DeploymentDraft {
    pub values: Vec<String>,
    pub value_wei: String,
}

impl DeploymentDraft {
    pub fn new(spec: Option<&ConstructorSpec>) -> Self {
        Self {
            values: spec.map(|spec| vec![String::new(); spec.inputs.len()]).unwrap_or_default(),
            value_wei: "0".into(),
        }
    }
}

pub fn field_hint(kind: &ParamType) -> String {
    let detail = match kind {
        ParamType::Uint(bits) => format!("unsigned {bits}-bit integer, decimal or 0x hex"),
        ParamType::Int(bits) => {
            format!("signed {bits}-bit integer, decimal or 0x hex; use - for negatives")
        }
        ParamType::Bool => "true or false; blank is not a boolean".into(),
        ParamType::Address => "0x followed by exactly 40 hex digits (20 bytes)".into(),
        ParamType::Bytes => "0x followed by an even number of hex digits; 0x is empty bytes".into(),
        ParamType::FixedBytes(length) => {
            format!("0x followed by exactly {} hex digits ({length} bytes)", length.saturating_mul(2))
        }
        ParamType::String => "raw text; spaces are preserved and empty text is valid".into(),
        ParamType::Array(_) => "positional JSON array; [] is a valid empty array".into(),
        ParamType::FixedArray(_, length) => {
            format!("positional JSON array with exactly {length} elements")
        }
        ParamType::Tuple(elements) => {
            format!("positional JSON array with exactly {} tuple elements", elements.len())
        }
    };
    let mut hint = format!("{kind}: {detail}.");
    if is_composite(kind) {
        hint.push_str(" Nested arrays and tuples also use JSON arrays. Use JSON booleans and strings. Integers may be unquoted exact decimal numbers or quoted decimal/0x hex integers; no fractions or exponents.");
    }
    hint
}

pub fn field_placeholder(kind: &ParamType) -> String {
    let mut remaining = MAX_ELEMENTS;
    match placeholder_value(kind, 0, &mut remaining) {
        Some(Value::String(text)) => text,
        Some(value) => {
            let text = value.to_string();
            if text.len() <= MAX_INPUT_BYTES { text } else { "Positional JSON array".into() }
        }
        None => "Positional JSON array".into(),
    }
}

fn placeholder_value(kind: &ParamType, depth: usize, remaining: &mut usize) -> Option<Value> {
    if depth > MAX_DEPTH || (is_composite(kind) && depth == MAX_DEPTH) {
        return None;
    }
    Some(match kind {
        ParamType::Uint(_) | ParamType::Int(_) => Value::from(0),
        ParamType::Bool => Value::Bool(true),
        ParamType::Address => Value::String(format!("0x{}", "00".repeat(20))),
        ParamType::Bytes => Value::String("0x".into()),
        ParamType::FixedBytes(length) if (1..=32).contains(length) => {
            Value::String(format!("0x{}", "00".repeat(*length)))
        }
        ParamType::FixedBytes(_) => return None,
        ParamType::String => Value::String("Text with spaces".into()),
        ParamType::Array(inner) if matches!(inner.as_ref(), ParamType::Uint(_) | ParamType::Int(_)) => {
            take_elements(remaining, 3).ok()?;
            Value::Array(vec![Value::from(1), Value::from(2), Value::from(3)])
        }
        ParamType::Array(_) => Value::Array(Vec::new()),
        ParamType::FixedArray(inner, length) => {
            take_elements(remaining, *length).ok()?;
            Value::Array(
                (0..*length)
                    .map(|_| placeholder_value(inner, depth + 1, remaining))
                    .collect::<Option<Vec<_>>>()?,
            )
        }
        ParamType::Tuple(kinds) => {
            take_elements(remaining, kinds.len()).ok()?;
            Value::Array(
                kinds.iter()
                    .map(|kind| placeholder_value(kind, depth + 1, remaining))
                    .collect::<Option<Vec<_>>>()?,
            )
        }
    })
}

#[derive(Clone, Debug)]
pub struct DraftValidation {
    pub field_errors: Vec<String>,
    pub value_error: String,
    pub form_error: String,
}

impl DraftValidation {
    pub fn is_valid(&self) -> bool {
        self.field_errors.iter().all(String::is_empty)
            && self.value_error.is_empty()
            && self.form_error.is_empty()
    }

    fn message(&self) -> String {
        let mut errors = Vec::new();
        if !self.form_error.is_empty() {
            errors.push(self.form_error.clone());
        }
        for (index, error) in self.field_errors.iter().enumerate() {
            if !error.is_empty() {
                errors.push(format!("Constructor argument {}: {error}", index + 1));
            }
        }
        if !self.value_error.is_empty() {
            errors.push(format!("Value (wei): {}", self.value_error));
        }
        errors.join("\n")
    }
}

pub fn validate_draft(spec: Option<&ConstructorSpec>, draft: &DeploymentDraft) -> DraftValidation {
    parse_draft(spec, draft).0
}

fn parse_draft(
    spec: Option<&ConstructorSpec>,
    draft: &DeploymentDraft,
) -> (DraftValidation, Vec<Token>) {
    let inputs = spec.map(|spec| spec.inputs.as_slice()).unwrap_or(&[]);
    let mut validation = DraftValidation {
        field_errors: vec![String::new(); inputs.len()],
        value_error: String::new(),
        form_error: String::new(),
    };
    let mut form_errors = Vec::new();
    if inputs.len() != draft.values.len() {
        form_errors.push(format!(
            "Expected {} constructor arguments, received {}.",
            inputs.len(),
            draft.values.len()
        ));
    }
    let total_bytes = draft.values.iter().fold(0usize, |total, value| total.saturating_add(value.len()));
    if total_bytes > MAX_INPUT_BYTES {
        form_errors.push("Constructor input exceeds the total limit of 64 KiB.".into());
    }
    if inputs.len() > MAX_ELEMENTS {
        form_errors.push("Constructor has more than 4096 inputs.".into());
    }
    validation.form_error = form_errors.join(" ");
    let mut type_budget = MAX_ELEMENTS;
    let mut element_budget = MAX_ELEMENTS;
    let mut tokens = Vec::new();
    for (index, input) in inputs.iter().enumerate() {
        let result = check_kind(&input.kind, 0, &mut type_budget).and_then(|()| {
            let raw = draft.values.get(index).ok_or("Missing constructor argument.")?;
            if raw.len() > MAX_INPUT_BYTES {
                return Err("Input exceeds 64 KiB.".into());
            }
            if total_bytes > MAX_INPUT_BYTES || inputs.len() > MAX_ELEMENTS {
                return Ok(None);
            }
            parse_field(&input.kind, raw, &mut element_budget).map(Some)
        });
        match result {
            Ok(Some(token)) => tokens.push(token),
            Ok(None) => {}
            Err(error) => validation.field_errors[index] = error,
        }
    }
    validation.value_error = if draft.value_wei.len() > MAX_INPUT_BYTES {
        "Value input exceeds 64 KiB.".into()
    } else {
        match parse_integer(&draft.value_wei, 256, false) {
            Ok(value) if !value.is_zero() && !spec.map_or(false, |spec| spec.payable) => {
                "This constructor is nonpayable; value must be zero wei.".into()
            }
            Ok(_) => String::new(),
            Err(error) => error,
        }
    };
    (validation, tokens)
}

fn is_composite(kind: &ParamType) -> bool {
    matches!(kind, ParamType::Array(_) | ParamType::FixedArray(_, _) | ParamType::Tuple(_))
}

fn valid_integer_width(bits: usize) -> bool {
    (8..=256).contains(&bits) && bits % 8 == 0
}

fn take_elements(remaining: &mut usize, count: usize) -> Result<(), String> {
    if count > *remaining {
        return Err("Input exceeds the budget of 4096 elements.".into());
    }
    *remaining -= count;
    Ok(())
}

fn check_kind(kind: &ParamType, depth: usize, remaining: &mut usize) -> Result<(), String> {
    if depth > MAX_DEPTH || (is_composite(kind) && depth == MAX_DEPTH) {
        return Err("Type nesting exceeds 32 levels.".into());
    }
    take_elements(remaining, 1)?;
    match kind {
        ParamType::Uint(bits) | ParamType::Int(bits) if !valid_integer_width(*bits) => {
            Err("Integer type width must be 8 to 256 bits, in steps of 8.".into())
        }
        ParamType::FixedBytes(length) if !(1..=32).contains(length) => {
            Err("Fixed bytes length must be 1 to 32.".into())
        }
        ParamType::FixedArray(_, 0) => Err("A fixed array must have a positive length.".into()),
        ParamType::Array(inner) | ParamType::FixedArray(inner, _) => {
            check_kind(inner, depth + 1, remaining)
        }
        ParamType::Tuple(kinds) => {
            for kind in kinds {
                check_kind(kind, depth + 1, remaining)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

pub(super) fn parse_field(kind: &ParamType, raw: &str, remaining: &mut usize) -> Result<Token, String> {
    if is_composite(kind) {
        let value: Value = serde_json::from_str(raw)
            .map_err(|error| format!("Enter a positional JSON array: {error}"))?;
        parse_json_token(kind, &value, 0, remaining)
    } else {
        parse_scalar(kind, raw)
    }
}

fn parse_json_token(
    kind: &ParamType,
    value: &Value,
    depth: usize,
    remaining: &mut usize,
) -> Result<Token, String> {
    if depth > MAX_DEPTH || (is_composite(kind) && depth == MAX_DEPTH) {
        return Err("Input nesting exceeds 32 levels.".into());
    }
    match kind {
        ParamType::Array(inner) | ParamType::FixedArray(inner, _) => {
            let values = value.as_array().ok_or("Expected a JSON array.")?;
            if let ParamType::FixedArray(_, length) = kind {
                if values.len() != *length {
                    return Err(format!("Expected exactly {length} array elements, received {}.", values.len()));
                }
            }
            take_elements(remaining, values.len())?;
            let tokens = values
                .iter()
                .enumerate()
                .map(|(index, value)| {
                    parse_json_token(inner, value, depth + 1, remaining)
                        .map_err(|error| format!("Element {}: {error}", index + 1))
                })
                .collect::<Result<Vec<_>, _>>()?;
            if matches!(kind, ParamType::Array(_)) { Ok(Token::Array(tokens)) } else { Ok(Token::FixedArray(tokens)) }
        }
        ParamType::Tuple(kinds) => {
            let values = value.as_array().ok_or("Expected a positional JSON array for the tuple.")?;
            if values.len() != kinds.len() {
                return Err(format!("Expected exactly {} tuple elements, received {}.", kinds.len(), values.len()));
            }
            take_elements(remaining, values.len())?;
            let tokens = kinds
                .iter()
                .zip(values)
                .enumerate()
                .map(|(index, (kind, value))| {
                    parse_json_token(kind, value, depth + 1, remaining)
                        .map_err(|error| format!("Tuple element {}: {error}", index + 1))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Token::Tuple(tokens))
        }
        ParamType::Uint(_) | ParamType::Int(_) => match value {
            Value::String(text) => parse_scalar(kind, text),
            // With serde_json/arbitrary_precision this retains every digit, without an f64 hop.
            Value::Number(number) => parse_scalar(kind, &number.to_string()),
            _ => Err("Expected an integer JSON number or a quoted decimal/0x integer.".into()),
        },
        ParamType::Bool => value.as_bool().map(Token::Bool).ok_or_else(|| "Expected a JSON boolean (true or false).".into()),
        _ => {
            let text = value.as_str().ok_or("Expected a JSON string.")?;
            parse_scalar(kind, text)
        }
    }
}

fn parse_scalar(kind: &ParamType, raw: &str) -> Result<Token, String> {
    match kind {
        ParamType::Uint(bits) => parse_integer(raw, *bits, false).map(Token::Uint),
        ParamType::Int(bits) => parse_integer(raw, *bits, true).map(Token::Int),
        ParamType::String => Ok(Token::String(raw.to_owned())),
        ParamType::Bool => match raw.trim() {
            "true" => Ok(Token::Bool(true)),
            "false" => Ok(Token::Bool(false)),
            _ => Err("Enter true or false.".into()),
        },
        ParamType::Address => {
            let bytes = parse_hex(raw, Some(20))?;
            Ok(Token::Address(Address::from_slice(&bytes)))
        }
        ParamType::Bytes => parse_hex(raw, None).map(Token::Bytes),
        ParamType::FixedBytes(length) => parse_hex(raw, Some(*length)).map(Token::FixedBytes),
        _ => Err("Enter a positional JSON array.".into()),
    }
}

fn parse_integer(raw: &str, bits: usize, signed: bool) -> Result<U256, String> {
    if !valid_integer_width(bits) {
        return Err("Invalid integer width.".into());
    }
    let text = raw.trim();
    let negative = text.starts_with('-');
    if negative && !signed {
        return Err("Enter an unsigned integer, decimal or 0x hex.".into());
    }
    let digits = if negative { &text[1..] } else { text };
    let (digits, radix) = match strip_hex_prefix(digits) {
        Some(digits) => (digits, 16),
        None => (digits, 10),
    };
    if digits.is_empty()
        || !digits.bytes().all(|byte| {
            if radix == 16 { byte.is_ascii_hexdigit() } else { byte.is_ascii_digit() }
        })
    {
        return Err("Enter an integer in decimal or 0x hex, without fractions or exponents.".into());
    }
    let significant = digits.trim_start_matches('0');
    let magnitude = if significant.is_empty() {
        U256::zero()
    } else {
        U256::from_str_radix(significant, radix).map_err(|_| "Integer exceeds 256 bits.".to_owned())?
    };
    let in_range = if signed {
        let limit = U256::one() << (bits - 1);
        if negative { magnitude <= limit } else { magnitude < limit }
    } else {
        bits == 256 || magnitude < (U256::one() << bits)
    };
    if !in_range {
        let prefix = if signed { "int" } else { "uint" };
        return Err(format!("Integer is outside the {prefix}{bits} range."));
    }
    // ABI signed integers use a full 256-bit two's-complement word, even for int8.
    Ok(if negative { magnitude.overflowing_neg().0 } else { magnitude })
}

fn strip_hex_prefix(text: &str) -> Option<&str> {
    text.strip_prefix("0x").or_else(|| text.strip_prefix("0X"))
}

fn parse_hex(raw: &str, length: Option<usize>) -> Result<Vec<u8>, String> {
    let digits = strip_hex_prefix(raw.trim()).ok_or("Hex values must start with 0x.")?;
    if !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Hex values may contain only 0-9, a-f, or A-F.".into());
    }
    if digits.len() % 2 != 0 {
        return Err("Hex bytes require an even number of digits.".into());
    }
    if let Some(length) = length {
        if digits.len() / 2 != length {
            return Err(format!("Expected exactly {length} bytes ({} hex digits).", length.saturating_mul(2)));
        }
    }
    hex::decode(digits).map_err(|_| "Hex values may contain only 0-9, a-f, or A-F.".into())
}

pub fn prepare_creation(
    bytecode: &str,
    spec: Option<&ConstructorSpec>,
    draft: &DeploymentDraft,
) -> Result<Vec<u8>, String> {
    let (validation, tokens) = parse_draft(spec, draft);
    if !validation.is_valid() {
        return Err(validation.message());
    }
    let bytecode = bytecode.trim();
    let digits = strip_hex_prefix(bytecode).unwrap_or(bytecode);
    if digits.is_empty() {
        return Err("This contract has no creation bytecode.".into());
    }
    if !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Creation bytecode contains invalid hex.".into());
    }
    if digits.len() % 2 != 0 {
        return Err("Creation bytecode must have an even number of hex digits.".into());
    }
    let code = hex::decode(digits).map_err(|_| "Creation bytecode contains invalid hex.".to_owned())?;
    match spec {
        Some(spec) => Constructor { inputs: spec.inputs.clone() }
            .encode_input(code, &tokens)
            .map_err(|error| format!("Cannot encode constructor arguments: {error}")),
        None => Ok(code),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ethers::abi::{decode, encode};
    use serde_json::json;

    const CODE: &str = "0x60006000";
    const OWNER: &str = "0x1111111111111111111111111111111111111111";
    const SECOND: &str = "0x2222222222222222222222222222222222222222";
    const UINT_MAX: &str = "115792089237316195423570985008687907853269984665640564039457584007913129639935";
    const UINT_OVERFLOW: &str = "115792089237316195423570985008687907853269984665640564039457584007913129639936";

    fn param(name: &str, kind: ParamType) -> Param {
        Param { name: name.into(), kind, internal_type: None }
    }

    fn spec(kinds: Vec<ParamType>, payable: bool) -> ConstructorSpec {
        ConstructorSpec {
            inputs: kinds.into_iter().enumerate().map(|(index, kind)| param(&format!("arg{index}"), kind)).collect(),
            payable,
        }
    }

    fn banks_spec() -> ConstructorSpec {
        ConstructorSpec {
            inputs: vec![param("owner", ParamType::Address), param("amounts", ParamType::Array(Box::new(ParamType::Uint(256))))],
            payable: false,
        }
    }

    fn draft(values: &[&str], value_wei: &str) -> DeploymentDraft {
        DeploymentDraft { values: values.iter().map(|value| (*value).into()).collect(), value_wei: value_wei.into() }
    }

    fn decoded(spec: &ConstructorSpec, draft: &DeploymentDraft) -> Vec<Token> {
        let validation = validate_draft(Some(spec), draft);
        assert!(validation.is_valid(), "{validation:?}");
        let creation = prepare_creation(CODE, Some(spec), draft).unwrap();
        assert_eq!(&creation[..4], &[0x60, 0, 0x60, 0]);
        let kinds = spec.inputs.iter().map(|param| param.kind.clone()).collect::<Vec<_>>();
        decode(&kinds, &creation[4..]).unwrap()
    }

    fn valid(kind: ParamType, raw: &str) -> Token {
        decoded(&spec(vec![kind], false), &draft(&[raw], "0")).remove(0)
    }

    fn invalid(kind: ParamType, raw: &str) {
        let spec = spec(vec![kind], false);
        let draft = draft(&[raw], "0");
        let validation = validate_draft(Some(&spec), &draft);
        assert!(!validation.is_valid(), "Unexpected valid input: {raw:?}");
        assert!(!validation.field_errors[0].is_empty(), "{validation:?}");
        assert!(prepare_creation(CODE, Some(&spec), &draft).is_err());
    }

    fn array_text(length: usize, element: &str) -> String {
        format!("[{}]", vec![element; length].join(","))
    }

    #[test]
    fn constructor_absence_and_zero_arguments_are_distinct() {
        assert!(ConstructorSpec::from_abi(&json!([])).unwrap().is_none());
        assert!(ConstructorSpec::from_abi(&json!([{"type":"function","name":"f"}])).unwrap().is_none());
        let zero = ConstructorSpec::from_abi(&json!([{"type":"constructor","inputs":[],"stateMutability":"nonpayable"}])).unwrap().unwrap();
        assert!(zero.inputs.is_empty());
        assert!(!zero.payable);
        for spec in [None, Some(&zero)] {
            let draft = DeploymentDraft::new(spec);
            assert!(draft.values.is_empty());
            assert_eq!(draft.value_wei, "0");
            assert!(validate_draft(spec, &draft).is_valid());
            assert_eq!(prepare_creation(CODE, spec, &draft).unwrap(), vec![0x60, 0, 0x60, 0]);
        }
        let spec = banks_spec();
        let draft = DeploymentDraft::new(Some(&spec));
        assert_eq!(draft.values, vec!["", ""]);
        assert_eq!(draft.value_wei, "0");
        assert!(!validate_draft(Some(&spec), &draft).is_valid());
    }

    #[test]
    fn reads_constructor_metadata_and_nested_tuple_types() {
        let abi = json!([{"type":"constructor","stateMutability":"payable","inputs":[
            {"name":"owner","type":"address","internalType":"address"},
            {"name":"entries","type":"tuple[2][]","components":[
                {"type":"uint"}, {"name":"data","type":"bytes3"},
                {"type":"tuple","components":[{"type":"int8"}]}
            ]}
        ]}]);
        let spec = ConstructorSpec::from_abi(&abi).unwrap().unwrap();
        assert!(spec.payable);
        assert_eq!(spec.inputs[0].name, "owner");
        assert_eq!(spec.inputs[0].internal_type.as_deref(), Some("address"));
        assert_eq!(spec.inputs[0].kind, ParamType::Address);
        assert_eq!(spec.inputs[1].kind, ParamType::Array(Box::new(ParamType::FixedArray(Box::new(ParamType::Tuple(vec![
            ParamType::Uint(256), ParamType::FixedBytes(3), ParamType::Tuple(vec![ParamType::Int(8)])
        ])), 2))));
    }

    #[test]
    fn malformed_constructor_metadata_is_never_absence() {
        let cases = vec![
            Value::Null, json!({}), json!([null]), json!([{}]), json!([{"type":4}]),
            json!([{"type":"Constructor","inputs":[]}]),
            json!([{"type":"constructor"}]), json!([{"type":"constructor","inputs":null}]),
            json!([{"type":"constructor","inputs":{}}]),
            json!([{"type":"constructor","inputs":[null]}]),
            json!([{"type":"constructor","inputs":[{"name":"x"}]}]),
            json!([{"type":"constructor","inputs":[{"type":"uint256"}]}]),
            json!([{"type":"constructor","inputs":[{"name":1,"type":"uint256"}]}]),
            json!([{"type":"constructor","inputs":[{"name":"x","type":3}]}]),
            json!([{"type":"constructor","inputs":[{"name":"x","type":"tuple"}]}]),
            json!([{"type":"constructor","inputs":[{"name":"x","type":"tuple","components":null}]}]),
            json!([{"type":"constructor","inputs":[{"name":"x","type":"bool","components":[]}]}]),
            json!([{"type":"constructor","inputs":[{"name":"x","type":"bool","internalType":4}]}]),
            json!([{"type":"constructor","inputs":[],"stateMutability":"view"}]),
            json!([{"type":"constructor","inputs":[],"stateMutability":null}]),
            json!([{"type":"constructor","inputs":[],"payable":"true"}]),
            json!([{"type":"constructor","inputs":[],"payable":false,"stateMutability":"payable"}]),
            json!([{"type":"constructor","inputs":[],"outputs":[{"type":"bool"}]}]),
            json!([{"type":"constructor","inputs":[],"outputs":null}]),
            json!([{"type":"constructor","inputs":[]},{"type":"constructor","inputs":[]}]),
        ];
        for abi in cases {
            assert!(ConstructorSpec::from_abi(&abi).is_err(), "Unexpected valid ABI: {abi}");
        }
        for name in ["nonsense", "function", "uint0", "uint7", "uint264", "int257", "bytes0", "bytes33", "uint08", "bytes01", "bool[0]", "bool[-1]", "bool[2]junk", "bool[", "bool[9999999999999999999999999999999999]", "é"] {
            let abi = json!([{"type":"constructor","inputs":[{"name":"x","type":name}]}]);
            assert!(ConstructorSpec::from_abi(&abi).is_err(), "Unexpected valid type: {name}");
        }
    }

    #[test]
    fn legacy_payable_metadata_is_supported_without_guessing_nonzero_value() {
        for (entry, payable) in [
            (json!({"type":"constructor","inputs":[]}), false),
            (json!({"type":"constructor","inputs":[],"payable":false}), false),
            (json!({"type":"constructor","inputs":[],"payable":true}), true),
            (json!({"type":"constructor","inputs":[],"payable":true,"stateMutability":"payable"}), true),
        ] {
            assert_eq!(ConstructorSpec::from_abi(&json!([entry])).unwrap().unwrap().payable, payable);
        }
    }

    #[test]
    fn banks_owner_and_amounts_decode_in_exact_input_order() {
        let spec = banks_spec();
        let draft = draft(&[OWNER, "[3,9007199254740993,\"0xff\",0]"], "0");
        assert_eq!(decoded(&spec, &draft), vec![
            Token::Address(OWNER.parse().unwrap()),
            Token::Array(vec![Token::Uint(3.into()), Token::Uint(U256::from_dec_str("9007199254740993").unwrap()), Token::Uint(255.into()), Token::Uint(0.into())]),
        ]);
    }

    #[test]
    fn nested_tuples_arrays_fixed_bytes_and_dynamic_offsets_round_trip() {
        let kind = ParamType::Tuple(vec![
            ParamType::Array(Box::new(ParamType::Tuple(vec![ParamType::Address, ParamType::FixedArray(Box::new(ParamType::FixedBytes(2)), 2)]))),
            ParamType::FixedArray(Box::new(ParamType::Array(Box::new(ParamType::Int(8)))), 2),
            ParamType::String,
        ]);
        let raw = format!(r#"[[["{OWNER}",["0x1234","0xabcd"]],["{SECOND}",["0x0000","0xffff"]]],[[-128,"0x7f"],[]],"  nested text \t "]"#);
        let expected = Token::Tuple(vec![
            Token::Array(vec![
                Token::Tuple(vec![Token::Address(OWNER.parse().unwrap()), Token::FixedArray(vec![Token::FixedBytes(vec![0x12, 0x34]), Token::FixedBytes(vec![0xab, 0xcd])])]),
                Token::Tuple(vec![Token::Address(SECOND.parse().unwrap()), Token::FixedArray(vec![Token::FixedBytes(vec![0, 0]), Token::FixedBytes(vec![0xff, 0xff])])]),
            ]),
            Token::FixedArray(vec![Token::Array(vec![Token::Int(U256::from(128).overflowing_neg().0), Token::Int(127.into())]), Token::Array(Vec::new())]),
            Token::String("  nested text \t ".into()),
        ]);
        assert_eq!(valid(kind.clone(), &raw), expected);
        let spec = spec(vec![kind], false);
        let creation = prepare_creation(CODE, Some(&spec), &draft(&[&raw], "0")).unwrap();
        assert_eq!(&creation[4..], encode(&[expected]).as_slice());
    }

    #[test]
    fn empty_array_is_explicit_boolean_is_required_and_strings_are_raw() {
        let array = ParamType::Array(Box::new(ParamType::Uint(8)));
        assert_eq!(valid(array.clone(), "[]"), Token::Array(Vec::new()));
        invalid(array, "");
        invalid(ParamType::Bool, "");
        invalid(ParamType::Bool, " ");
        assert_eq!(valid(ParamType::Bool, " false "), Token::Bool(false));
        for text in ["", "  ", " \t hello \n ", "\"not JSON\""] {
            assert_eq!(valid(ParamType::String, text), Token::String(text.into()));
        }
        let spec = spec(vec![ParamType::String, ParamType::Uint(256)], false);
        let draft = draft(&["  text  ", " \t0x2a\n"], " 0x00 ");
        let original = draft.clone();
        assert!(validate_draft(Some(&spec), &draft).is_valid());
        prepare_creation(CODE, Some(&spec), &draft).unwrap();
        assert_eq!(draft.values, original.values);
        assert_eq!(draft.value_wei, original.value_wei);
    }

    #[test]
    fn uint256_maximum_is_exact_in_decimal_and_hex_and_overflow_is_rejected() {
        assert_eq!(valid(ParamType::Uint(256), UINT_MAX), Token::Uint(U256::max_value()));
        assert_eq!(valid(ParamType::Uint(256), &format!("0x{}", "f".repeat(64))), Token::Uint(U256::max_value()));
        invalid(ParamType::Uint(256), UINT_OVERFLOW);
        invalid(ParamType::Uint(256), &format!("0x1{}", "0".repeat(64)));
        assert_eq!(valid(ParamType::Uint(8), "255"), Token::Uint(255.into()));
        invalid(ParamType::Uint(8), "256");
        invalid(ParamType::Uint(8), "0x100");
        invalid(ParamType::Uint(256), "-1");
        invalid(ParamType::Uint(256), "-0");
        assert_eq!(valid(ParamType::Uint(8), &format!("0x{}ff", "0".repeat(100))), Token::Uint(255.into()));
    }

    #[test]
    fn signed_int8_and_int256_bounds_use_sign_extended_raw_tokens() {
        assert_eq!(valid(ParamType::Int(8), "127"), Token::Int(127.into()));
        let negative_128 = Token::Int(U256::max_value() - U256::from(127));
        assert_eq!(valid(ParamType::Int(8), "-128"), negative_128);
        assert_eq!(valid(ParamType::Int(8), "-0x80"), negative_128);
        assert_eq!(valid(ParamType::Int(8), "-0"), Token::Int(U256::zero()));
        for value in ["128", "-129", "0x80", "0xff", "-0x81"] { invalid(ParamType::Int(8), value); }
        let limit = U256::one() << 255;
        assert_eq!(valid(ParamType::Int(256), &(limit - U256::one()).to_string()), Token::Int(limit - U256::one()));
        assert_eq!(valid(ParamType::Int(256), &format!("-{limit}")), Token::Int(limit));
        assert_eq!(valid(ParamType::Int(256), &format!("-0x8{}", "0".repeat(63))), Token::Int(limit));
        invalid(ParamType::Int(256), &limit.to_string());
        invalid(ParamType::Int(256), &format!("-{}", limit + U256::one()));
        invalid(ParamType::Int(256), UINT_MAX);
    }

    #[test]
    fn json_integers_remain_exact_above_f64_and_u64_precision() {
        let kind = ParamType::Array(Box::new(ParamType::Uint(256)));
        let raw = format!(r#"[9007199254740993,18446744073709551617,{UINT_MAX},"{UINT_MAX}","0x{}"]"#, "f".repeat(64));
        assert_eq!(valid(kind.clone(), &raw), Token::Array(vec![
            Token::Uint(U256::from_dec_str("9007199254740993").unwrap()),
            Token::Uint(U256::from_dec_str("18446744073709551617").unwrap()),
            Token::Uint(U256::max_value()), Token::Uint(U256::max_value()), Token::Uint(U256::max_value()),
        ]));
        invalid(kind.clone(), &format!("[{UINT_OVERFLOW}]"));
        invalid(kind, &format!("[\"{UINT_OVERFLOW}\"]"));
        let minimum = U256::one() << 255;
        assert_eq!(valid(ParamType::Array(Box::new(ParamType::Int(256))), &format!("[-{minimum},\"-{minimum}\"]")), Token::Array(vec![Token::Int(minimum), Token::Int(minimum)]));
    }

    #[test]
    fn integer_grammar_rejects_fraction_exponent_nonfinite_and_bad_hex() {
        for raw in ["", "1.0", "1e3", "1E3", "NaN", "Infinity", "+1", "0x", "0xgg", "- 1", "1_000", "0b10", "１２"] {
            invalid(ParamType::Int(256), raw);
        }
        let kind = ParamType::Array(Box::new(ParamType::Uint(256)));
        for raw in ["[1.0]", "[1e3]", "[1e10000]", "[\"1e3\"]", "[\"1.0\"]", "[true]", "[null]", "[{}]", "[[]]"] {
            invalid(kind.clone(), raw);
        }
    }

    #[test]
    fn address_and_bytes_are_actual_hex_with_exact_lengths() {
        assert_eq!(valid(ParamType::Bytes, "0x"), Token::Bytes(Vec::new()));
        assert_eq!(valid(ParamType::Bytes, "0xaB00"), Token::Bytes(vec![0xab, 0]));
        assert_eq!(valid(ParamType::FixedBytes(2), "0x1234"), Token::FixedBytes(vec![0x12, 0x34]));
        for raw in ["", "1234", "0x1", "0xzz", "0x12 34", "0x0x1234", "0x0X1234"] { invalid(ParamType::Bytes, raw); }
        for raw in ["0x", "0x12", "0x123456", "0x12gg"] { invalid(ParamType::FixedBytes(2), raw); }
        for raw in ["", "0x1", "1111111111111111111111111111111111111111", "0x111111111111111111111111111111111111111", "0x111111111111111111111111111111111111111111", "0xgg11111111111111111111111111111111111111"] {
            invalid(ParamType::Address, raw);
        }
        for raw in ["1", "0", "TRUE", "False", "yes", "\"true\""] { invalid(ParamType::Bool, raw); }
    }

    #[test]
    fn json_types_fixed_lengths_and_tuple_arity_are_strict() {
        let fixed = ParamType::FixedArray(Box::new(ParamType::Bool), 2);
        assert_eq!(valid(fixed.clone(), "[true,false]"), Token::FixedArray(vec![Token::Bool(true), Token::Bool(false)]));
        for raw in ["[]", "[true]", "[true,false,true]", "[\"true\",false]", "[1,0]", "{}", "null", "\"[true,false]\""] {
            invalid(fixed.clone(), raw);
        }
        let tuple = ParamType::Tuple(vec![ParamType::String, ParamType::Array(Box::new(ParamType::Uint(8)))]);
        assert_eq!(valid(tuple.clone(), "[\"  text  \",[255,\"0x00\"]]"), Token::Tuple(vec![Token::String("  text  ".into()), Token::Array(vec![Token::Uint(255.into()), Token::Uint(0.into())])]));
        for raw in ["[]", "[\"text\"]", "[\"text\",[],1]", "{\"arg0\":\"text\",\"arg1\":[]}", "[true,[]]", "[\"text\",[256]]", "[\"text\",\"[]\"]"] {
            invalid(tuple.clone(), raw);
        }
        invalid(ParamType::Array(Box::new(ParamType::Address)), "[12]");
        invalid(ParamType::Array(Box::new(ParamType::Bytes)), "[[1,2]]");
        invalid(ParamType::Array(Box::new(ParamType::FixedBytes(2))), "[\"0x01\"]");
        let empty_tuple = spec(vec![ParamType::Tuple(Vec::new())], false);
        let empty_draft = draft(&["[]"], "0");
        let (validation, tokens) = parse_draft(Some(&empty_tuple), &empty_draft);
        assert!(validation.is_valid(), "{validation:?}");
        assert_eq!(tokens, vec![Token::Tuple(Vec::new())]);
        assert_eq!(
            prepare_creation(CODE, Some(&empty_tuple), &empty_draft).unwrap(),
            vec![0x60, 0, 0x60, 0]
        );
    }

    #[test]
    fn payable_and_nonpayable_value_validation_uses_uint256_wei() {
        let payable = spec(Vec::new(), true);
        let nonpayable = spec(Vec::new(), false);
        for spec in [None, Some(&nonpayable), Some(&payable)] {
            for zero in ["0", "0x00", " 000 "] {
                let draft = draft(&[], zero);
                assert!(validate_draft(spec, &draft).is_valid());
                assert!(prepare_creation(CODE, spec, &draft).is_ok());
            }
        }
        for spec in [None, Some(&nonpayable)] {
            let validation = validate_draft(spec, &draft(&[], "0x1"));
            assert!(!validation.is_valid());
            assert!(validation.value_error.contains("nonpayable"));
            assert!(prepare_creation(CODE, spec, &draft(&[], "1")).is_err());
        }
        for raw in ["1", "0x2a", UINT_MAX] {
            assert!(validate_draft(Some(&payable), &draft(&[], raw)).is_valid());
            assert_eq!(prepare_creation(CODE, Some(&payable), &draft(&[], raw)).unwrap(), vec![0x60, 0, 0x60, 0]);
        }
        for raw in ["", "-1", "+1", "1.0", "1e3", "0x", "0xzz", UINT_OVERFLOW] {
            let validation = validate_draft(Some(&payable), &draft(&[], raw));
            assert!(!validation.value_error.is_empty(), "Unexpected valid wei: {raw}");
            assert!(prepare_creation(CODE, Some(&payable), &draft(&[], raw)).is_err());
        }
    }

    #[test]
    fn argument_count_and_each_visible_field_have_validation() {
        let spec = spec(vec![ParamType::Uint(8), ParamType::Bool, ParamType::Address, ParamType::String], false);
        let validation = validate_draft(Some(&spec), &draft(&["256", "", "0xzz", ""], "-1"));
        assert_eq!(validation.field_errors.len(), 4);
        assert!(validation.field_errors[..3].iter().all(|error| !error.is_empty()));
        assert!(validation.field_errors[3].is_empty());
        assert!(!validation.value_error.is_empty());
        assert!(!validation.is_valid());
        let missing = validate_draft(Some(&spec), &draft(&["1", "true", OWNER], "0"));
        assert!(!missing.form_error.is_empty());
        assert!(!missing.field_errors[3].is_empty());
        let extra = draft(&["1", "true", OWNER, "", "extra"], "0");
        assert!(!validate_draft(Some(&spec), &extra).form_error.is_empty());
        assert!(prepare_creation(CODE, Some(&spec), &extra).is_err());
        let extra = draft(&["extra"], "0");
        assert!(!validate_draft(None, &extra).form_error.is_empty());
        assert!(prepare_creation(CODE, None, &extra).is_err());
    }

    #[test]
    fn validation_precedes_even_bytecode_parsing() {
        let spec = spec(vec![ParamType::Bool], false);
        let error = prepare_creation("not hex", Some(&spec), &draft(&[""], "1")).unwrap_err();
        assert!(error.contains("Constructor argument 1"));
        assert!(error.contains("nonpayable"));
        assert!(!error.contains("bytecode"));
    }

    #[test]
    fn creation_bytecode_is_strict_hex_with_an_optional_prefix() {
        let draft = DeploymentDraft::new(None);
        for code in ["60006000", "0X60006000", " \n0x60006000\t"] {
            assert_eq!(prepare_creation(code, None, &draft).unwrap(), vec![0x60, 0, 0x60, 0]);
        }
        for code in ["", "0x", " ", "0", "0x0", "0xgg", "0x60 00", "6000__", "0x0x6000"] {
            assert!(prepare_creation(code, None, &draft).is_err(), "Unexpected valid bytecode: {code:?}");
        }
    }

    #[test]
    fn input_size_limits_count_bytes_and_bound_the_whole_constructor() {
        let spec = spec(vec![ParamType::String], false);
        let maximum = "a".repeat(MAX_INPUT_BYTES);
        assert!(validate_draft(Some(&spec), &draft(&[&maximum], "0")).is_valid());
        assert!(prepare_creation(CODE, Some(&spec), &draft(&[&maximum], "0")).is_ok());
        invalid(ParamType::String, &"a".repeat(MAX_INPUT_BYTES + 1));
        invalid(ParamType::String, &"é".repeat(MAX_INPUT_BYTES / 2 + 1));
        let two = super::tests::spec(vec![ParamType::String, ParamType::String], false);
        let half = "a".repeat(MAX_INPUT_BYTES / 2);
        assert!(validate_draft(Some(&two), &draft(&[&half, &half], "0")).is_valid());
        let over = format!("{half}a");
        let validation = validate_draft(Some(&two), &draft(&[&half, &over], "0"));
        assert!(validation.form_error.contains("64 KiB"));
        assert!(prepare_creation(CODE, Some(&two), &draft(&[&half, &over], "0")).is_err());
        let wei = "0".repeat(MAX_INPUT_BYTES + 1);
        assert!(!validate_draft(None, &draft(&[], &wei)).value_error.is_empty());
    }

    #[test]
    fn nesting_limit_includes_arrays_and_tuples() {
        for depth in [MAX_DEPTH, MAX_DEPTH + 1] {
            let mut kind = ParamType::Uint(8);
            let mut raw = "0".to_owned();
            for index in 0..depth {
                kind = if index % 2 == 0 { ParamType::Array(Box::new(kind)) } else { ParamType::Tuple(vec![kind]) };
                raw = format!("[{raw}]");
            }
            let spec = spec(vec![kind], false);
            let draft = draft(&[&raw], "0");
            assert_eq!(validate_draft(Some(&spec), &draft).is_valid(), depth == MAX_DEPTH);
            assert_eq!(prepare_creation(CODE, Some(&spec), &draft).is_ok(), depth == MAX_DEPTH);
        }
        let raw = format!("{}0{}", "[".repeat(200), "]".repeat(200));
        invalid(ParamType::Array(Box::new(ParamType::Uint(8))), &raw);
    }

    #[test]
    fn element_budget_is_shared_across_arrays_tuples_and_fields() {
        let kind = ParamType::Array(Box::new(ParamType::Uint(8)));
        let maximum = array_text(MAX_ELEMENTS, "0");
        let spec = spec(vec![kind.clone()], false);
        assert!(validate_draft(Some(&spec), &draft(&[&maximum], "0")).is_valid());
        assert!(prepare_creation(CODE, Some(&spec), &draft(&[&maximum], "0")).is_ok());
        invalid(kind.clone(), &array_text(MAX_ELEMENTS + 1, "0"));
        let two = super::tests::spec(vec![kind.clone(), kind], false);
        let half = array_text(MAX_ELEMENTS / 2, "0");
        assert!(validate_draft(Some(&two), &draft(&[&half, &half], "0")).is_valid());
        let over = array_text(MAX_ELEMENTS / 2 + 1, "0");
        assert!(!validate_draft(Some(&two), &draft(&[&half, &over], "0")).is_valid());
        let nested = ParamType::Tuple(vec![ParamType::Array(Box::new(ParamType::Tuple(Vec::new())))]);
        invalid(nested, &format!("[{}]", array_text(MAX_ELEMENTS, "[]")));
    }

    #[test]
    fn malformed_public_paramtype_models_fail_even_inside_empty_arrays() {
        for kind in [ParamType::Uint(0), ParamType::Uint(264), ParamType::Int(7), ParamType::FixedBytes(0), ParamType::FixedBytes(33), ParamType::FixedArray(Box::new(ParamType::Bool), 0)] {
            invalid(ParamType::Array(Box::new(kind)), "[]");
        }
    }

    #[test]
    fn metadata_nesting_and_type_element_budgets_are_bounded() {
        for depth in [MAX_DEPTH, MAX_DEPTH + 1] {
            let name = format!("uint256{}", "[]".repeat(depth));
            let abi = json!([{"type":"constructor","inputs":[{"name":"x","type":name}]}]);
            assert_eq!(ConstructorSpec::from_abi(&abi).is_ok(), depth == MAX_DEPTH);
            let mut component = json!({"type":"uint256"});
            for _ in 0..depth { component = json!({"type":"tuple","components":[component]}); }
            component["name"] = json!("x");
            let abi = json!([{"type":"constructor","inputs":[component]}]);
            assert_eq!(ConstructorSpec::from_abi(&abi).is_ok(), depth == MAX_DEPTH);
        }
        let inputs = vec![json!({"name":"x","type":"bool"}); MAX_ELEMENTS + 1];
        assert!(ConstructorSpec::from_abi(&json!([{"type":"constructor","inputs":inputs}])).is_err());
        let too_many = spec(vec![ParamType::Bool; MAX_ELEMENTS + 1], false);
        let draft = DeploymentDraft::new(Some(&too_many));
        assert!(!validate_draft(Some(&too_many), &draft).is_valid());
    }

    #[test]
    fn hints_name_exact_types_and_placeholders_are_typed_examples() {
        let kinds = vec![
            ParamType::Uint(8), ParamType::Int(256), ParamType::Bool, ParamType::Address,
            ParamType::Bytes, ParamType::FixedBytes(3), ParamType::String,
            ParamType::Array(Box::new(ParamType::Uint(256))),
            ParamType::FixedArray(Box::new(ParamType::FixedBytes(2)), 2),
            ParamType::Tuple(vec![ParamType::String, ParamType::FixedArray(Box::new(ParamType::Bool), 2), ParamType::Array(Box::new(ParamType::Address))]),
        ];
        for kind in kinds {
            let hint = field_hint(&kind);
            assert!(hint.contains(&kind.to_string()));
            let placeholder = field_placeholder(&kind);
            let spec = spec(vec![kind], false);
            assert!(validate_draft(Some(&spec), &draft(&[&placeholder], "0")).is_valid(), "{placeholder}");
        }
        assert_eq!(field_placeholder(&ParamType::Bool), "true");
        assert_eq!(field_placeholder(&ParamType::Bytes), "0x");
        assert_eq!(field_placeholder(&ParamType::FixedBytes(3)), "0x000000");
        assert!(field_hint(&ParamType::String).contains("spaces are preserved"));
        assert!(field_hint(&ParamType::Array(Box::new(ParamType::Uint(256)))).contains("unquoted exact"));
    }
}
