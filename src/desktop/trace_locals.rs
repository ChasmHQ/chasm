//! Solidity local variables in a recorded trace, ported from the sevm
//! debugger (`chasm_debugger/locals`). solc gives no location for locals; the
//! AST says what each one is and where it is in scope, and the run says where
//! it is on the stack:
//!
//! - **Frame base.** An internal call is a JUMP marked `i`. At the JUMPDEST it
//!   lands on, the arguments sit directly below the stack top.
//! - **Allocation site.** The instruction that allocates a local is mapped to
//!   the declaration's own source range (`uint256 fee`), not the statement's.
//!   The stack height before it is the local's slot for the rest of the frame.
//!
//! Legacy (non via-IR) codegen only; anything uncertain shows as unavailable.
use serde_json::Value;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Param,
    Return,
    Local,
}

#[derive(Clone, Debug)]
pub struct Decl {
    pub id: i64,
    pub name: String,
    pub type_string: String,
    pub location: String,
    pub kind: Kind,
    pub start: usize,
    pub length: usize,
    pub scope_start: usize,
    pub scope_end: usize,
    pub function: i64,
}

impl Decl {
    fn visible_at(&self, offset: usize) -> bool {
        (self.scope_start..self.scope_end).contains(&offset) && (self.kind != Kind::Local || offset >= self.start)
    }

    /// Stack words the value takes; None when unknown, which hides it and
    /// every parameter deeper than it rather than misreading neighbours.
    fn slots(&self) -> Option<usize> {
        let t = self.type_string.trim();
        if t.is_empty() {
            return None;
        }
        if t.starts_with("function ") {
            return Some(if t.contains(" external") { 2 } else { 1 });
        }
        if self.location == "calldata" && (t.contains("[]") || t.starts_with("string") || t.starts_with("bytes ") || t == "bytes") {
            return Some(2);
        }
        Some(1)
    }

    fn base_type(&self) -> String {
        let t = self.type_string.trim();
        for suffix in [" storage ref", " storage pointer", " memory", " calldata", " storage"] {
            if let Some(stripped) = t.strip_suffix(suffix) {
                return stripped.to_string();
            }
        }
        t.to_string()
    }
}

#[derive(Clone, Debug, Default)]
pub struct Function {
    pub id: i64,
    pub start: usize,
    pub end: usize,
    params: Vec<usize>,
    returns: Vec<usize>,
    body: Vec<usize>,
}

/// Declarations of one source file, from its solc AST.
#[derive(Clone, Debug, Default)]
pub struct Index {
    pub functions: Vec<Function>,
    pub decls: Vec<Decl>,
    by_range: HashMap<(usize, usize), usize>,
    modifiers: Vec<i64>,
}

fn src(node: &Value) -> Option<(usize, usize, i64)> {
    let mut parts = node["src"].as_str()?.split(':');
    let start = parts.next()?.parse().ok()?;
    let length = parts.next()?.parse().ok()?;
    let file = parts.next().and_then(|text| text.parse().ok()).unwrap_or(-1);
    Some((start, length, file))
}

const SCOPES: [&str; 7] = ["Block", "ForStatement", "UncheckedBlock", "FunctionDefinition", "ModifierDefinition", "TryCatchClause", "ContractDefinition"];

impl Index {
    /// Index the declarations in `file` (solc source id) of an AST.
    pub fn build(ast: &Value, file: Option<u32>) -> Self {
        let mut scopes = HashMap::new();
        collect_scopes(ast, &mut scopes);
        let mut index = Index::default();
        index.walk(ast, None, &scopes, file.map(i64::from));
        for function in &mut index.functions {
            for list in [&mut function.params, &mut function.returns, &mut function.body] {
                list.sort_by_key(|decl| index.decls[*decl].start);
            }
        }
        index
    }

    fn walk(&mut self, node: &Value, function: Option<&Value>, scopes: &HashMap<i64, (i64, usize, usize)>, file: Option<i64>) {
        match node {
            Value::Array(items) => items.iter().for_each(|item| self.walk(item, function, scopes, file)),
            Value::Object(map) => {
                let kind = map.get("nodeType").and_then(Value::as_str).unwrap_or("");
                let mut function = function;
                if matches!(kind, "FunctionDefinition" | "ModifierDefinition") {
                    function = Some(node);
                    if let Some((start, length, node_file)) = src(node) {
                        if file.is_none_or(|file| file == node_file) {
                            let id = node["id"].as_i64().unwrap_or(-1);
                            self.functions.push(Function { id, start, end: start + length, ..Default::default() });
                            if kind == "ModifierDefinition" {
                                self.modifiers.push(id);
                            }
                        }
                    }
                } else if kind == "VariableDeclaration" && node["stateVariable"] != true {
                    if let Some(function) = function {
                        self.add(node, function, scopes, file);
                    }
                }
                for value in map.values() {
                    if value.is_object() || value.is_array() {
                        self.walk(value, function, scopes, file);
                    }
                }
            }
            _ => {}
        }
    }

    fn add(&mut self, node: &Value, function: &Value, scopes: &HashMap<i64, (i64, usize, usize)>, file: Option<i64>) {
        let Some((start, length, node_file)) = src(node) else { return };
        if file.is_some_and(|file| file != node_file) {
            return;
        }
        let id = node["id"].as_i64().unwrap_or(-1);
        let function_id = function["id"].as_i64().unwrap_or(-1);
        let Some(owner) = self.functions.iter().position(|item| item.id == function_id) else { return };
        // A declaration whose scope cannot be placed is dropped, not guessed.
        let Some(&(scope_file, scope_start, scope_end)) = node["scope"].as_i64().and_then(|scope| scopes.get(&scope)) else { return };
        if scope_file != node_file || self.decls.iter().any(|decl| decl.id == id) {
            return;
        }
        let listed = |key: &str| function[key]["parameters"].as_array().is_some_and(|items| items.iter().any(|item| item["id"].as_i64() == Some(id)));
        let kind = if listed("parameters") { Kind::Param } else if listed("returnParameters") { Kind::Return } else { Kind::Local };
        let decl = Decl {
            id,
            name: node["name"].as_str().unwrap_or("").to_string(),
            type_string: node["typeDescriptions"]["typeString"].as_str().unwrap_or("").to_string(),
            location: node["storageLocation"].as_str().unwrap_or("default").to_string(),
            kind,
            start,
            length,
            scope_start,
            scope_end,
            function: function_id,
        };
        let at = self.decls.len();
        self.by_range.insert((start, length), at);
        self.decls.push(decl);
        let function = &mut self.functions[owner];
        match kind {
            Kind::Param => function.params.push(at),
            Kind::Return => function.returns.push(at),
            Kind::Local => function.body.push(at),
        }
    }

    /// The innermost function or modifier whose body contains `offset`.
    pub fn function_at(&self, offset: usize) -> Option<&Function> {
        self.functions.iter().filter(|function| (function.start..function.end).contains(&offset)).min_by_key(|function| function.end - function.start)
    }

    /// The local or return variable an instruction mapped to `range` allocates.
    pub fn allocated(&self, start: usize, length: usize) -> Option<&Decl> {
        self.by_range.get(&(start, length)).map(|at| &self.decls[*at]).filter(|decl| decl.kind != Kind::Param)
    }

    fn function(&self, id: i64) -> Option<&Function> {
        self.functions.iter().find(|function| function.id == id)
    }

    /// In-scope declarations at `offset` in a function, inner scopes shadowing outer.
    fn visible(&self, function: &Function, offset: usize) -> Vec<&Decl> {
        let mut chosen: HashMap<&str, &Decl> = HashMap::new();
        let mut unnamed = Vec::new();
        for at in function.params.iter().chain(&function.returns).chain(&function.body) {
            let decl = &self.decls[*at];
            if !decl.visible_at(offset) {
                continue;
            }
            if decl.name.is_empty() {
                unnamed.push(decl);
                continue;
            }
            let tighter = chosen.get(decl.name.as_str()).is_none_or(|previous| decl.scope_end - decl.scope_start <= previous.scope_end - previous.scope_start);
            if tighter {
                chosen.insert(&decl.name, decl);
            }
        }
        let mut out: Vec<&Decl> = chosen.into_values().chain(unnamed).collect();
        out.sort_by_key(|decl| (decl.kind != Kind::Param, decl.kind != Kind::Return, decl.start));
        out
    }

    fn is_modifier(&self, id: i64) -> bool {
        self.modifiers.contains(&id)
    }
}

fn collect_scopes(node: &Value, out: &mut HashMap<i64, (i64, usize, usize)>) {
    match node {
        Value::Array(items) => items.iter().for_each(|item| collect_scopes(item, out)),
        Value::Object(map) => {
            if map.get("nodeType").and_then(Value::as_str).is_some_and(|kind| SCOPES.contains(&kind)) {
                if let (Some(id), Some((start, length, file))) = (node["id"].as_i64(), src(node)) {
                    out.insert(id, (file, start, start + length));
                }
            }
            for value in map.values() {
                if value.is_object() || value.is_array() {
                    collect_scopes(value, out);
                }
            }
        }
        _ => {}
    }
}

/// One internal Solidity frame observed while replaying an external frame.
#[derive(Clone, Debug)]
pub struct Frame {
    pub function: Option<i64>,
    pub entry_sp: Option<usize>,
    pub slots: HashMap<i64, usize>,
}

/// What a step contributes to frame tracking, from the session.
pub struct StepView<'a> {
    pub pc: u64,
    pub op: &'a str,
    pub stack_len: usize,
    /// Absolute source offset and length of the step, in this program's file.
    pub range: Option<(usize, usize)>,
    pub jump: Option<char>,
}

/// Replays one external frame's steps (all at the same depth, in order, up to
/// and including the cursor) and returns its internal frames at the cursor.
pub fn frames_at(index: &Index, steps: &[StepView]) -> Vec<Frame> {
    let mut frames: Vec<Frame> = Vec::new();
    for at in 0..steps.len().saturating_sub(1) {
        let (step, next) = (&steps[at], &steps[at + 1]);
        // Back in a known function: frames above it have returned.
        if let Some(function) = step.range.and_then(|(start, _)| index.function_at(start)) {
            if let Some(found) = frames.iter().rposition(|frame| frame.function == Some(function.id)) {
                frames.truncate(found + 1);
            }
        }
        if let (Some((start, length)), Some(top)) = (step.range, frames.last_mut()) {
            if let Some(decl) = index.allocated(start, length) {
                if top.function == Some(decl.function) || index.is_modifier(decl.function) {
                    top.slots.insert(decl.id, step.stack_len);
                }
            }
        }
        if step.op != "JUMP" || next.pc == step.pc + 1 {
            continue;
        }
        match step.jump {
            Some('o') => {
                frames.pop();
            }
            Some('i') => {
                let function = next.range.and_then(|(start, _)| index.function_at(start)).map(|function| function.id);
                // The dispatcher's jump into a public function's body re-enters it.
                if function.is_some() && frames.last().is_some_and(|top| top.function == function && top.entry_sp.is_none()) {
                    frames.last_mut().unwrap().entry_sp = Some(next.stack_len);
                    continue;
                }
                frames.push(Frame { function, entry_sp: Some(next.stack_len), slots: HashMap::new() });
            }
            _ => {}
        }
    }
    frames
}

/// One decoded local: name, type label, value text.
#[derive(Clone, Debug, PartialEq)]
pub struct Value3 {
    pub name: String,
    pub type_label: String,
    pub value: String,
}

/// Locals of the innermost frame at the cursor. `stack` is bottom first, as
/// the struct logger records it; `memory` is 32-byte hex words.
/// `calldata` is the frame's input, for calldata arrays and strings.
pub fn read(index: &Index, frame: &Frame, offset: usize, stack: &[String], memory: &[String], calldata: &[u8]) -> Vec<Value3> {
    let Some(function) = frame.function.and_then(|id| index.function(id)) else { return Vec::new() };
    let mut positions: HashMap<i64, usize> = HashMap::new();
    if let Some(mut cursor) = frame.entry_sp {
        for at in function.params.iter().rev() {
            let decl = &index.decls[*at];
            let Some(width) = decl.slots() else { break };
            let Some(below) = cursor.checked_sub(width) else { break };
            cursor = below;
            positions.insert(decl.id, cursor);
        }
    }
    positions.extend(frame.slots.iter().map(|(id, slot)| (*id, *slot)));
    let mut out = Vec::new();
    for decl in index.visible(function, offset) {
        let name = if decl.name.is_empty() { "<return>".to_string() } else { decl.name.clone() };
        let type_label = if matches!(decl.location.as_str(), "memory" | "storage" | "calldata") { format!("{} {}", decl.base_type(), decl.location) } else { decl.base_type() };
        let value = match (positions.get(&decl.id), decl.slots()) {
            (None, _) => "<not allocated yet>".to_string(),
            (_, None) => "<unknown stack width>".to_string(),
            (Some(base), Some(width)) if base + width > stack.len() => {
                if (decl.start..decl.start + decl.length).contains(&offset) { "<allocated by this step>".into() } else { "<out of scope>".into() }
            }
            (Some(base), Some(width)) => decode(decl, &stack[*base..base + width], memory, calldata),
        };
        out.push(Value3 { name, type_label, value });
    }
    out
}

fn word(text: &str) -> [u8; 32] {
    let digits = text.trim_start_matches("0x");
    let digits = format!("{digits:0>64}");
    let mut out = [0u8; 32];
    if let Ok(bytes) = ethers::utils::hex::decode(&digits[digits.len() - 64..]) {
        out.copy_from_slice(&bytes);
    }
    out
}

fn decode(decl: &Decl, words: &[String], memory: &[String], calldata: &[u8]) -> String {
    let t = decl.base_type();
    let raw = word(&words[0]);
    let number = ethers::types::U256::from_big_endian(&raw);
    match decl.location.as_str() {
        "storage" => return format!("<storage pointer, slot {number:#x}>"),
        "calldata" => {
            // Dynamic arrays and strings are (offset, length); elements follow the offset.
            let at = |text: &String| ethers::types::U256::from_big_endian(&word(text)).low_u64() as usize;
            if let ([offset, length], Some(element)) = (words, t.strip_suffix("[]")) {
                let (offset, length) = (at(offset), at(length));
                if length <= 64 && !element.contains('[') && element != "string" && element != "bytes" {
                    let items: Option<Vec<String>> = (0..length)
                        .map(|item| calldata.get(offset + 32 * item..offset + 32 * item + 32).map(|raw| value_type(element, raw.try_into().unwrap())))
                        .collect();
                    if let Some(items) = items {
                        return format!("[{}]", items.join(", "));
                    }
                }
            }
            if let ([offset, length], "string" | "bytes") = (words, t.as_str()) {
                if let Some(data) = calldata.get(at(offset)..at(offset) + at(length)) {
                    return match (t.as_str(), std::str::from_utf8(data)) {
                        ("string", Ok(text)) => format!("{text:?}"),
                        _ => format!("0x{}", ethers::utils::hex::encode(data)),
                    };
                }
            }
            return format!("<calldata at {}>", words.iter().map(|text| format!("{:#x}", at(text))).collect::<Vec<_>>().join(", "));
        }
        "memory" => return decode_memory(&t, number.low_u64() as usize, memory, 0).unwrap_or_else(|error| format!("<memory at {number:#x}: {error}>")),
        _ => {}
    }
    value_type(&t, &raw)
}

fn value_type(t: &str, raw: &[u8; 32]) -> String {
    let number = ethers::types::U256::from_big_endian(raw);
    if t == "bool" {
        return (!number.is_zero()).to_string();
    }
    if t.starts_with("address") || t.starts_with("contract ") {
        return ethers::utils::to_checksum(&ethers::types::Address::from_slice(&raw[12..]), None);
    }
    if let Some(size) = t.strip_prefix("bytes").and_then(|size| size.parse::<usize>().ok()).filter(|size| (1..=32).contains(size)) {
        return format!("0x{}", ethers::utils::hex::encode(&raw[..size]));
    }
    if t.starts_with("int") {
        return ethers::types::I256::from_raw(number).to_string();
    }
    if t.starts_with("uint") || t.starts_with("enum ") {
        if t.starts_with("uint") && number >= ethers::types::U256::exp10(15) && number < ethers::types::U256::exp10(27) {
            return format!("{number} ({} ether)", ethers::utils::format_units(number, 18).unwrap_or_default().trim_end_matches('0').trim_end_matches('.'));
        }
        return number.to_string();
    }
    format!("{number:#x}")
}

fn memory_bytes(memory: &[String], start: usize, length: usize) -> Vec<u8> {
    let flat: Vec<u8> = memory.iter().flat_map(|text| word(text)).collect();
    (start..start + length).map(|at| flat.get(at).copied().unwrap_or(0)).collect()
}

fn decode_memory(t: &str, pointer: usize, memory: &[String], depth: u32) -> Result<String, String> {
    if depth > 2 {
        return Err("nested too deeply".into());
    }
    let length_at = |at: usize| ethers::types::U256::from_big_endian(&memory_bytes(memory, at, 32));
    if t == "string" || t == "bytes" {
        let length = length_at(pointer);
        if length > ethers::types::U256::from(4096) {
            return Err("not initialised yet".into());
        }
        let data = memory_bytes(memory, pointer + 32, length.as_usize());
        return Ok(match (t, String::from_utf8(data.clone())) {
            ("string", Ok(text)) => format!("{text:?}"),
            _ => format!("0x{}", ethers::utils::hex::encode(data)),
        });
    }
    let Some(element) = t.strip_suffix("[]") else {
        return Err(format!("no decoder for {t}"));
    };
    let count = length_at(pointer);
    if count > ethers::types::U256::from(64) {
        return Err("not initialised yet".into());
    }
    let items: Result<Vec<String>, String> = (0..count.as_usize())
        .map(|at| {
            let raw: [u8; 32] = memory_bytes(memory, pointer + 32 + 32 * at, 32).try_into().unwrap();
            if element == "string" || element == "bytes" || element.ends_with("[]") {
                decode_memory(element, ethers::types::U256::from_big_endian(&raw).low_u64() as usize, memory, depth + 1)
            } else {
                Ok(value_type(element, &raw))
            }
        })
        .collect();
    Ok(format!("[{}]", items?.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// function f(uint256 a) returns (uint256 r) { uint256 b = a; } in file 0.
    fn ast() -> Value {
        json!({"nodeType":"SourceUnit","id":1,"src":"0:200:0","nodes":[{"nodeType":"ContractDefinition","id":2,"src":"0:200:0","nodes":[{
            "nodeType":"FunctionDefinition","id":3,"src":"10:100:0",
            "parameters":{"parameters":[{"nodeType":"VariableDeclaration","id":4,"name":"a","scope":3,"src":"20:9:0","storageLocation":"default","typeDescriptions":{"typeString":"uint256"}}]},
            "returnParameters":{"parameters":[{"nodeType":"VariableDeclaration","id":5,"name":"r","scope":3,"src":"40:9:0","storageLocation":"default","typeDescriptions":{"typeString":"uint256"}}]},
            "body":{"nodeType":"Block","id":6,"src":"50:60:0","statements":[{"nodeType":"VariableDeclarationStatement","src":"55:15:0","declarations":[
                {"nodeType":"VariableDeclaration","id":7,"name":"b","scope":6,"src":"55:9:0","storageLocation":"default","typeDescriptions":{"typeString":"uint256"}}]}]}
        }]}]})
    }

    #[test]
    fn frames_place_params_below_entry_and_locals_at_allocation() {
        let index = Index::build(&ast(), Some(0));
        assert_eq!(index.functions.len(), 1);
        assert_eq!(index.decls.len(), 3);
        let view = |pc, op, stack_len, range, jump| StepView { pc, op, stack_len, range, jump };
        // Jump into f with [ret, a] on the stack, allocate r at height 2 and b at height 3.
        let steps = [
            view(0, "JUMP", 3, Some((0, 5)), Some('i')),
            view(10, "JUMPDEST", 2, Some((10, 100)), None),
            view(11, "PUSH0", 2, Some((40, 9)), None),
            view(12, "PUSH0", 3, Some((55, 9)), None),
            view(13, "STOP", 4, Some((60, 1)), None),
        ];
        let frames = frames_at(&index, &steps);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].entry_sp, Some(2));
        assert_eq!(frames[0].slots.get(&5), Some(&2));
        assert_eq!(frames[0].slots.get(&7), Some(&3));
        let stack: Vec<String> = ["0xaa", "0x2a", "0x0", "0x7"].iter().map(|text| text.to_string()).collect();
        let values = read(&index, &frames[0], 60, &stack, &[], &[]);
        let shown: Vec<(String, String)> = values.iter().map(|value| (value.name.clone(), value.value.clone())).collect();
        assert_eq!(shown, vec![("a".into(), "42".into()), ("r".into(), "0".into()), ("b".into(), "7".into())]);
        // Before b's declaration only a and r are visible.
        assert_eq!(read(&index, &frames[0], 45, &stack, &[], &[]).len(), 2);
    }

    #[test]
    fn values_decode_by_type() {
        let raw = word("0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff");
        assert_eq!(value_type("int256", &raw), "-1");
        assert_eq!(value_type("bool", &word("0x1")), "true");
        assert_eq!(value_type("uint256", &word("0xde0b6b3a7640000")), "1000000000000000000 (1 ether)");
        let memory = vec![format!("{:064x}", 5), format!("{:0<64}", "68656c6c6f")];
        assert_eq!(decode_memory("string", 0, &memory, 0).unwrap(), "\"hello\"");
    }
}
