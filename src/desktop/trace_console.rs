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
p <variable>     state variable at this step: p owner, p balanceOf[0x…], p pos.amount
x <offset> [n]   n bytes of memory at offset (default 32)
sload <slot>     storage slot of the running contract at this step
where            current location
args             decoded arguments of the call
writes           storage writes so far, named by the layout
diff             every slot and balance the execution changed, before and after
gas [lines]      gas per call frame, or per source line
locals           Solidity locals of the function at the cursor
calls            the call tree; frame <n> jumps to a frame's first step
b *<pc>, b op <OP>, b entry   break on a pc, an opcode, or entering any call
d *<pc>, d op <OP>, d entry   delete those breakpoints";

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
            let mut names = Names::default();
            let done: Vec<String> = session
                .writes
                .iter()
                .filter(|write| write.step <= session.cursor)
                .map(|write| {
                    let undone = match write.undone_at {
                        Some(at) if at <= session.cursor => "  (undone by a revert)",
                        Some(_) => "  (undone later by a revert)",
                        None => "",
                    };
                    let before = names.value(session, &write.address, write.slot, Some(write.before));
                    let after = names.value(session, &write.address, write.slot, Some(write.after));
                    format!("step {}  {}  {before} -> {after}{undone}", write.step + 1, names.slot(session, &write.address, write.slot))
                })
                .collect();
            if done.is_empty() { "No storage written up to this step.".into() } else { done.join("\n") }
        }
        "diff" => diff(session),
        "locals" | "info locals" => match session.locals() {
            Some(locals) if !locals.is_empty() => locals.iter().map(|(name, kind, value)| format!("{name} ({kind}) = {value}")).collect::<Vec<_>>().join("\n"),
            Some(_) => "No locals in scope here.".into(),
            None => "Locals need the running contract's AST from this workspace.".into(),
        },
        "gas" => {
            let rows = if args.first().is_some_and(|arg| arg.starts_with('l')) { super::gas_profile::by_line(session) } else { super::gas_profile::by_call(session) };
            super::gas_profile::table(&rows, 20)
        }
        "calls" => {
            let frames = session.call_frames();
            let here = super::call_tree::frame_at(&frames.iter().map(|(frame, _)| frame.clone()).collect::<Vec<_>>(), session.cursor);
            frames
                .iter()
                .enumerate()
                .map(|(index, (frame, text))| format!("{}{index:>3} {}{text}", if Some(index) == here { ">" } else { " " }, "  ".repeat(frame.depth.saturating_sub(1) as usize)))
                .collect::<Vec<_>>()
                .join("\n")
        }
        "frame" => match args.first().and_then(|text| text.parse::<usize>().ok()) {
            Some(index) => match session.call_frames().get(index) {
                Some((frame, _)) => {
                    let first = frame.first;
                    session.go(first);
                    where_line(session)
                }
                None => format!("No frame {index}. calls lists them."),
            },
            None => "Usage: frame <n> (see calls)".into(),
        },
        "b" | "break" if args.first().is_some_and(|arg| arg.starts_with('*') || *arg == "op" || *arg == "entry") => {
            match (args.first().copied(), args.get(1)) {
                (Some("entry"), _) => {
                    session.break_on_entry = true;
                    "Break on entering any call frame.".into()
                }
                (Some("op"), Some(op)) => {
                    let op = op.to_ascii_uppercase();
                    if !session.op_breaks.contains(&op) {
                        session.op_breaks.push(op.clone());
                    }
                    format!("Break on every {op}.")
                }
                (Some(pc), _) => match parse_number(pc.trim_start_matches('*')).filter(|pc| *pc <= U256::from(u64::MAX)).map(|pc| pc.as_u64()) {
                    Some(pc) => {
                        if !session.pc_breaks.contains(&pc) {
                            session.pc_breaks.push(pc);
                        }
                        format!("Break at pc {pc} in any contract.")
                    }
                    None => "Usage: b *<pc>, b op <OPCODE>, b entry".into(),
                },
                _ => "Usage: b *<pc>, b op <OPCODE>, b entry".into(),
            }
        }
        "d" | "delete" if args.first().is_some_and(|arg| arg.starts_with('*') || *arg == "op" || *arg == "entry") => {
            match (args.first().copied(), args.get(1)) {
                (Some("entry"), _) => {
                    session.break_on_entry = false;
                    "No longer breaking on frame entry.".into()
                }
                (Some("op"), Some(op)) => {
                    let op = op.to_ascii_uppercase();
                    session.op_breaks.retain(|item| item != &op);
                    format!("Deleted the {op} breakpoint.")
                }
                (Some(pc), _) => match parse_number(pc.trim_start_matches('*')).filter(|pc| *pc <= U256::from(u64::MAX)).map(|pc| pc.as_u64()) {
                    Some(pc) => {
                        session.pc_breaks.retain(|item| *item != pc);
                        format!("Deleted the breakpoint at pc {pc}.")
                    }
                    None => "Usage: d *<pc>, d op <OPCODE>, d entry".into(),
                },
                _ => "Usage: d *<pc>, d op <OPCODE>, d entry".into(),
            }
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
            let mut lines = session.breakpoints.clone();
            lines.sort_unstable();
            let mut out: Vec<String> = lines.iter().map(|line| format!("line {line}")).collect();
            out.extend(session.pc_breaks.iter().map(|pc| format!("pc {pc}")));
            out.extend(session.op_breaks.iter().map(|op| format!("opcode {op}")));
            if session.break_on_entry {
                out.push("frame entry".into());
            }
            if out.is_empty() { "No breakpoints.".into() } else { out.join("\n") }
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
    // Anything but the stack is a state variable path.
    if !what.is_empty() && what != "stack" && !what.starts_with("stack[") && !what.starts_with('$') {
        let Some(layout) = session.layout_at(&step.address) else {
            return "State variables need the running contract's storage layout (a workspace build or verified source). sload <slot> reads raw slots.".into();
        };
        let (account, cursor) = (step.context.clone(), session.cursor);
        return layout.show(&what, &|slot| session.storage_at(&account, slot, cursor)).unwrap_or_else(|error| error);
    }
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
    if let Some(value) = session.storage_at(&step.context, wanted, session.cursor) {
        let text = format!("{value:#x}");
        return format!("slot {wanted:#x} = {text}{}", decimal(&text));
    }
    let found = step.storage.iter().find(|(key, _)| U256::from_str_radix(key.trim_start_matches("0x"), 16).ok() == Some(wanted));
    match found {
        Some((_, value)) => format!("slot {wanted:#x} = 0x{}{}", value.trim_start_matches("0x"), decimal(value)),
        None => format!("Slot {wanted:#x} was not read or written up to this step, and its value before the execution was not fetched."),
    }
}

/// Slot names and typed values from each contract's layout, cached per address.
#[derive(Default)]
struct Names {
    layouts: std::collections::HashMap<String, Option<(super::storage_layout::Layout, std::collections::HashMap<U256, Vec<u8>>)>>,
}

impl Names {
    fn field(&mut self, session: &Session, address: &str, slot: U256) -> Option<super::storage_layout::SlotField> {
        let entry = self.layouts.entry(address.to_string()).or_insert_with(|| {
            let layout = session.layout_at(address)?;
            let preimages = session.naming_preimages(&layout);
            Some((layout, preimages))
        });
        let (layout, preimages) = entry.as_ref()?;
        let fields = layout.name_slot(slot, preimages)?;
        // Packed slots are named together; their value is the whole word.
        if fields.len() == 1 {
            return fields.into_iter().next();
        }
        Some(super::storage_layout::SlotField { path: fields.iter().map(|field| field.path.as_str()).collect::<Vec<_>>().join("+"), type_label: "bytes32".into(), offset: 0, size: 32 })
    }

    fn slot(&mut self, session: &Session, address: &str, slot: U256) -> String {
        match self.field(session, address, slot) {
            Some(field) => field.path,
            None => format!("slot {slot:#x}"),
        }
    }

    fn value(&mut self, session: &Session, address: &str, slot: U256, word: Option<ethers::types::H256>) -> String {
        let Some(word) = word else { return "?".into() };
        match self.field(session, address, slot) {
            Some(field) => super::storage_layout::scalar(&field.type_label, word, field.offset, field.size),
            None => format!("{:#x}", U256::from_big_endian(word.as_bytes())),
        }
    }
}

/// Net effect of the execution: named slots and balances, before and after.
fn diff(session: &Session) -> String {
    let mut names = Names::default();
    let mut lines = Vec::new();
    let mut account = String::new();
    let checksum = |account: &str| account.parse::<ethers::types::Address>().map(|address| ethers::utils::to_checksum(&address, None)).unwrap_or_else(|_| account.to_string());
    for change in session.state_diff() {
        if change.account != account {
            account = change.account.clone();
            let name = session.mapped.get(&change.account).or_else(|| session.mapped.get(&change.address)).map(|program| session.programs[*program].name.clone()).unwrap_or_else(|| "account".into());
            lines.push(format!("{name} {}", checksum(&account)));
        }
        let before = match change.before {
            Some(_) => names.value(session, &change.address, change.slot, change.before),
            None => "? (not fetched)".into(),
        };
        let after = names.value(session, &change.address, change.slot, Some(change.after));
        lines.push(format!("  {}: {before} -> {after}", names.slot(session, &change.address, change.slot)));
    }
    for (account, before, after) in &session.balances {
        lines.push(format!("balance {}: {before} -> {after} wei", checksum(account)));
    }
    if lines.is_empty() {
        return if session.failed { "Nothing changed: the execution reverted.".into() } else { "No storage or balance changed.".into() };
    }
    lines.join("\n")
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
        assert!(run(&mut session, "gas").contains("total execution gas"));
        assert!(run(&mut session, "gas lines").contains("no source"));
        assert!(run(&mut session, "calls").contains("→ returned"), "{}", run(&mut session, "calls"));
        assert_eq!(run(&mut session, "b op sload"), "Break on every SLOAD.");
        assert!(run(&mut session, "goto 1").starts_with("step 1/3"));
        assert!(run(&mut session, "c").starts_with("step 2/3"), "continue stops on the SLOAD");
        assert!(run(&mut session, "info b").contains("opcode SLOAD"));
        assert_eq!(run(&mut session, "d op sload"), "Deleted the SLOAD breakpoint.");
        assert_eq!(run(&mut session, "b *3"), "Break at pc 3 in any contract.");
        assert!(run(&mut session, "goto 1").starts_with("step 1/3"));
        assert!(run(&mut session, "c").starts_with("step 3/3"));
    }

    /// 0xaa writes slot 1 = 5, DELEGATECALLs 0xbb which writes slot 2 = 7 in
    /// 0xaa's storage, then CALLs 0xcc which writes slot 3 = 9 and reverts.
    fn nested() -> Session {
        let word = |n: u64| format!("{n:#x}");
        let logs = vec![
            json!({"pc":0,"op":"SSTORE","gas":100,"gasCost":1,"depth":1,"stack":[word(5), word(1)],"memory":[]}),
            json!({"pc":1,"op":"DELEGATECALL","gas":99,"gasCost":1,"depth":1,"stack":["0x0","0x0","0x0","0x0","0xbb","0x50"],"memory":[]}),
            json!({"pc":0,"op":"SSTORE","gas":80,"gasCost":1,"depth":2,"stack":[word(7), word(2)],"memory":[]}),
            json!({"pc":1,"op":"STOP","gas":79,"gasCost":0,"depth":2,"stack":[],"memory":[]}),
            json!({"pc":2,"op":"CALL","gas":70,"gasCost":1,"depth":1,"stack":["0x0","0x0","0x0","0x0","0x0","0xcc","0x40"],"memory":[]}),
            json!({"pc":0,"op":"SSTORE","gas":60,"gasCost":1,"depth":2,"stack":[word(9), word(3)],"memory":[]}),
            json!({"pc":1,"op":"REVERT","gas":59,"gasCost":0,"depth":2,"stack":["0x0","0x0"],"memory":[]}),
            json!({"pc":3,"op":"STOP","gas":50,"gasCost":0,"depth":1,"stack":[],"memory":[]}),
        ];
        let trace = json!({"failed":false,"gas":30000,"returnValue":"","structLogs":logs});
        super::super::trace_debug::from_trace("t".into(), "0x00000000000000000000000000000000000000aa", &trace, Vec::new(), &HashMap::new()).unwrap()
    }

    #[test]
    fn writes_follow_the_storage_context_and_reverts() {
        let aa = "0x00000000000000000000000000000000000000aa";
        let cc = "0x00000000000000000000000000000000000000cc";
        let mut session = nested();
        let accounts: Vec<(String, u64)> = session.writes.iter().map(|write| (write.account.clone(), write.slot.as_u64())).collect();
        assert_eq!(accounts, vec![(aa.into(), 1), (aa.into(), 2), (cc.into(), 3)], "the delegatecall writes into the caller");
        assert_eq!(session.writes[1].address, "0x00000000000000000000000000000000000000bb", "named with the library's layout");
        assert_eq!(session.writes.iter().map(|write| write.undone_at).collect::<Vec<_>>(), vec![None, None, Some(6)]);
        // cc's slot 3 holds 9 inside the call and its old value after the revert.
        assert_eq!(session.storage_at(cc, U256::from(3), 5).map(|word| word.to_low_u64_be()), Some(9));
        assert_eq!(session.storage_at(cc, U256::from(3), 7), None, "unknown before: never read or fetched");
        assert!(run(&mut session, "goto 8").starts_with("step 8/8"));
        let writes = run(&mut session, "writes");
        assert!(writes.contains("slot 0x3  0x0 -> 0x9  (undone by a revert)"), "{writes}");
        assert_eq!(run(&mut session, "sload 2"), "slot 0x2 = 0x0000000000000000000000000000000000000000000000000000000000000007  (7)");
        let diff = run(&mut session, "diff");
        assert!(diff.contains("slot 0x1: ? (not fetched) -> 0x5") && diff.contains("slot 0x2") && !diff.contains("slot 0x3"), "{diff}");

        // With the state before the execution fetched, the diff is exact and
        // a slot that ends where it started is not listed.
        let mut prestate = super::super::trace_debug::Prestate::default();
        prestate.storage.insert(aa.into(), HashMap::from([("0x1".to_string(), "0x5".to_string()), ("0x2".to_string(), "0x3".to_string())]));
        prestate.storage.insert(cc.into(), HashMap::from([("0x3".to_string(), "0x4".to_string())]));
        prestate.balances.insert(aa.into(), ("0x10".into(), "0x0".into()));
        session.set_prestate(&prestate);
        assert_eq!(session.storage_at(cc, U256::from(3), 7).map(|word| word.to_low_u64_be()), Some(4));
        let diff = run(&mut session, "diff");
        assert!(!diff.contains("slot 0x1") && diff.contains("slot 0x2: 0x3 -> 0x7") && diff.contains("balance 0x00000000000000000000000000000000000000AA: 16 -> 0 wei"), "{diff}");
        assert!(run(&mut session, "p owner").contains("storage layout"));
    }
}
