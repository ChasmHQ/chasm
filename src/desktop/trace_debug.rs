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
    /// Account whose storage and balance the step uses: the code's address,
    /// or the caller's under DELEGATECALL and CALLCODE.
    pub context: String,
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
    /// solc AST of `source`, for local variables.
    pub ast: Option<Value>,
    /// Byte ranges (start, length) of `runtime` that differ per deployment:
    /// immutables and linked library addresses. Ignored when matching code.
    pub variable: Vec<(usize, usize)>,
}

/// One transaction of a scenario session: where its steps start and how it ended.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Part {
    pub first: usize,
    pub title: String,
    pub target: String,
    pub calldata: Vec<u8>,
    pub failed: bool,
    pub gas_used: u64,
}

/// A watched storage value: stop where a write changes it.
#[derive(Clone, Debug, PartialEq)]
pub struct Watch {
    pub label: String,
    pub account: String,
    pub slot: ethers::types::U256,
    pub offset: u64,
    pub size: u64,
}

/// One SSTORE, named when the layout can explain the slot.
#[derive(Clone, Debug, PartialEq)]
pub struct StorageWrite {
    pub step: usize,
    /// Code that wrote, whose layout names the slot.
    pub address: String,
    /// Account whose storage changed.
    pub account: String,
    pub slot: ethers::types::U256,
    pub before: ethers::types::H256,
    pub after: ethers::types::H256,
    /// Last step of the reverted frame that undoes this write, if any.
    pub undone_at: Option<usize>,
}

/// State before the execution, read from the node when it was recorded and
/// saved with the trace, so a reopened trace diffs without the node.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Prestate {
    /// account -> 0x slot -> 0x value before the execution.
    #[serde(default)]
    pub storage: HashMap<String, HashMap<String, String>>,
    /// account -> (balance before, balance after); mined transactions only.
    #[serde(default)]
    pub balances: HashMap<String, (String, String)>,
}

/// One slot the execution changed, with its value before and after.
#[derive(Clone, Debug, PartialEq)]
pub struct SlotChange {
    pub account: String,
    pub address: String,
    pub slot: ethers::types::U256,
    /// None when the slot was neither read first nor fetched from the node.
    pub before: Option<ethers::types::H256>,
    pub after: ethers::types::H256,
}

#[derive(Clone, Debug, Default)]
pub struct Location {
    pub contract: String,
    pub file: String,
    pub line: usize,
    /// Byte range of the mapped expression within the line, when known.
    /// Kept for highlighting the expression; the listing shows whole lines.
    #[allow(dead_code)]
    pub start: usize,
    #[allow(dead_code)]
    pub length: usize,
}

#[derive(Debug, Default)]
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
    /// (account, slot) -> value before the execution: read by an SLOAD
    /// before any write, or fetched from the node (`prestate`).
    observed: HashMap<(String, ethers::types::U256), ethers::types::H256>,
    prestate: HashMap<(String, ethers::types::U256), ethers::types::H256>,
    /// account -> (balance before, balance after).
    pub balances: Vec<(String, ethers::types::U256, ethers::types::U256)>,
    /// Calldata of the outermost call, for decoding arguments.
    pub calldata: Vec<u8>,
    /// Per program: byte offset where each source line starts, so a step's
    /// line is a binary search instead of a scan.
    line_starts: Vec<Vec<usize>>,
    /// Per program: how many steps ran each source line (index = line).
    line_hits: Vec<Vec<i32>>,
    /// Per program: local variable declarations from its AST.
    locals: Vec<Option<super::trace_locals::Index>>,
    /// ABIs to decode frames: matched programs, attached addresses, workspace.
    pub abis: super::call_tree::Abis,
    /// Breakpoints that need no source: by pc (any contract), by opcode, and
    /// on entering any call frame.
    pub pc_breaks: Vec<u64>,
    pub op_breaks: Vec<String>,
    pub break_on_entry: bool,
    /// Conditions on breakpoints, keyed `line N`, `pc N` or `op OP`.
    pub conditions: HashMap<String, String>,
    /// Standalone conditions: stop where one becomes true.
    pub when: Vec<String>,
    pub watches: Vec<Watch>,
    /// Transactions of a scenario, in order; empty for a single execution.
    pub parts: Vec<Part>,
    /// Frame tracking saved every `LOCALS_CHECKPOINT` steps of an external
    /// frame: (frame entry step, step) -> internal frames after that step.
    locals_cache: std::sync::Mutex<HashMap<(usize, usize), Vec<super::trace_locals::Frame>>>,
}

const LOCALS_CHECKPOINT: usize = 4096;

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
        let (line, line_start) = match self.line_starts.get(program) {
            Some(starts) => {
                let line = starts.partition_point(|at| *at <= start);
                (line, starts[line - 1])
            }
            None => (
                source[..start].bytes().filter(|byte| *byte == b'\n').count() + 1,
                source[..start].rfind('\n').map(|at| at + 1).unwrap_or(0),
            ),
        };
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
        // A condition that cannot be read at a step does not stop there.
        let holds = |session: &Self, key: String, index: usize| {
            session.conditions.get(&key).is_none_or(|condition| super::trace_condition::eval(session, index, condition).unwrap_or(false))
        };
        let hit = |session: &Self, index: usize| {
            let step = &session.steps[index];
            let entered = index > 0 && step.depth > session.steps[index - 1].depth;
            let line = session.location(index).map(|location| location.line).filter(|line| lines.contains(line));
            (line.is_some_and(|line| session.line_key(index) != session.line_key(index.wrapping_sub(1)) && holds(session, format!("line {line}"), index)))
                || (session.pc_breaks.contains(&step.pc) && holds(session, format!("pc {}", step.pc), index))
                || (session.op_breaks.iter().any(|op| op == &step.op) && holds(session, format!("op {}", step.op), index))
                || (session.break_on_entry && entered)
                || session.watch_hit(index).is_some()
                || session.when.iter().any(|condition| {
                    super::trace_condition::eval(session, index, condition).unwrap_or(false)
                        && (index == 0 || !super::trace_condition::eval(session, index - 1, condition).unwrap_or(false))
                })
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

    /// The watch a write at `index` changes, if any.
    pub fn watch_hit(&self, index: usize) -> Option<&Watch> {
        if self.watches.is_empty() || self.steps.get(index).is_none_or(|step| step.op != "SSTORE") {
            return None;
        }
        let at = self.writes.binary_search_by_key(&index, |write| write.step).ok()?;
        let write = &self.writes[at];
        self.watches.iter().find(|watch| {
            watch.account == write.account
                && watch.slot == write.slot
                && super::storage_layout::field(write.before, watch.offset, watch.size) != super::storage_layout::field(write.after, watch.offset, watch.size)
        })
    }

    /// Next step at this call depth or shallower: one instruction, stepping
    /// over calls.
    pub fn next_instruction(&mut self) -> bool {
        let Some(depth) = self.current().map(|step| step.depth) else { return false };
        match (self.cursor + 1..self.steps.len()).find(|&index| self.steps[index].depth <= depth) {
            Some(index) => self.go(index),
            None => self.go(self.steps.len().saturating_sub(1)),
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
    #[cfg(test)]
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

/// Joins transactions recorded one by one into one session, in order. Each
/// keeps its own frames, nesting and outcome; storage carries over, so the
/// value of a slot in a later transaction includes the earlier writes. The
/// state before the scenario is the first fetched value of each slot.
pub fn scenario(title: String, parts: Vec<(Session, Prestate)>) -> Result<Session, String> {
    let mut parts = parts.into_iter();
    let (mut merged, first_prestate) = parts.next().ok_or("A scenario needs at least one transaction.")?;
    merged.parts = vec![Part { first: 0, title: merged.title.clone(), target: merged.target.clone(), calldata: merged.calldata.clone(), failed: merged.failed, gas_used: merged.gas_used }];
    let mut prestate = first_prestate;
    for (session, part_prestate) in parts {
        if merged.steps.len() + session.steps.len() > MAX_STEPS {
            return Err(format!("The scenario is over {MAX_STEPS} steps; debug fewer transactions at once."));
        }
        merged.parts.push(Part { first: merged.steps.len(), title: session.title.clone(), target: session.target.clone(), calldata: session.calldata.clone(), failed: session.failed, gas_used: session.gas_used });
        merged.steps.extend(session.steps);
        for (address, program) in session.mapped {
            merged.mapped.entry(address).or_insert(program);
        }
        merged.truncated |= session.truncated;
        merged.gas_used += session.gas_used;
        // A slot keeps the value fetched before the first transaction that read it.
        for (account, slots) in part_prestate.storage {
            let known = prestate.storage.entry(account).or_default();
            for (slot, value) in slots {
                known.entry(slot).or_insert(value);
            }
        }
        for (account, (before, after)) in part_prestate.balances {
            prestate.balances.entry(account).and_modify(|span| span.1 = after.clone()).or_insert((before, after));
        }
    }
    merged.failed = merged.parts.iter().any(|part| part.failed);
    merged.title = title;
    merged.cursor = 0;
    merged.index_lines();
    merged.set_prestate(&prestate);
    Ok(merged)
}

/// Parse a struct-logger trace. `target` is the address called at depth 1.
pub fn from_trace(title: String, target: &str, trace: &Value, programs: Vec<Program>, code_at: &HashMap<String, Vec<u8>>) -> Result<Session, String> {
    let logs = trace["structLogs"].as_array().ok_or("The node returned no step trace. Restart the project so the node records steps.")?;
    let truncated = logs.len() > MAX_STEPS;
    // Track which address runs at each depth from CALL-family operands.
    let mut address_at: Vec<String> = vec![String::new(), target.to_ascii_lowercase()];
    // Storage context per depth: DELEGATECALL and CALLCODE keep the caller's.
    let mut context_at: Vec<String> = address_at.clone();
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
                let context = match op.as_str() {
                    "DELEGATECALL" | "CALLCODE" => context_at.get(prev_depth as usize).cloned().unwrap_or_default(),
                    _ => callee.clone().unwrap_or_default(),
                };
                address_at.truncate(depth as usize);
                address_at.resize(depth as usize, String::new());
                address_at.push(callee.unwrap_or_default());
                context_at.truncate(depth as usize);
                context_at.resize(depth as usize, String::new());
                context_at.push(context);
            }
        }
        address_at.truncate(depth as usize + 1);
        context_at.truncate(depth as usize + 1);
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
            context: context_at.get(depth as usize).cloned().unwrap_or_default(),
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
        if let Some(index) = programs.iter().position(|program| program_matches(program, code)) {
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
    session.index_lines();
    session.locals = session.programs.iter().map(|program| program.ast.as_ref().map(|ast| super::trace_locals::Index::build(ast, program.source_id))).collect();
    Ok(session)
}

impl Session {
    /// Line offsets and per-line hit counts, computed once per trace: the
    /// listing shows hit counts on every repaint.
    fn index_lines(&mut self) {
        self.line_starts = self
            .programs
            .iter()
            .map(|program| std::iter::once(0).chain(program.source.match_indices('\n').map(|(at, _)| at + 1)).collect())
            .collect();
        let mut hits: Vec<Vec<i32>> = self.programs.iter().map(|program| vec![0; program.source.lines().count() + 2]).collect();
        for index in 0..self.steps.len() {
            let Some(program) = self.mapped.get(&self.steps[index].address).copied() else { continue };
            if let Some(location) = self.location(index) {
                if let Some(hit) = hits[program].get_mut(location.line) {
                    *hit += 1;
                }
            }
        }
        self.line_hits = hits;
    }

    /// Steps that ran `line` (1-based) of the program at `address`.
    pub fn line_hits(&self, address: &str, line: usize) -> i32 {
        self.mapped
            .get(address)
            .and_then(|program| self.line_hits.get(*program))
            .and_then(|hits| hits.get(line))
            .copied()
            .unwrap_or(0)
    }

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
        // Values before the execution: fetched from the node when known.
        let mut known: HashMap<(String, U256), H256> = self.prestate.clone();
        self.writes.clear();
        self.observed.clear();
        // Writes of each open frame, to mark them undone when it reverts.
        let mut open: Vec<Vec<usize>> = vec![Vec::new()];
        let ranges = self.part_ranges();
        for index in 0..self.steps.len() {
            if index > 0 && ranges.iter().any(|(first, ..)| *first == index) {
                open = vec![Vec::new()];
            }
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
                        let key = (step.context.clone(), slot);
                        if !known.contains_key(&key) {
                            self.observed.insert(key.clone(), value);
                            known.insert(key, value);
                        }
                    }
                }
                "SSTORE" => {
                    let (Some(slot), Some(value)) = (top(0), step.stack.get(step.stack.len().wrapping_sub(2)).and_then(|text| word(text))) else { continue };
                    let key = (step.context.clone(), slot);
                    let before = known.get(&key).copied().unwrap_or_default();
                    known.insert(key, value);
                    if self.writes.len() < 4096 {
                        self.writes.push(StorageWrite { step: index, address: step.address.clone(), account: step.context.clone(), slot, before, after: value, undone_at: None });
                        if let Some(frame) = open.last_mut() {
                            frame.push(self.writes.len() - 1);
                        }
                    }
                }
                _ => {}
            }
            let step = &self.steps[index];
            match self.steps.get(index + 1) {
                Some(next) if next.depth > step.depth => open.push(Vec::new()),
                Some(next) if next.depth < step.depth => {
                    // Leaving a frame: a revert (or failure) undoes its writes;
                    // otherwise they become the caller's.
                    let mine = open.pop().unwrap_or_default();
                    if step.op == "RETURN" || step.op == "STOP" || step.op == "SELFDESTRUCT" {
                        if let Some(parent) = open.last_mut() {
                            parent.extend(mine);
                        }
                    } else {
                        for at in mine.into_iter().rev() {
                            self.writes[at].undone_at = Some(index);
                            // The slot reads its value from before the frame again.
                            let write = &self.writes[at];
                            known.insert((write.account.clone(), write.slot), write.before);
                        }
                    }
                    if open.is_empty() {
                        open.push(Vec::new());
                    }
                }
                _ => {}
            }
        }
        // A transaction that failed keeps none of its writes.
        for (first, last, failed) in ranges {
            if failed {
                for write in self.writes.iter_mut().filter(|write| first <= write.step && write.step <= last) {
                    write.undone_at.get_or_insert(last);
                }
            }
        }
        let _ = logs;
    }

    /// Solidity locals of the innermost function at the cursor, when the
    /// running contract's AST is known: (name, type, value).
    pub fn locals(&self) -> Option<Vec<(String, String, String)>> {
        let step = self.current()?;
        let program = *self.mapped.get(&step.address)?;
        let index = self.locals.get(program)?.as_ref()?;
        let view = |at: usize| {
            let item = &self.steps[at];
            let instruction = self.pc_index[program].get(&item.pc).copied();
            super::trace_locals::StepView {
                pc: item.pc,
                op: &item.op,
                stack_len: item.stack.len(),
                range: instruction.and_then(|instruction| self.ranges[program].get(instruction).copied().flatten()),
                jump: instruction.and_then(|instruction| self.jumps[program].get(instruction).copied()),
            }
        };
        // Steps of this external frame, from its entry to the cursor. Tracking
        // resumes from the newest checkpoint so long traces stay fast.
        let floor = self.part_first(self.cursor);
        let begin = (floor..=self.cursor).rev().take_while(|at| self.steps[*at].depth >= step.depth).last().unwrap_or(self.cursor);
        let own: Vec<usize> = (begin..=self.cursor).filter(|at| self.steps[*at].depth == step.depth).collect();
        let mut cache = self.locals_cache.lock().unwrap();
        let mut start = 0;
        let mut frames = Vec::new();
        for (position, at) in own.iter().enumerate().rev() {
            if let Some(saved) = cache.get(&(begin, *at)) {
                start = position;
                frames = saved.clone();
                break;
            }
        }
        for position in start..own.len().saturating_sub(1) {
            super::trace_locals::advance(index, &mut frames, &view(own[position]), &view(own[position + 1]));
            if (position + 1) % LOCALS_CHECKPOINT == 0 {
                cache.insert((begin, own[position + 1]), frames.clone());
            }
        }
        drop(cache);
        let offset = view(*own.last()?).range?.0;
        let frame = frames.last()?;
        // Only the outermost call's input is recorded; inner calls show calldata as pointers.
        let calldata = if step.depth == self.steps.get(floor).map(|first| first.depth).unwrap_or(1) { self.calldata_at(self.cursor) } else { &[] };
        Some(super::trace_locals::read(index, frame, offset, &step.stack, &step.memory, calldata).into_iter().map(|value| (value.name, value.type_label, value.value)).collect())
    }

    /// (first step, last step, failed) of each transaction.
    pub fn part_ranges(&self) -> Vec<(usize, usize, bool)> {
        let end = self.steps.len().saturating_sub(1);
        if self.parts.is_empty() {
            return vec![(0, end, self.failed)];
        }
        self.parts
            .iter()
            .enumerate()
            .map(|(at, part)| (part.first, self.parts.get(at + 1).map(|next| next.first.saturating_sub(1)).unwrap_or(end), part.failed))
            .collect()
    }

    /// Index of the transaction running at `index` (0 for a single execution).
    pub fn part_at(&self, index: usize) -> usize {
        self.parts.iter().rposition(|part| part.first <= index).unwrap_or(0)
    }

    /// First step of the transaction running at `index`.
    pub fn part_first(&self, index: usize) -> usize {
        self.parts.get(self.part_at(index)).map(|part| part.first).unwrap_or(0)
    }

    /// Calldata of the transaction running at `index`.
    pub fn calldata_at(&self, index: usize) -> &[u8] {
        self.parts.get(self.part_at(index)).map(|part| &part.calldata[..]).unwrap_or(&self.calldata)
    }

    /// Values before the execution, from the node. Recomputes the writes'
    /// previous values, which were zero for slots the trace never read.
    pub fn set_prestate(&mut self, prestate: &Prestate) {
        use ethers::types::{H256, U256};
        let parse = |text: &str| U256::from_str_radix(text.trim_start_matches("0x"), 16).ok();
        self.prestate = prestate
            .storage
            .iter()
            .flat_map(|(account, slots)| slots.iter().filter_map(move |(slot, value)| Some(((account.to_ascii_lowercase(), parse(slot)?), { let mut bytes = [0u8; 32]; parse(value)?.to_big_endian(&mut bytes); H256::from(bytes) }))))
            .collect();
        self.balances = prestate
            .balances
            .iter()
            .filter_map(|(account, (before, after))| Some((account.to_ascii_lowercase(), parse(before)?, parse(after)?)))
            .filter(|(_, before, after)| before != after)
            .collect();
        self.balances.sort();
        self.collect_storage(&[]);
    }

    /// Every (account, slot) the trace wrote, to fetch their earlier values.
    pub fn written_slots(&self) -> Vec<(String, ethers::types::U256)> {
        let mut out: Vec<(String, ethers::types::U256)> = Vec::new();
        for write in &self.writes {
            if !out.iter().any(|(account, slot)| account == &write.account && *slot == write.slot) {
                out.push((write.account.clone(), write.slot));
            }
        }
        out
    }

    /// (account, code address) pairs that ran, so a proxy's storage is read
    /// with its implementation's layout.
    pub fn storage_views(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        for step in &self.steps {
            if !step.context.is_empty() && !out.iter().any(|(account, address)| account == &step.context && address == &step.address) {
                out.push((step.context.clone(), step.address.clone()));
            }
        }
        out
    }

    /// Accounts whose code ran, for balance changes.
    pub fn accounts(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for step in &self.steps {
            if !step.context.is_empty() && !out.contains(&step.context) {
                out.push(step.context.clone());
            }
        }
        out
    }

    /// Value of a slot at step `at`: the last write up to it (undone writes
    /// restored), else the value before the execution if known.
    pub fn storage_at(&self, account: &str, slot: ethers::types::U256, at: usize) -> Option<ethers::types::H256> {
        let key = (account.to_ascii_lowercase(), slot);
        let initial = self.prestate.get(&key).or_else(|| self.observed.get(&key)).copied();
        let mut value = initial;
        // (step, order, value): writes, then undos in reverse write order. An
        // undo restores the write's previous value; for the first write of a
        // slot that is only known when the value before the execution is.
        let mut events: Vec<(usize, usize, Option<ethers::types::H256>)> = Vec::new();
        for (order, write) in self.writes.iter().filter(|write| write.account == key.0 && write.slot == slot).enumerate() {
            events.push((write.step, order, Some(write.after)));
            if let Some(undone) = write.undone_at {
                let restored = if order == 0 { initial } else { Some(write.before) };
                events.push((undone, usize::MAX - order, restored));
            }
        }
        events.sort_by_key(|(step, order, _)| (*step, *order));
        for (step, _, after) in events {
            if step > at {
                break;
            }
            value = after;
        }
        value
    }

    /// Net storage changes of the whole execution, in order of first write.
    pub fn state_diff(&self) -> Vec<SlotChange> {
        let mut out = Vec::new();
        let end = self.steps.len().saturating_sub(1);
        for (account, slot) in self.written_slots() {
            let key = (account.clone(), slot);
            let before = self.prestate.get(&key).or_else(|| self.observed.get(&key)).copied();
            let Some(after) = self.storage_at(&account, slot, end) else { continue };
            if before == Some(after) {
                continue;
            }
            let address = self.writes.iter().rev().find(|write| write.account == account && write.slot == slot).map(|write| write.address.clone()).unwrap_or_default();
            out.push(SlotChange { account, address, slot, before, after });
        }
        out
    }

    /// keccak preimages for naming slots: the trace's own, plus mapping
    /// entries keyed by any address in the trace or word of its calldata.
    pub fn naming_preimages(&self, layout: &super::storage_layout::Layout) -> HashMap<ethers::types::U256, Vec<u8>> {
        let mut keys: Vec<[u8; 32]> = Vec::new();
        let mut add = |key: [u8; 32]| {
            if keys.len() < 64 && !keys.contains(&key) {
                keys.push(key);
            }
        };
        for account in self.accounts().iter().chain(std::iter::once(&self.target)) {
            if let Ok(bytes) = ethers::utils::hex::decode(account.trim_start_matches("0x")) {
                if bytes.len() == 20 {
                    let mut key = [0u8; 32];
                    key[12..].copy_from_slice(&bytes);
                    add(key);
                }
            }
        }
        let calldata: Vec<&[u8]> = if self.parts.is_empty() { vec![&self.calldata] } else { self.parts.iter().map(|part| &part.calldata[..]).collect() };
        for input in calldata {
            for chunk in input.get(4..).unwrap_or_default().chunks(32).filter(|chunk| chunk.len() == 32) {
                add(chunk.try_into().unwrap());
            }
        }
        let mut out = layout.guessed_preimages(&keys);
        out.extend(self.preimages.iter().map(|(hash, bytes)| (*hash, bytes.clone())));
        out
    }

    /// Storage layout of the code running at `address`.
    pub fn layout_at(&self, address: &str) -> Option<super::storage_layout::Layout> {
        let program = self.programs.get(*self.mapped.get(address)?)?;
        super::storage_layout::Layout::from_json(program.layout.as_ref()?).ok()
    }

    /// Function and decoded arguments of the outermost call, when its ABI is known.
    /// Decoded arguments of the call frame at the cursor (the outermost call
    /// when the cursor is in it), using every ABI the session knows.
    pub fn call_arguments(&self) -> Option<(String, Vec<(String, String)>)> {
        let frames = super::call_tree::frames(self);
        let index = super::call_tree::frame_at(&frames, self.cursor).unwrap_or(0);
        let mut abis = self.abis.clone();
        // A matched program's own ABI decodes its frames even with no attach.
        for (address, program) in &self.mapped {
            if let Some(abi) = &self.programs[*program].abi {
                abis.by_address.entry(address.clone()).or_insert_with(|| (self.programs[*program].name.clone(), abi.clone()));
            }
        }
        super::call_tree::arguments(frames.get(index)?, &abis)
    }

    /// The call tree with names, for the Calls tab and console.
    pub fn call_frames(&self) -> Vec<(super::call_tree::Frame, String)> {
        let mut abis = self.abis.clone();
        for (address, program) in &self.mapped {
            if let Some(abi) = &self.programs[*program].abi {
                abis.by_address.entry(address.clone()).or_insert_with(|| (self.programs[*program].name.clone(), abi.clone()));
            }
        }
        super::call_tree::frames(self).into_iter().map(|frame| {
            let text = super::call_tree::describe(self, &frame, &abis);
            (frame, text)
        }).collect()
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

pub fn token_text(token: &ethers::abi::Token) -> String {
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

/// Length of `code` without solc's CBOR metadata tail. The tail ends in its
/// own 2-byte length and starts with a CBOR map (0xa1-0xa5); code built
/// with `bytecode_hash = none` and no metadata has no tail to strip.
fn without_metadata(code: &[u8]) -> usize {
    if code.len() < 2 {
        return code.len();
    }
    let meta = u16::from_be_bytes([code[code.len() - 2], code[code.len() - 1]]) as usize + 2;
    if meta >= code.len() || meta < 3 {
        return code.len();
    }
    let start = code.len() - meta;
    if (0xa1..=0xa5).contains(&code[start]) { start } else { code.len() }
}

/// Runtime code equality ignoring the CBOR metadata tail solc appends.
#[cfg(test)]
pub fn same_code(compiled: &[u8], deployed: &[u8]) -> bool {
    same_code_except(compiled, deployed, &[])
}

/// Like `same_code`, ignoring the bytes in `variable` (immutables, links).
pub fn same_code_except(compiled: &[u8], deployed: &[u8], variable: &[(usize, usize)]) -> bool {
    let (a, b) = (without_metadata(compiled), without_metadata(deployed));
    if a != b || a == 0 {
        return false;
    }
    let mut skip = vec![false; a];
    for &(start, length) in variable {
        for flag in skip.iter_mut().skip(start).take(length) {
            *flag = true;
        }
    }
    compiled[..a].iter().zip(&deployed[..b]).zip(&skip).all(|((x, y), skip)| *skip || x == y)
}

/// Whether deployed `code` is this program's runtime.
pub fn program_matches(program: &Program, code: &[u8]) -> bool {
    same_code_except(&program.runtime, code, &program.variable)
}

/// Variable byte ranges from a deployedBytecode artifact: immutableReferences
/// (`{id: [{start, length}]}`) and linkReferences (`{file: {lib: [{start, length}]}}`).
pub fn variable_ranges(deployed: &Value) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut push = |item: &Value| {
        if let (Some(start), Some(length)) = (item["start"].as_u64(), item["length"].as_u64()) {
            ranges.push((start as usize, length as usize));
        }
    };
    for list in deployed["immutableReferences"].as_object().into_iter().flat_map(|map| map.values()) {
        list.as_array().into_iter().flatten().for_each(&mut push);
    }
    for libraries in deployed["linkReferences"].as_object().into_iter().flat_map(|map| map.values()) {
        for list in libraries.as_object().into_iter().flat_map(|map| map.values()) {
            list.as_array().into_iter().flatten().for_each(&mut push);
        }
    }
    ranges
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
            ast: None,
            variable: Vec::new(),
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
        let program = Program { name: "A".into(), runtime: runtime.clone(), source_map: map, source_path: "A.sol".into(), source, source_id: Some(0), layout: None, abi: None, ast: None, variable: Vec::new() };
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
    fn code_match_tolerates_immutables_links_and_missing_metadata() {
        // bytecode_hash = none: no CBOR tail, so the last bytes are code and must match.
        assert!(same_code(&[1, 2, 3, 4], &[1, 2, 3, 4]));
        assert!(!same_code(&[1, 2, 3, 4], &[1, 2, 3, 5]));
        // An immutable at bytes 1..3 differs per deployment.
        assert!(same_code_except(&[0x60, 0, 0, 0x55], &[0x60, 0xab, 0xcd, 0x55], &[(1, 2)]));
        assert!(!same_code_except(&[0x60, 0, 0, 0x55], &[0x61, 0xab, 0xcd, 0x55], &[(1, 2)]));
        let deployed = json!({
            "immutableReferences": {"7": [{"start": 1, "length": 2}]},
            "linkReferences": {"src/L.sol": {"L": [{"start": 10, "length": 20}]}},
        });
        assert_eq!(variable_ranges(&deployed), vec![(1, 2), (10, 20)]);
        // A real CBOR tail (a2 ... 0033) is still stripped.
        let mut compiled = vec![0x60, 0x01];
        compiled.extend([0xa2, 0x64, 0x69, 0x70, 0x00, 0x04]);
        let mut deployed = vec![0x60, 0x01];
        deployed.extend([0xa2, 0x64, 0x69, 0x71, 0x00, 0x04]);
        assert!(same_code(&compiled, &deployed));
    }

    #[test]
    fn code_match_ignores_metadata_but_not_body() {
        assert!(same_code(&[1, 2, 3, 0xa1, 0xaa, 0x00, 0x02], &[1, 2, 3, 0xa1, 0xbb, 0x00, 0x02]));
        assert!(!same_code(&[1, 2, 4, 0xa1, 0xaa, 0x00, 0x02], &[1, 2, 3, 0xa1, 0xbb, 0x00, 0x02]));
    }
}
