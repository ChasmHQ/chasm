//! Breakpoint conditions over a recorded step: `$0 > 5`, `pc == 0x1a`,
//! `balanceOf[0xf39F…] >= 1000 && depth == 2`. Operands are numbers, stack
//! words (`$i`, `stack[i]`), `pc`, `gas`, `depth`, or a state variable of
//! the code running at that step, read with the storage layout. Comparisons
//! chain left to right with `&&` and `||`; there is no grouping.
use super::trace_debug::Session;
use ethers::types::U256;

const OPERATORS: [&str; 6] = ["==", "!=", "<=", ">=", "<", ">"];

/// Checks the syntax without a step to evaluate it on.
pub fn parse(text: &str) -> Result<(), String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Write a condition, e.g. $0 > 5 or balanceOf[0x…] == 0.".into());
    }
    for (_, part) in parts(text) {
        let (left, _, right) = split(part);
        operand_syntax(left)?;
        if let Some(right) = right {
            operand_syntax(right)?;
        }
    }
    Ok(())
}

/// The condition at `index`; an operand that cannot be read there is an error.
pub fn eval(session: &Session, index: usize, text: &str) -> Result<bool, String> {
    let mut result: Option<bool> = None;
    for (join, part) in parts(text.trim()) {
        let (left, operator, right) = split(part);
        let left = operand(session, index, left)?;
        let value = match (operator, right) {
            (Some(operator), Some(right)) => {
                let right = operand(session, index, right)?;
                match operator {
                    "==" => left == right,
                    "!=" => left != right,
                    "<=" => left <= right,
                    ">=" => left >= right,
                    "<" => left < right,
                    _ => left > right,
                }
            }
            _ => !left.is_zero(),
        };
        result = Some(match (result, join) {
            (None, _) => value,
            (Some(previous), "||") => previous || value,
            (Some(previous), _) => previous && value,
        });
    }
    Ok(result.unwrap_or(false))
}

/// (joiner before, comparison) pairs; the first joiner is empty.
fn parts(text: &str) -> Vec<(&str, &str)> {
    let mut out = Vec::new();
    let mut join = "";
    let mut rest = text;
    loop {
        let next = [rest.find("&&").map(|at| (at, "&&")), rest.find("||").map(|at| (at, "||"))].into_iter().flatten().min_by_key(|(at, _)| *at);
        match next {
            Some((at, found)) => {
                out.push((join, rest[..at].trim()));
                join = found;
                rest = &rest[at + 2..];
            }
            None => {
                out.push((join, rest.trim()));
                return out;
            }
        }
    }
}

fn split(part: &str) -> (&str, Option<&'static str>, Option<&str>) {
    // A comparison's operator sits outside brackets, so a mapping key such as
    // `m[a<b]` is not split. Two-character operators are tried first.
    let mut depth = 0i32;
    let bytes = part.as_bytes();
    for at in 0..bytes.len() {
        match bytes[at] {
            b'[' => depth += 1,
            b']' => depth -= 1,
            _ if depth == 0 => {
                for operator in OPERATORS {
                    if part[at..].starts_with(operator) {
                        return (part[..at].trim(), Some(operator), Some(part[at + operator.len()..].trim()));
                    }
                }
            }
            _ => {}
        }
    }
    (part.trim(), None, None)
}

fn operand_syntax(text: &str) -> Result<(), String> {
    if text.is_empty() {
        return Err("A comparison is missing an operand.".into());
    }
    if number(text).is_some() || stack_index(text).is_some() || matches!(text, "pc" | "gas" | "depth") {
        return Ok(());
    }
    if text.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
        return Ok(()); // a state variable path, resolved per step
    }
    Err(format!("`{text}` is not a number, $i, stack[i], pc, gas, depth or a state variable."))
}

fn operand(session: &Session, index: usize, text: &str) -> Result<U256, String> {
    let step = session.steps.get(index).ok_or("No such step.")?;
    if let Some(value) = number(text) {
        return Ok(value);
    }
    if let Some(at) = stack_index(text) {
        let word = step.stack.iter().rev().nth(at).ok_or_else(|| format!("The stack has {} items here.", step.stack.len()))?;
        return U256::from_str_radix(word.trim_start_matches("0x"), 16).map_err(|_| "Bad stack word.".to_string());
    }
    match text {
        "pc" => return Ok(step.pc.into()),
        "gas" => return Ok(step.gas.into()),
        "depth" => return Ok(step.depth.into()),
        _ => {}
    }
    let layout = session.layout_at(&step.address).ok_or_else(|| format!("{text}: no storage layout for the code at this step."))?;
    let account = step.context.clone();
    layout.value(text, &|slot| session.storage_at(&account, slot, index)).map(|(value, ..)| value)
}

fn number(text: &str) -> Option<U256> {
    match text.strip_prefix("0x") {
        Some(hex) => U256::from_str_radix(hex, 16).ok(),
        None if text.bytes().all(|byte| byte.is_ascii_digit()) => U256::from_dec_str(text).ok(),
        None => None,
    }
}

fn stack_index(text: &str) -> Option<usize> {
    text.strip_prefix('$').or_else(|| text.strip_prefix("stack[").and_then(|rest| rest.strip_suffix(']'))).and_then(|digits| digits.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    fn session() -> Session {
        let logs = vec![
            json!({"pc":0,"op":"PUSH1","gas":100,"gasCost":3,"depth":1,"stack":[],"memory":[]}),
            json!({"pc":2,"op":"PUSH1","gas":97,"gasCost":3,"depth":1,"stack":["0x7"],"memory":[]}),
            json!({"pc":4,"op":"SSTORE","gas":94,"gasCost":3,"depth":1,"stack":["0x7","0x0"],"memory":[]}),
        ];
        let trace = json!({"failed":false,"gas":21000,"returnValue":"","structLogs":logs});
        super::super::trace_debug::from_trace("t".into(), "0x00000000000000000000000000000000000000aa", &trace, Vec::new(), &HashMap::new()).unwrap()
    }

    #[test]
    fn conditions_compare_stack_words_and_step_fields() {
        let session = session();
        assert_eq!(eval(&session, 1, "$0 == 7").unwrap(), true);
        assert_eq!(eval(&session, 2, "$0 == 0 && stack[1] > 6").unwrap(), true);
        assert_eq!(eval(&session, 2, "pc == 3 || gas >= 94").unwrap(), true);
        assert_eq!(eval(&session, 0, "depth != 1").unwrap(), false);
        assert!(eval(&session, 0, "$0 > 1").unwrap_err().contains("stack has 0"));
        assert!(eval(&session, 0, "owner == 0").unwrap_err().contains("no storage layout"));
        assert_eq!(split("m[a<b] >= 3"), ("m[a<b]", Some(">="), Some("3")));
        assert!(parse("$0 >").is_err());
        assert!(parse("1 == 2 && ").is_err());
        assert!(parse("%x == 1").is_err());
        assert!(parse("balanceOf[0xab] > 0x10").is_ok());
    }
}
