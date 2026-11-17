//! Step debugger for transactions and calls on the workspace's own Anvil node.
//!
//! The node records every executed opcode with its stack, memory and storage
//! (`debug_traceTransaction` / `debug_traceCall`, struct logger). Each step is
//! mapped back to a Solidity line with the compiler's deployed source map when
//! the executing contract's runtime code is one of the workspace artifacts.
//! Navigation is over the recorded steps, so stepping backwards is free.
//!
//! Step over and step out work on frames: an external call (deeper call
//! depth) and an internal Solidity function call (a JUMP the source map marks
//! `i`, returning through a JUMP marked `o`) each open one frame.
use serde_json::{json, Value};
use std::collections::HashMap;

/// A trace longer than this is cut; the summary says so.
pub const MAX_STEPS: usize = 200_000;

#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub pc: u64,
    pub op: String,
    pub gas: u64,
    pub gas_cost: u64,
    pub depth: u64,
    pub stack: Vec<String>,
    pub memory: Vec<String>,
    pub storage: Vec<(String, String)>,
    /// Address whose code runs at this step.
    pub address: String,
    /// Frame nesting: external call depth plus internal function calls.
    pub nest: u32,
}

/// One compiled contract the trace can be mapped to.
#[derive(Clone, Debug)]
pub struct Program {
    pub name: String,
    pub runtime: Vec<u8>,
    pub source_map: String,
    pub source_path: String,
    pub source: String,
    /// solc source id of `source`; None maps every non-generated entry.
    pub source_id: Option<u32>,
    /// Compiler storage layout JSON, for naming slots.
    pub layout: Option<Value>,
    pub abi: Option<ethers::abi::Abi>,
}

/// One SSTORE, named when the layout can explain the slot.
#[derive(Clone, Debug, PartialEq)]
pub struct StorageWrite {
    pub step: usize,
    pub address: String,
    pub slot: ethers::types::U256,
    pub before: ethers::types::H256,
    pub after: ethers::types::H256,
}

#[derive(Clone, Debug, Default)]
pub struct Location {
    pub contract: String,
    pub file: String,
    pub line: usize,
    /// Byte range of the mapped expression within the line, when known.
    pub start: usize,
    pub length: usize,
}

#[derive(Clone, Debug, Default)]
pub struct Session {
    pub title: String,
    pub target: String,
    pub steps: Vec<Step>,
    pub cursor: usize,
    pub failed: bool,
    pub return_value: String,
    pub gas_used: u64,
    pub truncated: bool,
    pub breakpoints: Vec<usize>,
    /// address -> program index, for addresses whose runtime code matched.
    pub mapped: HashMap<String, usize>,
    pub programs: Vec<Program>,
    /// Per program: pc -> instruction index, and instruction index -> source range.
    pc_index: Vec<HashMap<u64, usize>>,
    ranges: Vec<Vec<Option<(usize, usize)>>>,
    /// Per program: instruction index -> source map jump type (`i`, `o`, `-`).
    jumps: Vec<Vec<char>>,
    /// keccak result -> hashed bytes, from every KECCAK256 in the trace.
    pub preimages: HashMap<ethers::types::U256, Vec<u8>>,
    /// Every SSTORE with the slot value before and after.
    pub writes: Vec<StorageWrite>,
    /// Calldata of the outermost call, for decoding arguments.
    pub calldata: Vec<u8>,
}

impl Session {
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    pub fn current(&self) -> Option<&Step> {
        self.steps.get(self.cursor)
    }

    pub fn location(&self, index: usize) -> Option<Location> {
        let step = self.steps.get(index)?;
        let program = *self.mapped.get(&step.address)?;
        let instruction = *self.pc_index[program].get(&step.pc)?;
        let (start, length) = (*self.ranges[program].get(instruction)?)?;
        let source = &self.programs[program].source;
        if start > source.len() {
            return None;
        }
        let line = source[..start].bytes().filter(|byte| *byte == b'\n').count() + 1;
        let line_start = source[..start].rfind('\n').map(|at| at + 1).unwrap_or(0);
        Some(Location {
            contract: self.programs[program].name.clone(),
            file: self.programs[program].source_path.rsplit('/').next().unwrap_or_default().to_string(),
            line,
            start: start - line_start,
            length,
        })
    }

    pub fn source_lines(&self, index: usize) -> Option<(String, Vec<&str>)> {
        let step = self.steps.get(index)?;
        let program = &self.programs[*self.mapped.get(&step.address)?];
        Some((program.source_path.rsplit('/').next().unwrap_or_default().to_string(), program.source.lines().collect()))
    }

    // --- Navigation. Each returns true when the cursor moved.

    pub fn go(&mut self, index: usize) -> bool {
        let index = index.min(self.steps.len().saturating_sub(1));
        let moved = index != self.cursor;
        self.cursor = index;
        moved
    }

    pub fn step_opcode(&mut self, forward: bool) -> bool {
        let target = if forward { self.cursor + 1 } else { self.cursor.saturating_sub(1) };
        self.go(target)
    }

    /// Next step on a different source line, entering any call (step into).
    /// Backwards it is the reverse: the previous line, wherever it ran.
    pub fn step_line(&mut self, forward: bool) -> bool {
        let start_line = self.line_key(self.cursor);
        let mut index = self.cursor;
        loop {
            let next = if forward { index + 1 } else { index.wrapping_sub(1) };
            if next >= self.steps.len() {
                return self.go(index);
            }
            index = next;
            let key = self.line_key(index);
            if key.is_some() && key != start_line {
                return self.go(index);
            }
        }
    }

    /// Next different line in this frame or an outer one (step over): calls
    /// and internal functions made by the line run to completion.
    pub fn next_line(&mut self, forward: bool) -> bool {
        let nest = self.steps.get(self.cursor).map(|step| step.nest).unwrap_or(0);
        let start_line = self.line_key(self.cursor);
        let mut index = self.cursor;
        loop {
            let next = if forward { index + 1 } else { index.wrapping_sub(1) };
            if next >= self.steps.len() {
                return self.go(index);
            }
            index = next;
            if self.steps[index].nest > nest {
                continue;
            }
            let key = self.line_key(index);
            if key.is_some() && key != start_line {
                return self.go(index);
            }
        }
    }

    /// Step out: the first step after the current frame (external call or
    /// internal function) returns. Backwards: the step that entered it.
    pub fn finish(&mut self, forward: bool) -> bool {
        let nest = self.steps.get(self.cursor).map(|step| step.nest).unwrap_or(0);
        let found = if forward {
            (self.cursor + 1..self.steps.len()).find(|&index| self.steps[index].nest < nest)
        } else {
            (0..self.cursor).rev().find(|&index| self.steps[index].nest < nest)
        };
        match found {
            Some(index) => self.go(index),
            None => self.go(if forward { self.steps.len().saturating_sub(1) } else { 0 }),
        }
    }

    /// Run to the next breakpoint (or the end); backwards runs to the previous one.
    pub fn resume(&mut self, forward: bool) -> bool {
        let lines: Vec<_> = self.breakpoints.clone();
        let hit = |session: &Self, index: usize| {
            session.location(index).is_some_and(|location| lines.contains(&location.line))
                && session.line_key(index) != session.line_key(index.wrapping_sub(1))
        };
        if forward {
            match (self.cursor + 1..self.steps.len()).find(|&index| hit(self, index)) {
                Some(index) => self.go(index),
                None => self.go(self.steps.len().saturating_sub(1)),
            }
        } else {
            match (0..self.cursor).rev().find(|&index| hit(self, index)) {
                Some(index) => self.go(index),
                None => self.go(0),
            }
        }
    }

    /// Next step whose opcode matches (e.g. SSTORE, CALL, REVERT).
    pub fn next_opcode(&mut self, opcode: &str, forward: bool) -> bool {
        let opcode = opcode.to_ascii_uppercase();
        let found = if forward {
            (self.cursor + 1..self.steps.len()).find(|&index| self.steps[index].op == opcode)
        } else {
            (0..self.cursor).rev().find(|&index| self.steps[index].op == opcode)
        };
        found.is_some_and(|index| self.go(index))
    }

    pub fn toggle_breakpoint(&mut self, line: usize) {
        match self.breakpoints.iter().position(|item| *item == line) {
            Some(at) => {
                self.breakpoints.remove(at);
            }
            None => self.breakpoints.push(line),
        }
    }

    fn line_key(&self, index: usize) -> Option<(String, usize)> {
        let location = self.location(index)?;
        Some((location.contract, location.line))
    }

    /// Call stack at the cursor: one entry per depth, from the outermost frame.
    pub fn frames(&self) -> Vec<(u64, String, String)> {
        let mut frames: Vec<(u64, String, String)> = Vec::new();
        for index in 0..=self.cursor.min(self.steps.len().saturating_sub(1)) {
            let step = &self.steps[index];
            while frames.last().is_some_and(|(depth, ..)| *depth > step.depth) {
                frames.pop();
            }
            if frames.last().map(|(depth, ..)| *depth) != Some(step.depth) {
                let name = self.mapped.get(&step.address).map(|program| self.programs[*program].name.clone()).unwrap_or_else(|| "unknown code".into());
                frames.push((step.depth, step.address.clone(), name));
            }
        }
        frames
    }
}

/// Parse a struct-logger trace. `target` is the address called at depth 1.
pub fn from_trace(title: String, target: &str, trace: &Value, programs: Vec<Program>, code_at: &HashMap<String, Vec<u8>>) -> Result<Session, String> {
    let logs = trace["structLogs"].as_array().ok_or("The node returned no step trace. Restart the project so the node records steps.")?;
    let truncated = logs.len() > MAX_STEPS;
    // Track which address runs at each depth from CALL-family operands.
    let mut address_at: Vec<String> = vec![String::new(), target.to_ascii_lowercase()];
    let mut steps = Vec::with_capacity(logs.len().min(MAX_STEPS));
    let mut previous: Option<(u64, String, Vec<String>)> = None;
    for log in logs.iter().take(MAX_STEPS) {
        let depth = log["depth"].as_u64().unwrap_or(1);
        if let Some((prev_depth, op, stack)) = previous.take() {
            if depth > prev_depth {
                // CALL/STATICCALL/DELEGATECALL/CALLCODE: address is stack[-2];
                // DELEGATECALL and CALLCODE keep running in the caller's storage but
                // execute the callee's code, which is what the listing needs.
                let callee = match op.as_str() {
                    "CALL" | "CALLCODE" | "STATICCALL" | "DELEGATECALL" => stack.get(stack.len().wrapping_sub(2)).map(|word| word_address(word)),
                    _ => None, // CREATE/CREATE2: unknown until it returns
                };
                address_at.truncate(depth as usize);
                address_at.resize(depth as usize, String::new());
                address_at.push(callee.unwrap_or_default());
            }
        }
        address_at.truncate(depth as usize + 1);
        let stack: Vec<String> = log["stack"].as_array().map(|items| items.iter().filter_map(|item| item.as_str().map(normalize_word)).collect()).unwrap_or_default();
        let op = log["op"].as_str().unwrap_or("").to_string();
        steps.push(Step {
            pc: log["pc"].as_u64().unwrap_or(0),
            op: op.clone(),
            gas: log["gas"].as_u64().unwrap_or(0),
            gas_cost: log["gasCost"].as_u64().unwrap_or(0),
            depth,
            memory: log["memory"].as_array().map(|items| items.iter().filter_map(|item| item.as_str().map(str::to_string)).collect()).unwrap_or_default(),
            storage: log["storage"].as_object().map(|map| map.iter().map(|(slot, value)| (slot.clone(), value.as_str().unwrap_or("").to_string())).collect()).unwrap_or_default(),
            address: address_at.get(depth as usize).cloned().unwrap_or_default(),
            nest: 0,
            stack: stack.clone(),
        });
        previous = Some((depth, op, stack));
    }
    let mut session = Session {
        title,
        target: target.to_ascii_lowercase(),
        failed: trace["failed"].as_bool().unwrap_or(false),
        return_value: trace["returnValue"].as_str().unwrap_or("").to_string(),
        gas_used: trace["gas"].as_u64().unwrap_or(0),
        truncated,
        ..Default::default()
    };
    for program in &programs {
        session.pc_index.push(pc_to_instruction(&program.runtime));
        session.ranges.push(parse_source_map(&program.source_map, program.source_id));
    }
    for (address, code) in code_at {
        if let Some(index) = programs.iter().position(|program| same_code(&program.runtime, code)) {
            session.mapped.insert(address.to_ascii_lowercase(), index);
        }
    }
    for program in &programs {
        session.jumps.push(jump_types(&program.source_map));
    }
    session.programs = programs;
    session.steps = steps;
    session.assign_nesting();
    session.collect_storage(logs);
    Ok(session)
}

impl Session {
    /// keccak preimages (from memory at each KECCAK256 and the hash on the
    /// next step's stack) and SSTOREs with their previous value. The value
    /// before a write is the last one this trace saw for the slot, from an
    /// earlier SLOAD/SSTORE in the same contract; unseen slots start at zero.
    fn collect_storage(&mut self, logs: &[Value]) {
        use ethers::types::{H256, U256};
        let word = |text: &str| -> Option<H256> {
            let bytes = ethers::utils::hex::decode(format!("{:0>64}", text.trim_start_matches("0x"))).ok()?;
            (bytes.len() == 32).then(|| H256::from_slice(&bytes))
        };
        let mut known: HashMap<(String, U256), H256> = HashMap::new();
        for index in 0..self.steps.len() {
            let step = &self.steps[index];
            let top = |back: usize| step.stack.get(step.stack.len().wrapping_sub(1 + back)).and_then(|text| U256::from_str_radix(text.trim_start_matches("0x"), 16).ok());
            match step.op.as_str() {
                "KECCAK256" | "SHA3" => {
                    let (Some(offset), Some(length)) = (top(0), top(1)) else { continue };
                    let Some(next) = self.steps.get(index + 1) else { continue };
                    let Some(hash) = next.stack.last().and_then(|text| U256::from_str_radix(text.trim_start_matches("0x"), 16).ok()) else { continue };
                    if length > U256::from(256u64) || offset > U256::from(1u64 << 24) || self.preimages.len() >= 4096 {
                        continue;
                    }
                    let memory: Vec<u8> = step.memory.iter().flat_map(|chunk| ethers::utils::hex::decode(chunk.trim_start_matches("0x")).unwrap_or_default()).collect();
                    let (start, end) = (offset.as_usize(), offset.as_usize() + length.as_usize());
                    if end <= memory.len() {
                        self.preimages.insert(hash, memory[start..end].to_vec());
                    }
                }
                "SLOAD" => {
                    let Some(slot) = top(0) else { continue };
                    if let Some(value) = self.steps.get(index + 1).and_then(|next| next.stack.last()).and_then(|text| word(text)) {
                        known.entry((step.address.clone(), slot)).or_insert(value);
                    }
                }
                "SSTORE" => {
                    let (Some(slot), Some(value)) = (top(0), step.stack.get(step.stack.len().wrapping_sub(2)).and_then(|text| word(text))) else { continue };
                    let key = (step.address.clone(), slot);
                    let before = known.get(&key).copied().unwrap_or_default();
                    known.insert(key, value);
                    if self.writes.len() < 4096 {
                        self.writes.push(StorageWrite { step: index, address: step.address.clone(), slot, before, after: value });
                    }
                }
                _ => {}
            }
        }
        let _ = logs;
    }

    /// Storage layout of the code running at `address`.
    pub fn layout_at(&self, address: &str) -> Option<super::storage_layout::Layout> {
        let program = self.programs.get(*self.mapped.get(address)?)?;
        super::storage_layout::Layout::from_json(program.layout.as_ref()?).ok()
    }

    /// Function and decoded arguments of the outermost call, when its ABI is known.
    pub fn call_arguments(&self) -> Option<(String, Vec<(String, String)>)> {
        use ethers::abi::FunctionExt;
        let program = self.programs.get(*self.mapped.get(&self.target)?)?;
        let abi = program.abi.as_ref()?;
        if self.calldata.len() < 4 {
            return None;
        }
        let function = abi.functions().find(|function| function.selector() == self.calldata[..4])?;
        let tokens = function.decode_input(&self.calldata[4..]).ok()?;
        let args = function
            .inputs
            .iter()
            .zip(tokens)
            .map(|(param, token)| (if param.name.is_empty() { param.kind.to_string() } else { param.name.clone() }, token_text(&token)))
            .collect();
        Some((function.abi_signature(), args))
    }

    fn assign_nesting(&mut self) {
        let mut nest = 0u32;
        // Nest at the call site, per open external call.
        let mut calls: Vec<u32> = Vec::new();
        let mut previous_depth = self.steps.first().map(|step| step.depth).unwrap_or(1);
        for index in 0..self.steps.len() {
            let depth = self.steps[index].depth;
            if depth > previous_depth {
                calls.push(nest);
                nest += 1;
            }
            while depth < previous_depth {
                nest = calls.pop().unwrap_or(0);
                previous_depth -= 1;
            }
            previous_depth = depth;
            self.steps[index].nest = nest;
            if self.steps[index].op == "JUMP" {
                let floor = calls.last().map(|base| base + 1).unwrap_or(0);
                match self.jump_type(index) {
                    Some('i') => nest += 1,
                    Some('o') if nest > floor => nest -= 1,
                    _ => {}
                }
            }
        }
    }

    fn jump_type(&self, index: usize) -> Option<char> {
        let step = &self.steps[index];
        let program = *self.mapped.get(&step.address)?;
        let instruction = *self.pc_index[program].get(&step.pc)?;
        self.jumps[program].get(instruction).copied()
    }
}

fn token_text(token: &ethers::abi::Token) -> String {
    use ethers::abi::Token;
    match token {
        Token::Address(address) => ethers::utils::to_checksum(address, None),
        Token::Uint(value) | Token::Int(value) => value.to_string(),
        Token::Bool(value) => value.to_string(),
        Token::String(text) => format!("{text:?}"),
        Token::Bytes(bytes) | Token::FixedBytes(bytes) => format!("0x{}", ethers::utils::hex::encode(bytes)),
        Token::Array(items) | Token::FixedArray(items) => format!("[{}]", items.iter().map(token_text).collect::<Vec<_>>().join(", ")),
        Token::Tuple(items) => format!("({})", items.iter().map(token_text).collect::<Vec<_>>().join(", ")),
    }
}

/// Jump type per instruction from a compressed source map (field 4).
pub fn jump_types(map: &str) -> Vec<char> {
    let mut jump = '-';
    map.split(';')
        .map(|entry| {
            if let Some(kind) = entry.split(':').nth(3).and_then(|text| text.chars().next()) {
                jump = kind;
            }
            jump
        })
        .collect()
}

/// Runtime code equality ignoring the CBOR metadata tail solc appends.
fn same_code(compiled: &[u8], deployed: &[u8]) -> bool {
    let strip = |code: &[u8]| -> usize {
        if code.len() < 2 {
            return code.len();
        }
        let meta = u16::from_be_bytes([code[code.len() - 2], code[code.len() - 1]]) as usize + 2;
        if meta < code.len() { code.len() - meta } else { code.len() }
    };
    let (a, b) = (strip(compiled), strip(deployed));
    a == b && compiled[..a] == deployed[..b] && a > 0
}

fn normalize_word(text: &str) -> String {
    let digits = text.trim_start_matches("0x").trim_start_matches('0');
    format!("0x{}", if digits.is_empty() { "0" } else { digits })
}

fn word_address(word: &str) -> String {
    let digits = word.trim_start_matches("0x");
    let digits = format!("{digits:0>40}");
    format!("0x{}", &digits[digits.len() - 40..]).to_ascii_lowercase()
}

/// Map each PC to its instruction index (PUSH data is skipped).
pub fn pc_to_instruction(code: &[u8]) -> HashMap<u64, usize> {
    let mut map = HashMap::new();
    let mut pc = 0usize;
    let mut index = 0usize;
    while pc < code.len() {
        map.insert(pc as u64, index);
        let op = code[pc];
        pc += 1 + if (0x60..=0x7f).contains(&op) { (op - 0x5f) as usize } else { 0 };
        index += 1;
    }
    map
}

/// Decode a compressed solc source map into (start, length) per instruction.
/// Entries for generated code (file -1), and with `file` set for any file but
/// that one (imports, solc's generated Yul helpers), have no source range.
pub fn parse_source_map(map: &str, file_id: Option<u32>) -> Vec<Option<(usize, usize)>> {
    let mut out = Vec::new();
    let (mut start, mut length, mut file) = (0i64, 0i64, 0i64);
    for entry in map.split(';') {
        let fields: Vec<&str> = entry.split(':').collect();
        let pick = |index: usize, current: i64| fields.get(index).filter(|text| !text.is_empty()).and_then(|text| text.parse::<i64>().ok()).unwrap_or(current);
        start = pick(0, start);
        length = pick(1, length);
        file = pick(2, file);
        let own = file >= 0 && file_id.is_none_or(|id| file == i64::from(id));
        out.push((own && start >= 0 && length >= 0).then_some((start as usize, length as usize)));
    }
    out
}

/// Readable text for one 32-byte word: hex plus decimal when small.
pub fn describe_word(word: &str) -> String {
    let digits = word.trim_start_matches("0x");
    match ethers::types::U256::from_str_radix(digits, 16) {
        Ok(value) if value < ethers::types::U256::from(1_000_000_000_000_000_000_000u128) => format!("{word}  ({value})"),
        _ => word.to_string(),
    }
}

/// Struct-logger request options: full memory and storage, no return data limits.
pub fn trace_options() -> Value {
    json!({"enableMemory": true, "disableStack": false, "disableStorage": false})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_map_inherits_missing_fields_and_skips_generated_code() {
        // The file index carries over too, so 20:2 stays in generated code until :0.
        let ranges = parse_source_map("10:5:0:-;;12::0;:3:-1;20:2;30:1:0", None);
        assert_eq!(ranges, vec![Some((10, 5)), Some((10, 5)), Some((12, 5)), None, None, Some((30, 1))]);
        // Entries in another file (here solc's generated helpers, id 1) are not this source.
        let ranges = parse_source_map("10:5:0;40:9:1;12:2:0", Some(0));
        assert_eq!(ranges, vec![Some((10, 5)), None, Some((12, 2))]);
    }

    #[test]
    fn push_data_is_not_an_instruction() {
        // PUSH1 0x80 PUSH1 0x40 MSTORE
        let map = pc_to_instruction(&[0x60, 0x80, 0x60, 0x40, 0x52]);
        assert_eq!((map[&0], map[&2], map[&4]), (0, 1, 2));
        assert!(!map.contains_key(&1));
    }

    fn fixture() -> Session {
        let source = "contract A {\n  function f() public {\n    x = 1;\n    y = 2;\n  }\n}\n".to_string();
        let line3 = source.find("x = 1").unwrap();
        let line4 = source.find("y = 2").unwrap();
        // Five one-byte instructions: lines 3, 3, 4, 4, 4.
        let program = Program {
            name: "A".into(),
            // Five JUMPDESTs, then a one-byte metadata tail and its length (1).
            runtime: vec![0x5b, 0x5b, 0x5b, 0x5b, 0x5b, 0xa2, 0x00, 0x01],
            source_map: format!("{line3}:5:0;;{line4}:5:0;;"),
            source_path: "src/A.sol".into(),
            source,
            source_id: Some(0),
            layout: None,
            abi: None,
        };
        let logs: Vec<Value> = (0..5).map(|pc| json!({"pc":pc,"op":if pc == 3 {"SSTORE"} else {"JUMPDEST"},"gas":100-pc,"gasCost":1,"depth":1,"stack":["0x01"],"memory":[]})).collect();
        let trace = json!({"failed":false,"gas":21000,"returnValue":"","structLogs":logs});
        let mut code = HashMap::new();
        code.insert("0x00000000000000000000000000000000000000aa".to_string(), vec![0x5b, 0x5b, 0x5b, 0x5b, 0x5b, 0xa3, 0x00, 0x01]);
        from_trace("t".into(), "0x00000000000000000000000000000000000000AA", &trace, vec![program], &code).unwrap()
    }

    #[test]
    fn steps_map_to_lines_and_navigation_moves_by_line_and_opcode() {
        let mut session = fixture();
        assert_eq!(session.steps.len(), 5);
        assert_eq!(session.location(0).unwrap().line, 3);
        assert_eq!(session.location(2).unwrap().line, 4);
        assert!(session.step_line(true));
        assert_eq!(session.cursor, 2);
        assert!(session.step_line(false));
        assert_eq!(session.cursor, 1);
        assert!(session.next_opcode("sstore", true));
        assert_eq!(session.cursor, 3);
        session.go(0);
        session.toggle_breakpoint(4);
        assert!(session.resume(true));
        assert_eq!(session.cursor, 2);
        assert!(session.resume(true));
        assert_eq!(session.cursor, 4);
        assert_eq!(session.frames(), vec![(1, "0x00000000000000000000000000000000000000aa".into(), "A".into())]);
    }

    /// f() calls g() internally on line 3; g's body is line 6.
    fn internal_call_fixture() -> Session {
        let source = "contract A {\n  function f() public {\n    g();\n    x = 1;\n  }\n  function g() internal {\n    y = 2;\n  }\n}\n".to_string();
        let call = source.find("g();").unwrap();
        let after = source.find("x = 1").unwrap();
        let body = source.find("y = 2").unwrap();
        // pc: 0 line 3, 1 JUMP into g, 2-3 line 7 (g body), 4 JUMP out, 5 line 4.
        let map = format!("{call}:4:0:-;{call}:4:0:i;{body}:5:0:-;{body}:5:0:-;{body}:5:0:o;{after}:5:0:-");
        let runtime = vec![0x5b, 0x56, 0x5b, 0x5b, 0x56, 0x5b, 0xa2, 0x00, 0x01];
        let program = Program { name: "A".into(), runtime: runtime.clone(), source_map: map, source_path: "A.sol".into(), source, source_id: Some(0), layout: None, abi: None };
        let ops = ["JUMPDEST", "JUMP", "JUMPDEST", "JUMPDEST", "JUMP", "JUMPDEST"];
        let logs: Vec<Value> = ops.iter().enumerate().map(|(pc, op)| json!({"pc":pc,"op":op,"gas":100,"gasCost":1,"depth":1,"stack":[],"memory":[]})).collect();
        let trace = json!({"failed":false,"gas":21000,"returnValue":"","structLogs":logs});
        let mut code = HashMap::new();
        code.insert("0x00000000000000000000000000000000000000aa".to_string(), runtime);
        from_trace("t".into(), "0x00000000000000000000000000000000000000aa", &trace, vec![program], &code).unwrap()
    }

    #[test]
    fn step_over_skips_internal_functions_and_step_out_leaves_them() {
        let mut session = internal_call_fixture();
        assert_eq!(session.steps.iter().map(|step| step.nest).collect::<Vec<_>>(), vec![0, 0, 1, 1, 1, 0]);
        // Over from line 3 lands on line 4, not inside g().
        assert!(session.next_line(true));
        assert_eq!(session.location(session.cursor).unwrap().line, 4);
        // Back over returns to line 3 without stopping in g().
        assert!(session.next_line(false));
        assert_eq!(session.location(session.cursor).unwrap().line, 3);
        // Into enters g().
        session.go(0);
        assert!(session.step_line(true));
        assert_eq!(session.location(session.cursor).unwrap().line, 7);
        // Out leaves g() to the caller's next step; back out returns to the call.
        assert!(session.finish(true));
        assert_eq!(session.cursor, 5);
        session.go(3);
        assert!(session.finish(false));
        assert_eq!(session.cursor, 1);
    }

    #[test]
    fn jump_types_carry_over_like_other_fields() {
        assert_eq!(jump_types("1:2:0:i;;3:4:0:o;5:6;7:8:0:-"), vec!['i', 'i', 'o', 'o', '-']);
    }

    #[test]
    fn code_match_ignores_metadata_but_not_body() {
        assert!(same_code(&[1, 2, 3, 0xaa, 0x00, 0x01], &[1, 2, 3, 0xbb, 0x00, 0x01]));
        assert!(!same_code(&[1, 2, 4, 0xaa, 0x00, 0x01], &[1, 2, 3, 0xbb, 0x00, 0x01]));
    }
}
