//! Compares two recorded executions: where they first take different paths,
//! then how their effects differ. Steps are aligned by code and pc in order,
//! so two runs of the same code line up until one branches.
use super::trace_debug::Session;

/// What differs between two traces, as console lines.
pub fn compare(this: &Session, other: &Session, other_name: &str) -> String {
    let mut lines = Vec::new();
    let place = |session: &Session, index: usize| {
        let step = &session.steps[index];
        let source = session.location(index).map(|location| format!("{}:{}", location.file, location.line)).unwrap_or_else(|| "no source".into());
        let frame = session.call_frames().into_iter().filter(|(frame, _)| frame.first <= index && index <= frame.last).max_by_key(|(frame, _)| frame.depth).map(|(_, text)| text.split(" → ").next().unwrap_or_default().to_string()).unwrap_or_default();
        format!("step {}  {source}  pc {} {}  in {frame}", index + 1, step.pc, step.op)
    };
    let same = this.steps.iter().zip(&other.steps).position(|(a, b)| a.address != b.address || a.pc != b.pc || a.depth != b.depth);
    match same {
        Some(index) => {
            lines.push(format!("First divergence at step {}:", index + 1));
            lines.push(format!("  this:  {}", place(this, index)));
            lines.push(format!("  {other_name}: {}", place(other, index)));
            if index > 0 {
                lines.push(format!("  both ran {} identical steps before it, ending at {}", index, place(this, index - 1)));
            }
        }
        None if this.steps.len() == other.steps.len() => lines.push(format!("Same path: all {} steps match.", this.steps.len())),
        None => {
            let shorter = this.steps.len().min(other.steps.len());
            lines.push(format!("Same path for {shorter} steps; then only {} goes on ({} vs {} steps).", if this.steps.len() > shorter { "this trace" } else { other_name }, this.steps.len(), other.steps.len()));
        }
    }
    lines.push(format!("Outcome: this {}, {other_name} {}; gas used {} vs {}.", outcome(this), outcome(other), this.gas_used, other.gas_used));
    // Net storage effects: slots one changed and the other did not, or to a different value.
    let ours = this.state_diff();
    let theirs = other.state_diff();
    let mut storage = Vec::new();
    for change in &ours {
        let slot = slot_name(this, &change.address, change.slot);
        match theirs.iter().find(|item| item.account == change.account && item.slot == change.slot) {
            Some(item) if item.after == change.after => {}
            Some(item) => storage.push(format!("  {} {slot}: this -> {}, {other_name} -> {}", short(&change.account), word(change.after), word(item.after))),
            None => storage.push(format!("  {} {slot}: only this changes it (-> {})", short(&change.account), word(change.after))),
        }
    }
    for item in theirs.iter().filter(|item| !ours.iter().any(|change| change.account == item.account && change.slot == item.slot)) {
        storage.push(format!("  {} {}: only {other_name} changes it (-> {})", short(&item.account), slot_name(other, &item.address, item.slot), word(item.after)));
    }
    if storage.is_empty() {
        lines.push("Storage effects match.".into());
    } else {
        lines.push("Storage effects differ:".into());
        lines.extend(storage);
    }
    let balance = |session: &Session, account: &str| session.balances.iter().find(|(item, ..)| item == account).map(|(_, before, after)| (*after).saturating_sub(*before).as_u128() as i128 - (*before).saturating_sub(*after).as_u128() as i128);
    let mut accounts: Vec<&String> = this.balances.iter().chain(&other.balances).map(|(account, ..)| account).collect();
    accounts.sort();
    accounts.dedup();
    for account in accounts {
        let (a, b) = (balance(this, account).unwrap_or(0), balance(other, account).unwrap_or(0));
        if a != b {
            lines.push(format!("  balance {}: this {a:+} wei, {other_name} {b:+} wei", short(account)));
        }
    }
    lines.join("\n")
}

/// `balanceOf[0x…]` when the layout names the slot, else `slot 0x…`.
fn slot_name(session: &Session, address: &str, slot: ethers::types::U256) -> String {
    session
        .layout_at(address)
        .and_then(|layout| {
            let preimages = session.naming_preimages(&layout);
            layout.name_slot(slot, &preimages)
        })
        .map(|fields| fields.iter().map(|field| field.path.as_str()).collect::<Vec<_>>().join("+"))
        .unwrap_or_else(|| format!("slot {slot:#x}"))
}

fn outcome(session: &Session) -> &'static str {
    if session.failed { "reverted" } else { "succeeded" }
}

fn word(value: ethers::types::H256) -> ethers::types::U256 {
    ethers::types::U256::from_big_endian(value.as_bytes())
}

fn short(address: &str) -> String {
    if address.len() > 12 { format!("{}…{}", &address[..6], &address[address.len() - 4..]) } else { address.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    fn run(value: u64, branch: bool) -> Session {
        let mut logs = vec![
            json!({"pc":0,"op":"PUSH1","gas":100,"gasCost":3,"depth":1,"stack":[],"memory":[]}),
            json!({"pc":2,"op":"PUSH1","gas":97,"gasCost":3,"depth":1,"stack":[format!("{value:#x}")],"memory":[]}),
        ];
        if branch {
            logs.push(json!({"pc":9,"op":"JUMPDEST","gas":94,"gasCost":1,"depth":1,"stack":[format!("{value:#x}"),"0x0"],"memory":[]}));
        }
        logs.push(json!({"pc":4,"op":"SSTORE","gas":93,"gasCost":3,"depth":1,"stack":[format!("{value:#x}"),"0x0"],"memory":[]}));
        logs.push(json!({"pc":5,"op":"STOP","gas":90,"gasCost":0,"depth":1,"stack":[],"memory":[]}));
        let trace = json!({"failed":false,"gas":21000 + value,"returnValue":"","structLogs":logs});
        super::super::trace_debug::from_trace("t".into(), "0x00000000000000000000000000000000000000aa", &trace, Vec::new(), &HashMap::new()).unwrap()
    }

    #[test]
    fn first_divergence_then_effects() {
        let same = compare(&run(7, false), &run(7, false), "old");
        assert!(same.contains("Same path: all 4 steps match.") && same.contains("Storage effects match."), "{same}");
        let text = compare(&run(7, false), &run(8, true), "old");
        assert!(text.contains("First divergence at step 3:"), "{text}");
        assert!(text.contains("this:  step 3  no source  pc 4 SSTORE") && text.contains("old: step 3  no source  pc 9 JUMPDEST"), "{text}");
        assert!(text.contains("both ran 2 identical steps"), "{text}");
        assert!(text.contains("slot 0x0: this -> 7, old -> 8"), "{text}");
        assert!(text.contains("gas used 21007 vs 21008"), "{text}");
    }
}
