//! sevm-style command line over a recorded step-debugger session. Commands
//! only move the cursor or read recorded state; nothing runs on the node.
use super::trace_debug::Session;
use ethers::types::U256;

pub const HELP: &str = "\
s, step          next source line, entering calls      rs   reverse
n, next          next line in this function            rn   reverse
si, stepi        one EVM instruction                   rsi  reverse
fin, finish      run until this function returns       rfin reverse
c, continue      run to the next breakpoint            rc   reverse
b <line>         set a breakpoint on a source line
d <line>         delete a breakpoint (d alone clears all)
info b           list breakpoints
goto <step>      jump to step number (1-based)
find <OP>        next step running OP; rfind <OP> searches back
p stack[i]       stack word i (0 is the top); p stack prints all
x <offset> [n]   n bytes of memory at offset (default 32)
sload <slot>     storage value of a slot touched so far
where            current location
args             decoded arguments of the call
writes           storage writes so far, by slot";

/// Run one command line and return what to print.
pub fn run(session: &mut Session, line: &str) -> String {
    let mut words = line.split_whitespace();
    let Some(head) = words.next() else { return String::new() };
    let args: Vec<&str> = words.collect();
    let moved = |session: &mut Session, step: fn(&mut Session) -> bool| {
        step(session);
        where_line(session)
    };
    match head {
        "help" | "h" | "?" => HELP.into(),
        "s" | "step" => moved(session, |s| s.step_line(true)),
        "rs" => moved(session, |s| s.step_line(false)),
        "n" | "next" => moved(session, |s| s.next_line(true)),
        "rn" => moved(session, |s| s.next_line(false)),
        "si" | "stepi" => moved(session, |s| s.step_opcode(true)),
        "rsi" => moved(session, |s| s.step_opcode(false)),
        "fin" | "finish" => moved(session, |s| s.finish(true)),
        "rfin" => moved(session, |s| s.finish(false)),
        "c" | "continue" => moved(session, |s| s.resume(true)),
        "rc" => moved(session, |s| s.resume(false)),
        "where" | "w" => where_line(session),
        "args" => match session.call_arguments() {
            Some((function, args)) if args.is_empty() => format!("{function} takes no arguments."),
            Some((function, args)) => format!("{function}
{}", args.iter().map(|(name, value)| format!("  {name} = {value}")).collect::<Vec<_>>().join("
")),
            None => "Arguments need the called contract's ABI from this workspace.".into(),
        },
        "writes" => {
            let done: Vec<String> = session.writes.iter().filter(|write| write.step <= session.cursor).map(|write| format!("step {}  slot {:#x}  {:#x} -> {:#x}", write.step + 1, write.slot, write.before, write.after)).collect();
            if done.is_empty() { "No storage written up to this step. The Variables tab names each slot.".into() } else { done.join("
") }
        }
        "b" | "break" => match args.first().and_then(|text| text.parse::<usize>().ok()).filter(|line| *line > 0) {
            Some(line) if session.breakpoints.contains(&line) => format!("Breakpoint already on line {line}."),
            Some(line) => {
                session.toggle_breakpoint(line);
                format!("Breakpoint on line {line}.")
            }
            None => "Usage: b <line>".into(),
        },
        "d" | "delete" => match args.first() {
            None => {
                let count = session.breakpoints.len();
                session.breakpoints.clear();
                format!("Deleted {count} breakpoints.")
            }
            Some(text) => match text.parse::<usize>() {
                Ok(line) if session.breakpoints.contains(&line) => {
                    session.toggle_breakpoint(line);
                    format!("Deleted the breakpoint on line {line}.")
                }
                _ => format!("No breakpoint on line {text}."),
            },
        },
        "info" | "i" if matches!(args.first(), Some(&("b" | "break" | "breakpoints"))) => {
            if session.breakpoints.is_empty() {
                "No breakpoints.".into()
            } else {
                let mut lines = session.breakpoints.clone();
                lines.sort_unstable();
                lines.iter().map(|line| format!("line {line}")).collect::<Vec<_>>().join("\n")
            }
        }
        "goto" | "g" => match args.first().and_then(|text| text.parse::<usize>().ok()).filter(|step| *step > 0) {
            Some(step) => {
                session.go(step - 1);
                where_line(session)
            }
            None => "Usage: goto <step number>".into(),
        },
        "find" | "rfind" => match args.first() {
            Some(op) if session.next_opcode(op, head == "find") => where_line(session),
            Some(op) => format!("No {} {} this step.", op.to_ascii_uppercase(), if head == "find" { "after" } else { "before" }),
            None => format!("Usage: {head} <OPCODE>"),
        },
        "p" | "print" => print(session, &args.join(" ")),
        "x" => memory(session, &args),
        "sload" => sload(session, args.first().copied()),
        other => format!("Unknown command `{other}`. Type help for the list."),
    }
}

fn where_line(session: &Session) -> String {
    let Some(step) = session.current() else { return "No trace loaded.".into() };
    let place = match session.location(session.cursor) {
        Some(location) => format!("{}:{}", location.file, location.line),
        None => "no source".into(),
    };
    format!("step {}/{}  {place}  pc {}  {}", session.cursor + 1, session.steps.len(), step.pc, step.op)
}

fn print(session: &Session, what: &str) -> String {
    let Some(step) = session.current() else { return "No trace loaded.".into() };
    let what = what.replace(' ', "");
    if what == "stack" {
        if step.stack.is_empty() {
            return "Stack is empty.".into();
        }
        return step.stack.iter().rev().enumerate().map(|(index, word)| format!("[{index}] {word}")).collect::<Vec<_>>().join("\n");
    }
    let index = what.strip_prefix("stack[").and_then(|rest| rest.strip_suffix(']')).or_else(|| what.strip_prefix('$'));
    match index.and_then(|text| text.parse::<usize>().ok()) {
        Some(index) => match step.stack.iter().rev().nth(index) {
            Some(word) => format!("stack[{index}] = {word}{}", decimal(word)),
            None => format!("The stack has {} items.", step.stack.len()),
        },
        None => "Usage: p stack[i], p $i or p stack".into(),
    }
}

fn decimal(word: &str) -> String {
    match U256::from_str_radix(word.trim_start_matches("0x"), 16) {
        Ok(value) if value < U256::from(u128::MAX) => format!("  ({value})"),
        _ => String::new(),
    }
}

fn parse_number(text: &str) -> Option<U256> {
    match text.strip_prefix("0x") {
        Some(hex) => U256::from_str_radix(hex, 16).ok(),
        None => U256::from_dec_str(text).ok(),
    }
}

fn memory(session: &Session, args: &[&str]) -> String {
    let Some(step) = session.current() else { return "No trace loaded.".into() };
    let Some(offset) = args.first().and_then(|text| parse_number(text)).filter(|value| *value < U256::from(1u64 << 32)) else {
        return "Usage: x <offset> [bytes]".into();
    };
    let length = args.get(1).and_then(|text| text.parse::<usize>().ok()).unwrap_or(32).clamp(1, 1024);
    let bytes: Vec<u8> = step.memory.iter().flat_map(|word| ethers::utils::hex::decode(word.trim_start_matches("0x")).unwrap_or_default()).collect();
    let start = offset.as_usize();
    if start >= bytes.len() {
        return format!("Memory is {} bytes; offset {start:#x} is past the end.", bytes.len());
    }
    let end = (start + length).min(bytes.len());
    bytes[start..end]
        .chunks(32)
        .enumerate()
        .map(|(row, chunk)| format!("{:#06x}  {}", start + row * 32, ethers::utils::hex::encode(chunk)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn sload(session: &Session, slot: Option<&str>) -> String {
    let Some(step) = session.current() else { return "No trace loaded.".into() };
    let Some(wanted) = slot.and_then(parse_number) else { return "Usage: sload <slot>".into() };
    let found = step.storage.iter().find(|(key, _)| U256::from_str_radix(key.trim_start_matches("0x"), 16).ok() == Some(wanted));
    match found {
        Some((_, value)) => format!("slot {wanted:#x} = 0x{}{}", value.trim_start_matches("0x"), decimal(value)),
        None => format!("Slot {wanted:#x} was not read or written by this contract up to this step."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    fn session() -> Session {
        let logs = vec![
            json!({"pc":0,"op":"PUSH1","gas":10,"gasCost":3,"depth":1,"stack":[],"memory":[]}),
            json!({"pc":2,"op":"SLOAD","gas":7,"gasCost":3,"depth":1,"stack":["0x2a","0x05"],"memory":["00000000000000000000000000000000000000000000000000000000000000ff"],"storage":{"0000000000000000000000000000000000000000000000000000000000000005":"0000000000000000000000000000000000000000000000000000000000000063"}}),
            json!({"pc":3,"op":"STOP","gas":4,"gasCost":0,"depth":1,"stack":[],"memory":[]}),
        ];
        let trace = json!({"failed":false,"gas":21000,"returnValue":"","structLogs":logs});
        super::super::trace_debug::from_trace("t".into(), "0x00000000000000000000000000000000000000aa", &trace, Vec::new(), &HashMap::new()).unwrap()
    }

    #[test]
    fn commands_move_and_read_recorded_state() {
        let mut session = session();
        assert!(run(&mut session, "si").starts_with("step 2/3"));
        assert_eq!(run(&mut session, "p stack[0]"), "stack[0] = 0x5  (5)");
        assert_eq!(run(&mut session, "p $1"), "stack[1] = 0x2a  (42)");
        assert!(run(&mut session, "p stack").contains("[1] 0x2a"));
        assert_eq!(run(&mut session, "sload 5"), "slot 0x5 = 0x0000000000000000000000000000000000000000000000000000000000000063  (99)");
        assert!(run(&mut session, "sload 0x6").contains("was not read"));
        assert!(run(&mut session, "x 0x1f 1").ends_with("ff"));
        assert!(run(&mut session, "x 64").contains("past the end"));
        assert!(run(&mut session, "rsi").starts_with("step 1/3"));
        assert!(run(&mut session, "find stop").starts_with("step 3/3"));
        assert!(run(&mut session, "rfind push1").starts_with("step 1/3"));
        assert!(run(&mut session, "goto 2").starts_with("step 2/3"));
        assert_eq!(run(&mut session, "b 12"), "Breakpoint on line 12.");
        assert_eq!(run(&mut session, "info b"), "line 12");
        assert_eq!(run(&mut session, "d 12"), "Deleted the breakpoint on line 12.");
        assert_eq!(run(&mut session, "info b"), "No breakpoints.");
        assert!(run(&mut session, "bogus").starts_with("Unknown command"));
        assert!(run(&mut session, "help").contains("rfin"));
    }
}
