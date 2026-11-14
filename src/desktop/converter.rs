use ethers::abi::{AbiParser, FunctionExt};
use ethers::types::U256;
use ethers::utils::{hex, keccak256};

const MAX_INPUT_BYTES: usize = 64 * 1024;
const OVERFLOW: &str = "The value exceeds the uint256 range.";
const ARRAY_BUDGET: &str = "The signature exceeds the fixed array element budget.";
const MAX_SIGNATURE_DEPTH: usize = 32;
const MAX_ARRAY_ELEMENTS: u64 = 1 << 20;

pub fn convert(tool: i32, mode: i32, raw: &str) -> Result<String, String> {
    if raw.len() > MAX_INPUT_BYTES {
        return Err("Input exceeds the 64 KiB converter limit.".into());
    }
    match tool {
        0 => hex_convert(mode, raw),
        1 => units_convert(mode, raw),
        2 => keccak_convert(mode, raw),
        3 => selector_convert(mode, raw),
        _ => Err("Unknown converter tool.".into()),
    }
}

pub fn title(tool: i32) -> &'static str {
    match tool {
        1 => "Units",
        2 => "Keccak",
        3 => "Selector",
        _ => "Hex",
    }
}

pub fn modes(tool: i32) -> Vec<&'static str> {
    match tool {
        1 => vec!["Wei", "Gwei", "Ether"],
        2 => vec!["Text", "Hex bytes"],
        3 => vec!["Signature"],
        _ => vec![
            "Decimal to hex",
            "Hex to decimal",
            "Text to hex",
            "Hex to text",
        ],
    }
}

pub fn hint(tool: i32, mode: i32) -> &'static str {
    match (tool, mode) {
        (0, 0) => "Enter a decimal integer up to 2^256-1. No signs, separators, or exponents.",
        (0, 1) => "Enter hex, 0x prefix optional. The result is decimal.",
        (0, 2) => "Encodes the text exactly as typed, whitespace included, into UTF-8 hex. Empty text gives 0x.",
        (0, 3) => "Decodes even-length hex, 0x prefix optional, into UTF-8 text.",
        (1, 0) => "Enter an integer wei amount. The result lists wei, gwei, and ether exactly.",
        (1, 1) => "Enter gwei with up to 9 decimals. Values that are not exact at 9 decimals are rejected.",
        (1, 2) => "Enter ether with up to 18 decimals. Values that are not exact at 18 decimals are rejected.",
        (2, 0) => "Keccak-256 of the text exactly as typed, whitespace included. Empty input is valid.",
        (2, 1) => "Keccak-256 of raw bytes. Paste even-length hex, 0x prefix optional.",
        (3, 0) => "Paste a canonical signature such as transfer(address,uint256). The result is the first 4 bytes of its keccak-256 hash.",
        _ => "Pick a mode for this tool.",
    }
}

fn hex_convert(mode: i32, raw: &str) -> Result<String, String> {
    match mode {
        0 => parse_decimal(raw.trim()).map(|value| format!("{value:#x}")),
        1 => parse_hex_number(raw.trim()).map(|value| value.to_string()),
        2 => Ok(format!("0x{}", hex::encode(raw.as_bytes()))),
        3 => parse_hex_bytes(raw.trim()).and_then(|bytes| {
            String::from_utf8(bytes).map_err(|_| "The bytes are not valid UTF-8.".into())
        }),
        _ => Err("Hex has four modes (0 to 3).".into()),
    }
}

fn units_convert(mode: i32, raw: &str) -> Result<String, String> {
    let (decimals, unit) = match mode {
        0 => (0, "wei"),
        1 => (9, "gwei"),
        2 => (18, "ether"),
        _ => return Err("Units takes wei, gwei, or ether input (modes 0 to 2).".into()),
    };
    let value = parse_units(raw.trim(), decimals, unit)?;
    Ok(format!(
        "wei {}\ngwei {}\nether {}",
        value,
        format_units(value, 9),
        format_units(value, 18)
    ))
}

fn keccak_convert(mode: i32, raw: &str) -> Result<String, String> {
    let bytes = match mode {
        0 => raw.as_bytes().to_vec(),
        1 => parse_hex_bytes(raw.trim())?,
        _ => return Err("Keccak has two modes (0 text, 1 hex bytes).".into()),
    };
    Ok(format!("0x{}", hex::encode(keccak256(bytes))))
}

fn selector_convert(mode: i32, raw: &str) -> Result<String, String> {
    if mode != 0 {
        return Err("Selector takes a signature in mode 0.".into());
    }
    let signature = raw.trim();
    if signature.is_empty() {
        return Err("Enter a function signature.".into());
    }
    // Canonical `name(types)` only: the parser below also accepts `function`
    // prefixes, `returns` clauses, named parameters, and aliases like `uint`.
    if signature.contains(char::is_whitespace) {
        return Err(
            "Use the canonical form without whitespace, e.g. transfer(address,uint256).".into(),
        );
    }
    let Some((name, tail)) = signature.split_once('(') else {
        return Err("A signature looks like name(type,type).".into());
    };
    if !tail.ends_with(')') {
        return Err("A signature looks like name(type,type).".into());
    }
    if !is_function_name(name) {
        return Err("The signature must start with a function name.".into());
    }
    validate_signature_shape(signature)?;
    let function = AbiParser::default()
        .parse_function(signature)
        .map_err(|_| format!("Not a parsable signature: {signature}."))?;
    // The parser coerces aliases and tolerates non-canonical spellings, so
    // require its standard signature to match the input byte for byte.
    if function.abi_signature() != signature {
        return Err(format!(
            "Not canonical. Enter the ABI spelling, e.g. transfer(address,uint256), not {signature}."
        ));
    }
    Ok(format!("0x{}", hex::encode(function.short_signature())))
}

// Bounds the parser before it runs: nesting depth, balanced delimiters, no
// text after the outermost ')', and a cap on fixed array elements.
fn validate_signature_shape(signature: &str) -> Result<(), String> {
    let mut depth = 0usize;
    let mut array_elements = 1u64;
    let mut dim: Option<u64> = None;
    let mut closed = false;
    for c in signature.chars() {
        if closed {
            return Err("Unexpected text after the signature.".into());
        }
        match c {
            '(' | '[' => {
                depth += 1;
                if depth > MAX_SIGNATURE_DEPTH {
                    return Err("The signature nests more than 32 levels deep.".into());
                }
                if c == '[' {
                    dim = Some(0);
                }
            }
            ')' | ']' => {
                depth = depth
                    .checked_sub(1)
                    .ok_or("Unbalanced parentheses or brackets.")?;
                match (c, dim.take()) {
                    (')', _) => {
                        if depth == 0 {
                            closed = true;
                        }
                    }
                    (']', Some(size)) if size > 0 => {
                        array_elements = array_elements
                            .checked_mul(size)
                            .filter(|total| *total <= MAX_ARRAY_ELEMENTS)
                            .ok_or(ARRAY_BUDGET)?;
                    }
                    _ => {}
                }
            }
            c @ '0'..='9' if dim.is_some() => {
                let digit = (c as u8 - b'0') as u64;
                dim = Some(
                    dim.unwrap_or(0)
                        .checked_mul(10)
                        .and_then(|v| v.checked_add(digit))
                        .ok_or("Array size is too large.")?,
                );
            }
            _ if dim.is_some() => return Err("Array sizes must be plain numbers.".into()),
            _ => {}
        }
    }
    if depth != 0 {
        return Err("Unbalanced parentheses or brackets.".into());
    }
    Ok(())
}

fn is_function_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn parse_decimal(raw: &str) -> Result<U256, String> {
    if raw.is_empty() {
        return Err("Enter a decimal integer.".into());
    }
    if !raw.bytes().all(|b| b.is_ascii_digit()) {
        return Err("Decimal digits only. No signs, separators, or exponents.".into());
    }
    fold_digits(raw, 10)
}

fn parse_hex_number(raw: &str) -> Result<U256, String> {
    let digits = strip_hex_prefix(raw);
    if digits.is_empty() {
        return Err("Enter a hex value.".into());
    }
    if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Hex digits only.".into());
    }
    if digits.len() > 64 {
        return Err(OVERFLOW.into());
    }
    fold_digits(digits, 16)
}

fn fold_digits(digits: &str, radix: u32) -> Result<U256, String> {
    let radix_value = U256::from(radix as u64);
    let mut value = U256::zero();
    for byte in digits.bytes() {
        let digit = match (byte as char).to_digit(radix) {
            Some(d) => U256::from(d as u64),
            None => return Err("Digits do not match the base.".into()),
        };
        value = value
            .checked_mul(radix_value)
            .and_then(|v| v.checked_add(digit))
            .ok_or(OVERFLOW)?;
    }
    Ok(value)
}

fn parse_hex_bytes(raw: &str) -> Result<Vec<u8>, String> {
    let digits = strip_hex_prefix(raw);
    if digits.len() % 2 != 0 {
        return Err("Use an even number of hex digits.".into());
    }
    hex::decode(digits).map_err(|_| "Hex digits only.".into())
}

fn strip_hex_prefix(raw: &str) -> &str {
    raw.strip_prefix("0x")
        .or_else(|| raw.strip_prefix("0X"))
        .unwrap_or(raw)
}

fn parse_units(raw: &str, decimals: u32, unit: &str) -> Result<U256, String> {
    let (whole_part, frac_part) = match raw.split_once('.') {
        Some(parts) => parts,
        None => (raw, ""),
    };
    for part in [whole_part, frac_part] {
        if !part.bytes().all(|b| b.is_ascii_digit()) {
            return Err(format!(
                "{unit} amounts are plain decimals. No signs, separators, or exponents."
            ));
        }
    }
    if whole_part.is_empty() && frac_part.is_empty() {
        return Err(format!("Enter a {unit} amount."));
    }
    // Trailing fractional zeros carry no value, so "1.500" gwei stays exact.
    let frac = frac_part.trim_end_matches('0');
    if frac.len() > decimals as usize {
        return Err(format!(
            "{unit} keeps at most {decimals} decimals, so this value is not exact."
        ));
    }
    let whole = fold_digits(
        if whole_part.is_empty() {
            "0"
        } else {
            whole_part
        },
        10,
    )?;
    let frac_value = fold_digits(if frac.is_empty() { "0" } else { frac }, 10)?;
    let scaled_frac = frac_value
        .checked_mul(pow10(decimals as usize - frac.len()))
        .ok_or(OVERFLOW)?;
    whole
        .checked_mul(pow10(decimals as usize))
        .and_then(|v| v.checked_add(scaled_frac))
        .ok_or_else(|| OVERFLOW.to_string())
}

fn format_units(value: U256, decimals: u32) -> String {
    let divisor = pow10(decimals as usize);
    let whole = value / divisor;
    let frac = value % divisor;
    if frac.is_zero() {
        return whole.to_string();
    }
    let frac_str = frac.to_string();
    let mut padded = String::with_capacity(decimals as usize);
    for _ in frac_str.len()..decimals as usize {
        padded.push('0');
    }
    padded.push_str(&frac_str);
    while padded.ends_with('0') {
        padded.pop();
    }
    format!("{whole}.{padded}")
}

fn pow10(n: usize) -> U256 {
    let mut value = U256::one();
    for _ in 0..n {
        value *= U256::from(10u64);
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    const EMPTY_KECCAK: &str = "0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470";
    const UINT256_MAX_DECIMAL: &str =
        "115792089237316195423570985008687907853269984665640564039457584007913129639935";
    // One past the uint256 maximum: 2^256.
    const UINT256_OVERFLOW_DECIMAL: &str =
        "115792089237316195423570985008687907853269984665640564039457584007913129639936";

    #[test]
    fn decimal_to_hex_is_exact() {
        assert_eq!(convert(0, 0, "255"), Ok("0xff".to_string()));
        assert_eq!(convert(0, 0, "0017"), Ok("0x11".to_string()));
        assert_eq!(convert(0, 0, " 0 "), Ok("0x0".to_string()));
        assert_eq!(
            convert(0, 0, UINT256_MAX_DECIMAL),
            Ok(format!("0x{}", "f".repeat(64)))
        );
        assert!(convert(0, 0, UINT256_OVERFLOW_DECIMAL).is_err());
    }

    #[test]
    fn decimal_rejects_signs_and_exponents() {
        for bad in ["-1", "1e3", "1.5", "12 3", "+1", "", "0x10"] {
            assert!(convert(0, 0, bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn hex_to_decimal_is_exact() {
        assert_eq!(convert(0, 1, "0xff"), Ok("255".to_string()));
        assert_eq!(convert(0, 1, "Ff"), Ok("255".to_string()));
        assert_eq!(convert(0, 1, "0x0"), Ok("0".to_string()));
        assert_eq!(convert(0, 1, "0xf"), Ok("15".to_string()));
        assert!(convert(0, 1, &"f".repeat(65)).is_err());
    }

    #[test]
    fn text_hex_preserves_whitespace() {
        assert_eq!(convert(0, 2, "hi"), Ok("0x6869".to_string()));
        assert_eq!(convert(0, 2, "a b"), Ok("0x612062".to_string()));
        assert_eq!(convert(0, 2, " x "), Ok("0x207820".to_string()));
        assert_eq!(convert(0, 2, ""), Ok("0x".to_string()));
    }

    #[test]
    fn hex_to_text_requires_utf8() {
        assert_eq!(convert(0, 3, "0x6869"), Ok("hi".to_string()));
        assert_eq!(convert(0, 3, "6869"), Ok("hi".to_string()));
        assert_eq!(convert(0, 3, "0x"), Ok(String::new()));
        assert!(convert(0, 3, "zz").is_err());
        assert!(convert(0, 3, "abc").is_err());
        assert!(convert(0, 3, "ff").is_err());
    }

    #[test]
    fn units_gwei_fraction_is_exact() {
        assert_eq!(
            convert(1, 1, "0.000000001"),
            Ok("wei 1\ngwei 0.000000001\nether 0.000000000000000001".to_string())
        );
        assert_eq!(
            convert(1, 0, "1000000001"),
            Ok("wei 1000000001\ngwei 1.000000001\nether 0.000000001000000001".to_string())
        );
    }

    #[test]
    fn units_trailing_zeros_stay_exact() {
        assert_eq!(
            convert(1, 1, "1.500"),
            Ok("wei 1500000000\ngwei 1.5\nether 0.0000000015".to_string())
        );
        assert_eq!(
            convert(1, 0, "1.0"),
            Ok("wei 1\ngwei 0.000000001\nether 0.000000000000000001".to_string())
        );
    }

    #[test]
    fn units_ether_input_is_exact() {
        assert_eq!(
            convert(1, 2, "1.5"),
            Ok("wei 1500000000000000000\ngwei 1500000000\nether 1.5".to_string())
        );
    }

    #[test]
    fn units_reject_loss_of_precision() {
        assert!(convert(1, 1, "1.0000000001").is_err());
        assert!(convert(1, 2, "0.0000000000000000001").is_err());
        assert!(convert(1, 0, "1.5").is_err());
    }

    #[test]
    fn units_reject_signs_exponents_and_overflow() {
        for bad in ["-1", "1e9", "1.5.2", "", ".", "+2"] {
            assert!(convert(1, 1, bad).is_err(), "{bad}");
        }
        assert!(convert(1, 0, UINT256_MAX_DECIMAL).is_ok());
        assert!(convert(1, 0, UINT256_OVERFLOW_DECIMAL).is_err());
    }

    #[test]
    fn keccak_empty_input_is_valid() {
        assert_eq!(convert(2, 0, ""), Ok(EMPTY_KECCAK.to_string()));
        assert_eq!(convert(2, 1, "0x"), Ok(EMPTY_KECCAK.to_string()));
        assert_eq!(convert(2, 0, "hi"), convert(2, 1, "0x6869"));
    }

    #[test]
    fn selector_transfer_is_known() {
        assert_eq!(
            convert(3, 0, "transfer(address,uint256)"),
            Ok("0xa9059cbb".to_string())
        );
        assert_eq!(
            convert(3, 0, "  transfer(address,uint256)  "),
            Ok("0xa9059cbb".to_string())
        );
    }

    #[test]
    fn selector_rejects_garbage() {
        for bad in [
            "",
            "transfer(uint256) returns (bool)",
            "function transfer(uint256)",
            "foo(bar)",
            "0x12345678",
            "()",
            "9mint(uint256)",
            "foo(uint)",
            "foo()(uint256)",
            "foo(uint256",
            "foo(uint256[1048577])",
        ] {
            assert!(convert(3, 0, bad).is_err(), "{bad}");
        }
        let deep = format!("f{}{}", "(".repeat(34), ")".repeat(34));
        assert!(convert(3, 0, &deep).is_err());
    }

    #[test]
    fn oversized_input_is_rejected() {
        let too_big = "a".repeat(MAX_INPUT_BYTES + 1);
        assert!(convert(2, 0, &too_big).is_err());
        let at_limit = "a".repeat(MAX_INPUT_BYTES);
        assert_eq!(
            convert(0, 2, &at_limit),
            Ok(format!("0x{}", "61".repeat(MAX_INPUT_BYTES)))
        );
    }

    #[test]
    fn metadata_covers_tools_and_modes() {
        assert_eq!(title(0), "Hex");
        assert_eq!(title(1), "Units");
        assert_eq!(title(2), "Keccak");
        assert_eq!(title(3), "Selector");
        assert_eq!(modes(0).len(), 4);
        assert_eq!(modes(1).len(), 3);
        assert_eq!(modes(2).len(), 2);
        assert_eq!(modes(3).len(), 1);
        assert!(!hint(0, 0).is_empty());
        assert!(convert(9, 0, "1").is_err());
        assert!(convert(0, 9, "1").is_err());
        assert!(convert(1, 9, "1").is_err());
        assert!(convert(2, 9, "1").is_err());
        assert!(convert(3, 9, "1").is_err());
    }
}
