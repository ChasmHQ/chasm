//! Gas profile of a recorded trace: where the gas went, per source line and
//! per external call. Costs are each step's own: a CALL's `gasCost` also
//! counts the gas it forwards, which the callee's steps already account for.
use super::trace_debug::Session;
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub label: String,
    pub gas: u64,
    pub steps: usize,
}

/// Own cost of each step: the callee's first step starts with the forwarded
/// gas, so a call step keeps only what the CALL itself charged.
pub fn own_costs(session: &Session) -> Vec<u64> {
    let steps = &session.steps;
    (0..steps.len())
        .map(|index| {
            let step = &steps[index];
            match steps.get(index + 1) {
                Some(next) if next.depth > step.depth => step.gas_cost.saturating_sub(next.gas),
                _ => step.gas_cost,
            }
        })
        .collect()
}

/// Gas per source line ("File.sol:12  text"), most expensive first. Steps
/// without source go to "no source (<contract>)".
pub fn by_line(session: &Session) -> Vec<Row> {
    let costs = own_costs(session);
    let mut rows: HashMap<String, Row> = HashMap::new();
    for (index, cost) in costs.iter().enumerate() {
        let label = match session.location(index) {
            Some(location) => {
                let text = session
                    .source_lines(index)
                    .and_then(|(_, lines)| lines.get(location.line - 1).map(|line| line.trim().to_string()))
                    .unwrap_or_default();
                format!("{}:{}  {}", location.file, location.line, text)
            }
            None => format!("no source ({})", contract_name(session, index)),
        };
        let row = rows.entry(label.clone()).or_insert(Row { label, gas: 0, steps: 0 });
        row.gas += cost;
        row.steps += 1;
    }
    sorted(rows)
}

/// Gas per external call frame, named `Contract.function` from the ABI when
/// the selector is known. Nested calls are counted in their own frame.
pub fn by_call(session: &Session) -> Vec<Row> {
    let costs = own_costs(session);
    let mut rows: HashMap<String, Row> = HashMap::new();
    // Name of the frame running at each depth.
    let mut frames: Vec<String> = Vec::new();
    for (index, step) in session.steps.iter().enumerate() {
        let depth = step.depth as usize;
        frames.truncate(depth);
        if frames.len() < depth {
            let selector = if index == 0 { session.calldata.get(..4).map(<[u8]>::to_vec) } else { call_selector(session, index) };
            frames.resize(depth, String::new());
            frames[depth - 1] = frame_name(session, index, selector.as_deref());
        }
        let label = frames[depth - 1].clone();
        let row = rows.entry(label.clone()).or_insert(Row { label, gas: 0, steps: 0 });
        row.gas += costs[index];
        row.steps += 1;
    }
    sorted(rows)
}

fn sorted(rows: HashMap<String, Row>) -> Vec<Row> {
    let mut rows: Vec<Row> = rows.into_values().collect();
    rows.sort_by(|a, b| b.gas.cmp(&a.gas).then_with(|| a.label.cmp(&b.label)));
    rows
}

fn contract_name(session: &Session, index: usize) -> String {
    let address = &session.steps[index].address;
    session
        .mapped
        .get(address)
        .map(|program| session.programs[*program].name.clone())
        .unwrap_or_else(|| short(address))
}

fn short(address: &str) -> String {
    if address.len() > 12 { format!("{}…{}", &address[..6], &address[address.len() - 4..]) } else { address.to_string() }
}

/// Selector of the call that entered the frame starting at `index`: the
/// first 4 bytes of the caller's memory at the call's args offset.
fn call_selector(session: &Session, index: usize) -> Option<Vec<u8>> {
    let call = session.steps.get(index.checked_sub(1)?)?;
    // CALL/CALLCODE: gas, to, value, argsOffset; DELEGATECALL/STATICCALL: gas, to, argsOffset.
    let from_top = match call.op.as_str() {
        "CALL" | "CALLCODE" => 3,
        "DELEGATECALL" | "STATICCALL" => 2,
        _ => return None,
    };
    let word = call.stack.get(call.stack.len().checked_sub(1 + from_top)?)?;
    let offset = usize::from_str_radix(word.trim_start_matches("0x"), 16).ok()?;
    let memory: Vec<u8> = call.memory.iter().flat_map(|word| ethers::utils::hex::decode(word.trim_start_matches("0x")).unwrap_or_default()).collect();
    memory.get(offset..offset + 4).map(<[u8]>::to_vec)
}

fn frame_name(session: &Session, index: usize, selector: Option<&[u8]>) -> String {
    use ethers::abi::FunctionExt;
    let contract = contract_name(session, index);
    let program = session.mapped.get(&session.steps[index].address).map(|program| &session.programs[*program]);
    let function = selector.and_then(|selector| {
        let abi = program?.abi.as_ref()?;
        abi.functions().find(|function| function.selector() == selector).map(|function| function.name.clone())
    });
    match (function, selector) {
        (Some(name), _) => format!("{contract}.{name}"),
        (None, Some(selector)) if selector.len() == 4 => format!("{contract}.0x{}", ethers::utils::hex::encode(selector)),
        _ => format!("{contract} (no calldata)"),
    }
}

/// Plain-text table for the console.
pub fn table(rows: &[Row], limit: usize) -> String {
    let total: u64 = rows.iter().map(|row| row.gas).sum();
    let mut out: Vec<String> = rows
        .iter()
        .take(limit)
        .map(|row| {
            let share = if total > 0 { row.gas as f64 * 100.0 / total as f64 } else { 0.0 };
            format!("{:>9}  {:>5.1}%  {:>6} steps  {}", row.gas, share, row.steps, row.label)
        })
        .collect();
    if rows.len() > limit {
        out.push(format!("… {} more", rows.len() - limit));
    }
    out.push(format!("{total:>9}  total execution gas (intrinsic and refunds not included)"));
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn call_gas_is_not_counted_twice_and_frames_are_named() {
        // depth 1: PUSH (3), CALL (cost 1000, forwards 900), depth 2: two steps, back to depth 1: STOP.
        let logs = vec![
            json!({"pc":0,"op":"PUSH1","gas":5000,"gasCost":3,"depth":1,"stack":[],"memory":[]}),
            json!({"pc":2,"op":"CALL","gas":4997,"gasCost":1000,"depth":1,"stack":["0x20","0x0","0x4","0x0","0x0","0xbb","0x384"],"memory":["a9059cbb00000000000000000000000000000000000000000000000000000000"]}),
            json!({"pc":0,"op":"PUSH1","gas":900,"gasCost":3,"depth":2,"stack":[],"memory":[]}),
            json!({"pc":2,"op":"STOP","gas":897,"gasCost":0,"depth":2,"stack":[],"memory":[]}),
            json!({"pc":3,"op":"STOP","gas":4894,"gasCost":0,"depth":1,"stack":[],"memory":[]}),
        ];
        let trace = json!({"failed":false,"gas":21000,"returnValue":"","structLogs":logs});
        let session = crate::desktop::trace_debug::from_trace("t".into(), "0x00000000000000000000000000000000000000aa", &trace, Vec::new(), &HashMap::new()).unwrap();
        assert_eq!(own_costs(&session), vec![3, 100, 3, 0, 0]);
        let calls = by_call(&session);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].gas, 103);
        assert!(calls[1].label.ends_with(".0xa9059cbb"), "{}", calls[1].label);
        assert_eq!(calls[1].gas, 3);
        let lines = by_line(&session);
        assert_eq!(lines.iter().map(|row| row.gas).sum::<u64>(), 106);
        assert!(table(&lines, 1).contains("1 more"));
    }
}
