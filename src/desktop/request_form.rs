//! Per-parameter request fields. The draft stays one positional JSON array in
//! `ContractView::arguments`, so the control API and the call service see the
//! same value the form shows; each field is one element of that array.
use ethers::abi::{Function, ParamType};
use ethers::types::U256;
use serde_json::Value;

#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub name: String,
    pub kind: String,
    pub value: String,
    pub hint: String,
    pub placeholder: String,
    pub error: String,
}

/// Split a stored draft into one text value per input. A draft that is not a
/// positional array of the right length (old free-text drafts) is shown in the
/// first field so nothing typed is lost.
pub fn split(function: &Function, arguments: &str) -> Vec<String> {
    let count = function.inputs.len();
    let mut values = vec![String::new(); count];
    if count == 0 || arguments.trim().is_empty() {
        return values;
    }
    match serde_json::from_str::<Vec<Value>>(arguments) {
        Ok(items) if items.len() == count => {
            for (slot, item) in values.iter_mut().zip(items) {
                *slot = match item {
                    Value::String(text) => text,
                    other => other.to_string(),
                };
            }
        }
        _ => values[0] = arguments.to_string(),
    }
    values
}

/// Join field texts back into the stored positional array. Composite inputs
/// that parse as JSON are kept as JSON; everything else is a JSON string, which
/// the call service parses by ABI type.
pub fn join(function: &Function, values: &[String]) -> String {
    if function.inputs.is_empty() || values.iter().all(|value| value.is_empty()) {
        return String::new();
    }
    let items: Vec<Value> = function
        .inputs
        .iter()
        .zip(values)
        .map(|(param, text)| match &param.kind {
            ParamType::Array(_) | ParamType::FixedArray(..) | ParamType::Tuple(_) => {
                serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.clone()))
            }
            _ => Value::String(text.clone()),
        })
        .collect();
    Value::Array(items).to_string()
}

pub fn fields(function: &Function, arguments: &str) -> Vec<Field> {
    let values = split(function, arguments);
    function
        .inputs
        .iter()
        .zip(values)
        .enumerate()
        .map(|(index, (param, value))| {
            let mut budget = 4096;
            let error = if value.is_empty() {
                String::new()
            } else {
                super::deployment::parse_field(&param.kind, &value, &mut budget).err().unwrap_or_default()
            };
            Field {
                name: if param.name.is_empty() { format!("arg{}", index + 1) } else { param.name.clone() },
                kind: param.kind.to_string(),
                value,
                hint: super::deployment::field_hint(&param.kind),
                placeholder: super::deployment::field_placeholder(&param.kind),
                error,
            }
        })
        .collect()
}

pub const UNITS: [&str; 3] = ["wei", "gwei", "ether"];

/// Exact decimal amount in a unit to wei. Fractions are allowed down to one wei.
pub fn to_wei(amount: &str, unit: usize) -> Result<String, String> {
    let amount = amount.trim();
    if amount.is_empty() {
        return Ok("0".into());
    }
    let decimals = [0usize, 9, 18][unit.min(2)];
    let (whole, fraction) = amount.split_once('.').unwrap_or((amount, ""));
    if whole.is_empty() && fraction.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err("Enter a plain decimal amount, for example 1.5.".into());
    }
    if fraction.len() > decimals {
        return Err(format!("At most {decimals} decimal places in {}.", UNITS[unit.min(2)]));
    }
    let digits = format!("{whole}{fraction:0<decimals$}");
    let digits = digits.trim_start_matches('0');
    let wei = if digits.is_empty() { U256::zero() } else { U256::from_dec_str(digits).map_err(|_| "Amount is larger than uint256.".to_string())? };
    Ok(wei.to_string())
}

/// Wei back to the largest unit with a short exact decimal, for display.
pub fn from_wei(wei: &str) -> (String, usize) {
    let Ok(value) = U256::from_dec_str(wei.trim()) else { return (wei.to_string(), 0) };
    if value.is_zero() {
        return ("0".into(), 0);
    }
    for unit in [2usize, 1] {
        let decimals = [0, 9, 18][unit];
        let scale = U256::exp10(decimals);
        let whole = value / scale;
        let fraction = format!("{:0>decimals$}", (value % scale).to_string());
        let fraction = fraction.trim_end_matches('0');
        // Keep it readable: at most six decimals, and at least 1 in that unit.
        if !whole.is_zero() && fraction.len() <= 6 {
            let text = if fraction.is_empty() { whole.to_string() } else { format!("{whole}.{fraction}") };
            return (text, unit);
        }
    }
    (value.to_string(), 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ethers::abi::{Param, StateMutability};

    fn function(inputs: &[(&str, ParamType)]) -> Function {
        #[allow(deprecated)]
        Function {
            name: "f".into(),
            inputs: inputs.iter().map(|(name, kind)| Param { name: (*name).into(), kind: kind.clone(), internal_type: None }).collect(),
            outputs: vec![],
            constant: None,
            state_mutability: StateMutability::NonPayable,
        }
    }

    #[test]
    fn fields_round_trip_through_the_stored_positional_array() {
        let f = function(&[("to", ParamType::Address), ("amounts", ParamType::Array(Box::new(ParamType::Uint(256)))), ("note", ParamType::String)]);
        let joined = join(&f, &["0x0000000000000000000000000000000000000001".into(), "[1,2]".into(), "hi, \"there\"".into()]);
        assert_eq!(joined, r#"["0x0000000000000000000000000000000000000001",[1,2],"hi, \"there\""]"#);
        let back = split(&f, &joined);
        assert_eq!(back, vec!["0x0000000000000000000000000000000000000001", "[1,2]", "hi, \"there\""]);
        let shown = fields(&f, &joined);
        assert!(shown.iter().all(|field| field.error.is_empty()), "{shown:?}");
        assert!(!fields(&f, r#"["0x12",[1],""]"#)[0].error.is_empty());
        // A legacy free-text draft is kept, not dropped.
        assert_eq!(split(&f, "0x1 0x2")[0], "0x1 0x2");
        assert_eq!(join(&f, &[String::new(), String::new(), String::new()]), "");
    }

    #[test]
    fn units_convert_exactly() {
        assert_eq!(to_wei("1.5", 2).unwrap(), "1500000000000000000");
        assert_eq!(to_wei("2", 1).unwrap(), "2000000000");
        assert_eq!(to_wei("", 2).unwrap(), "0");
        assert_eq!(to_wei("0.000000000000000001", 2).unwrap(), "1");
        assert!(to_wei("0.0000000000000000001", 2).is_err());
        assert!(to_wei("1e18", 0).is_err());
        assert!(to_wei("-1", 2).is_err());
        assert_eq!(from_wei("2000000000000000000"), ("2".into(), 2));
        assert_eq!(from_wei("3000000000"), ("3".into(), 1));
        assert_eq!(from_wei("17"), ("17".into(), 0));
        assert_eq!(from_wei("0"), ("0".into(), 0));
        assert_eq!(from_wei("1500000000000000000"), ("1.5".into(), 2));
        // 1.500000001 gwei needs nine decimals, so it stays in wei.
        assert_eq!(from_wei("1500000001"), ("1500000001".into(), 0));
    }
}
