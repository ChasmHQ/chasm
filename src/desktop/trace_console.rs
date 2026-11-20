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
d *<pc>, d op <OP>, d entry   delete those breakpoints
b <line|*pc|op OP> if <cond>  stop only where the condition holds
b if <cond>      stop where a condition becomes true; d if clears them
                 cond: $0 > 5, pc == 0x1a, depth == 2, balanceOf[0x…] >= 100, joined by && or ||
watch <variable> stop where a write changes it (watch slot <n> for a raw slot); unwatch <variable>
ni, nexti        one instruction, stepping over calls
txs              transactions of a scenario; tx <n> jumps to one's first step
compare <file>   first divergence and effect differences against a saved trace (compare alone lists them)
The recorded trace is read-only: set and Keep belong to the offline engine.";

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
        "ni" | "nexti" => moved(session, |s| s.next_instruction()),
        "set" | "keep" => "The recorded trace is read-only. To change state and run on, use the offline engine (Captures and Foundry tests).".into(),
        "watch" => watch(session, &args),
        "unwatch" => {
            let label = args.join(" ");
            let before = session.watches.len();
            session.watches.retain(|watch| watch.label != label);
            if session.watches.len() < before { format!("No longer watching {label}.") } else { format!("Not watching {label}.") }
        }
        "b" | "break" if args.first() == Some(&"if") => {
            let condition = args[1..].join(" ");
            match super::trace_condition::parse(&condition) {
                Ok(()) => {
                    session.when.push(condition.clone());
                    format!("Stop where {condition} becomes true.")
                }
                Err(error) => error,
            }
        }
        "d" | "delete" if args.first() == Some(&"if") => {
            let count = session.when.len();
            session.when.clear();
            format!("Deleted {count} conditions.")
        }
        "b" | "break" if args.contains(&"if") => {
            let at = args.iter().position(|arg| *arg == "if").unwrap();
            let condition = args[at + 1..].join(" ");
            if let Err(error) = super::trace_condition::parse(&condition) {
                return error;
            }
            let target = args[..at].join(" ");
            let out = run(session, &format!("b {target}"));
            let key = if let Some(pc) = target.strip_prefix('*') {
                parse_number(pc).map(|pc| format!("pc {pc}"))
            } else if let Some(op) = target.strip_prefix("op ") {
                Some(format!("op {}", op.trim().to_ascii_uppercase()))
            } else {
                target.parse::<usize>().ok().map(|line| format!("line {line}"))
            };
            match key {
                Some(key) if !out.starts_with("Usage") => {
                    session.conditions.insert(key, condition.clone());
                    format!("{} Only where {condition}.", out.trim_end_matches('.'))
                }
                _ => out,
            }
        }
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
            for line in out.iter_mut() {
                if let Some(condition) = session.conditions.get(line.as_str()).or_else(|| session.conditions.get(&line.replace("opcode ", "op "))) {
                    line.push_str(&format!(" if {condition}"));
                }
            }
            out.extend(session.when.iter().map(|condition| format!("when {condition}")));
            out.extend(session.watches.iter().map(|watch| format!("watch {}", watch.label)));
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
        "txs" => {
            if session.parts.is_empty() {
                return "One transaction. Debug several in one session with debugger.trace {hashes} or a macro run.".into();
            }
            let here = session.part_at(session.cursor);
            session
                .part_ranges()
                .iter()
                .zip(&session.parts)
                .enumerate()
                .map(|(at, ((first, last, failed), part))| format!("{}{:>2} {}  {}  steps {}–{}  gas {}", if at == here { ">" } else { " " }, at + 1, part.title, if *failed { "reverted" } else { "ok" }, first + 1, last + 1, part.gas_used))
                .collect::<Vec<_>>()
                .join("\n")
        }
        "tx" => match args.first().and_then(|text| text.parse::<usize>().ok()) {
            Some(number) if number >= 1 && number <= session.parts.len() => {
                let first = session.parts[number - 1].first;
                session.go(first);
                where_line(session)
            }
            _ => format!("Usage: tx <1–{}> (see txs)", session.parts.len().max(1)),
        },
        "compare" => "compare needs the project's saved traces; it runs from the Debugger console.".into(),
        other => format!("Unknown command `{other}`. Type help for the list."),
    }
}

fn where_line(session: &Session) -> String {
    let Some(step) = session.current() else { return "No trace loaded.".into() };
    let place = match session.location(session.cursor) {
        Some(location) => format!("{}:{}", location.file, location.line),
        None => "no source".into(),
    };
    let tx = if session.parts.is_empty() { String::new() } else { format!("  tx {}/{}", session.part_at(session.cursor) + 1, session.parts.len()) };
    format!("step {}/{}{tx}  {place}  pc {}  {}", session.cursor + 1, session.steps.len(), step.pc, step.op)
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

/// Watch a state variable of the running contract, or a raw slot.
fn watch(session: &mut Session, args: &[&str]) -> String {
    let Some(step) = session.current() else { return "No trace loaded.".into() };
    let account = step.context.clone();
    let watch = match args {
        ["slot", slot] => match parse_number(slot) {
            Some(slot) => super::trace_debug::Watch { label: format!("slot {slot:#x}"), account, slot, offset: 0, size: 32 },
            None => return "Usage: watch slot <n>".into(),
        },
        [] => return "Usage: watch <variable> or watch slot <n>".into(),
        _ => {
            let path = args.join("");
            let Some(layout) = session.layout_at(&step.address) else {
                return "Watching a variable needs the running contract's storage layout; watch slot <n> works without it.".into();
            };
            match layout.resolve(&path) {
                Ok((slot, offset, _)) => {
                    let size = layout.value(&path, &|_| Some(ethers::types::H256::zero())).map(|(_, _, _, size)| size).unwrap_or(32);
                    super::trace_debug::Watch { label: path, account, slot, offset, size }
                }
                Err(error) => return error,
            }
        }
    };
    let text = format!("Watching {} (slot {:#x}). c stops where a write changes it.", watch.label, watch.slot);
    if !session.watches.contains(&watch) {
        session.watches.push(watch);
    }
    text
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

    #[test]
    fn conditions_watches_and_nexti() {
        let mut session = nested();
        // Steps: 1 SSTORE(1=5) 2 DELEGATECALL 3 SSTORE(2=7) 4 STOP 5 CALL 6 SSTORE(3=9) 7 REVERT 8 STOP.
        assert_eq!(run(&mut session, "b op SSTORE if $0 == 2"), "Break on every SSTORE Only where $0 == 2.");
        assert!(run(&mut session, "goto 1").starts_with("step 1/8"));
        assert!(run(&mut session, "c").starts_with("step 3/8"), "only the SSTORE to slot 2 matches");
        assert!(run(&mut session, "info b").contains("opcode SSTORE if $0 == 2"));
        run(&mut session, "d op SSTORE");
        assert_eq!(run(&mut session, "b if depth == 2 && $0 == 3"), "Stop where depth == 2 && $0 == 3 becomes true.");
        assert!(run(&mut session, "goto 1").starts_with("step 1/8"));
        assert!(run(&mut session, "c").starts_with("step 6/8"));
        assert!(run(&mut session, "b if $0 >").contains("missing an operand"));
        assert_eq!(run(&mut session, "d if"), "Deleted 1 conditions.");
        // A watch belongs to the account running at the cursor: here 0xaa.
        assert!(run(&mut session, "goto 1").starts_with("step 1/8"));
        assert!(run(&mut session, "watch slot 2").starts_with("Watching slot 0x2"));
        assert!(run(&mut session, "c").starts_with("step 3/8"), "the delegatecall writes the caller's slot 2");
        assert!(run(&mut session, "info b").contains("watch slot 0x2"));
        assert_eq!(run(&mut session, "unwatch slot 0x2"), "No longer watching slot 0x2.");
        assert!(run(&mut session, "watch owner").contains("storage layout"));
        assert!(run(&mut session, "goto 2").starts_with("step 2/8"));
        assert!(run(&mut session, "ni").starts_with("step 5/8"), "nexti steps over the delegatecall");
        assert!(run(&mut session, "set x = 1").contains("read-only"));
    }

    /// One transaction to 0xaa: optional SLOAD of slot 0 (reads `seen`), then SSTORE slot 0 = `value`.
    fn tx(value: u64, seen: Option<u64>, fail: bool) -> Session {
        let mut logs = Vec::new();
        if let Some(seen) = seen {
            logs.push(json!({"pc":0,"op":"SLOAD","gas":100,"gasCost":1,"depth":1,"stack":["0x0"],"memory":[]}));
            logs.push(json!({"pc":1,"op":"POP","gas":99,"gasCost":1,"depth":1,"stack":[format!("{seen:#x}")],"memory":[]}));
        }
        logs.push(json!({"pc":2,"op":"SSTORE","gas":90,"gasCost":1,"depth":1,"stack":[format!("{value:#x}"),"0x0"],"memory":[]}));
        logs.push(json!({"pc":3,"op":if fail {"REVERT"} else {"STOP"},"gas":80,"gasCost":0,"depth":1,"stack":["0x0","0x0"],"memory":[]}));
        let trace = json!({"failed":fail,"gas":21000,"returnValue":"","structLogs":logs});
        super::super::trace_debug::from_trace(format!("tx {value}"), "0x00000000000000000000000000000000000000aa", &trace, Vec::new(), &HashMap::new()).unwrap()
    }

    #[test]
    fn a_scenario_carries_storage_across_transactions() {
        let aa = "0x00000000000000000000000000000000000000aa";
        let mut before = super::super::trace_debug::Prestate::default();
        before.storage.insert(aa.into(), HashMap::from([("0x0".to_string(), "0x1".to_string())]));
        // Tx 1 writes 5; tx 2 reads 5 and writes 6 but reverts; tx 3 reads 5 and writes 7.
        let parts = vec![(tx(5, None, false), before.clone()), (tx(6, Some(5), true), before.clone()), (tx(7, Some(5), false), before)];
        let mut session = super::super::trace_debug::scenario("s".into(), parts).unwrap();
        assert_eq!(session.parts.len(), 3);
        assert_eq!(session.part_ranges(), vec![(0, 1, false), (2, 5, true), (6, 9, false)]);
        let at = |session: &Session, step| session.storage_at(aa, U256::zero(), step).map(|word| word.to_low_u64_be());
        assert_eq!((at(&session, 0), at(&session, 4), at(&session, 5), at(&session, 9)), (Some(5), Some(6), Some(5), Some(7)));
        let txs = run(&mut session, "txs");
        assert!(txs.contains(" 2 tx 6  reverted  steps 3–6") && txs.starts_with("> 1 tx 5"), "{txs}");
        assert!(run(&mut session, "tx 3").starts_with("step 7/10  tx 3/3"));
        let calls = run(&mut session, "calls");
        assert_eq!(calls.lines().filter(|line| line.contains("tx ")).count(), 3, "{calls}");
        let diff = run(&mut session, "diff");
        assert!(diff.contains("slot 0x0: 0x1 -> 0x7"), "{diff}");
        assert!(run(&mut session, "tx 4").starts_with("Usage"));
    }
}
