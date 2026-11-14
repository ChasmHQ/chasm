use serde_json::{json, Value};
use std::collections::VecDeque;

pub const MAX_COMMAND_BYTES: usize = 4096;
pub const MAX_HISTORY: usize = 50;
pub const MAX_ENTRIES: usize = 100;
pub const MAX_STORED_BYTES: usize = 64 * 1024;
const MAX_OUTPUT_BYTES: usize = 16 * 1024;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const MAX_MEMORY_OFFSET: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub enum ConsoleAction {
    Local(LocalQuery),
    Remote { method: &'static str, params: Value },
    Cancel,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalQuery {
    Help,
    History,
    Pane(&'static str),
    Status,
    Clear,
}

/// Parses one bounded command. Expressions still run only through discard-only evaluation.
pub fn parse(input: &str) -> Result<ConsoleAction, String> {
    if input.len() > MAX_COMMAND_BYTES {
        return Err("Command exceeds 4096 bytes.".into());
    }
    if input.chars().any(forbidden_input_char) || input.contains(';') || input.contains('@') {
        return Err("Control characters, command batches, and @file are not supported.".into());
    }
    if input.contains('$') {
        return Err("Evaluation history references are not supported.".into());
    }
    if input.contains("//") || input.contains("/*") || input.contains("*/") {
        return Err("Comments and URLs are not supported in console commands.".into());
    }
    let input = input.trim();
    if input.is_empty() {
        return Err("Enter a command.".into());
    }
    let (verb, args) = split_first(input);
    match verb {
        "help" | "h" | "?" => local_no_args(args, LocalQuery::Help),
        "history" => local_no_args(args, LocalQuery::History),
        "clear" => local_no_args(args, LocalQuery::Clear),
        "status" => local_no_args(args, LocalQuery::Status),
        "cancel" => {
            no_args(args)?;
            Ok(ConsoleAction::Cancel)
        }
        "c" | "cont" | "continue" => navigation_no_args(args, "continue"),
        "finish" | "fin" => navigation_no_args(args, "finish"),
        "run" | "r" | "reset" | "restart" => navigation_no_args(args, "restart"),
        "s" | "step" => step(args, "step_line"),
        "si" | "stepi" => step(args, "step_opcode"),
        "n" | "next" => step(args, "next_line"),
        "until" | "u" | "advance" => {
            if args.is_empty() {
                navigation_no_args(args, "next_line")
            } else {
                Ok(remote(
                    "navigate",
                    json!({"action":"run_to", "location":location(args, true)?}),
                ))
            }
        }
        "p" | "print" | "inspect" | "ptype" => {
            let expression = required(args, "Enter a Solidity expression.")?;
            reject_vm(expression)?;
            Ok(remote(
                "evaluate",
                json!({"expression":expression, "keep":false}),
            ))
        }
        "bt" | "backtrace" | "where" => local_no_args(args, LocalQuery::Pane("frames")),
        "info" | "i" => info(args),
        "list" | "l" => window(args, "sourceStart", 1, MAX_SAFE_INTEGER, false),
        "disassemble" | "disas" => window(args, "aroundPc", 0, u32::MAX as u64, true),
        // The worker always returns its fixed 64-word memory window, not GDB formatting.
        "x" | "x/32xb" => {
            let value = address_integer(
                required(args, "Enter a memory byte offset.")?,
                MAX_MEMORY_OFFSET,
            )?;
            Ok(remote(
                "snapshot",
                json!({"memoryOffset":value.to_string()}),
            ))
        }
        "break" | "b" | "br" => breakpoint(args, "add"),
        "breakop" => breakpoint(args, "opcode"),
        "watch" => {
            let expression = required(args, "Enter a watch expression.")?;
            if expression.split_whitespace().any(|word| word == "if") {
                return Err("Watchpoint conditions are not supported.".into());
            }
            reject_vm(expression)?;
            Ok(remote(
                "breakpoint",
                json!({"action":"watch", "expression":expression}),
            ))
        }
        "delete" | "d" => {
            let id = exact_integer(
                required(args, "Enter one breakpoint ID.")?,
                1,
                MAX_SAFE_INTEGER,
            )?;
            Ok(remote("breakpoint", json!({"action":"delete", "id":id})))
        }
        _ => Err("Unsupported console command. Enter help for available commands.".into()),
    }
}

fn forbidden_input_char(ch: char) -> bool {
    ch.is_control()
        || matches!(ch, '\u{00ad}' | '\u{061c}' | '\u{180e}' | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}')
}

fn split_first(text: &str) -> (&str, &str) {
    match text.find(char::is_whitespace) {
        Some(index) => (&text[..index], text[index..].trim()),
        None => (text, ""),
    }
}

fn required<'a>(text: &'a str, message: &str) -> Result<&'a str, String> {
    if text.is_empty() {
        Err(message.into())
    } else {
        Ok(text)
    }
}

fn no_args(args: &str) -> Result<(), String> {
    if args.is_empty() {
        Ok(())
    } else {
        Err("This command takes no arguments.".into())
    }
}

fn local_no_args(args: &str, query: LocalQuery) -> Result<ConsoleAction, String> {
    no_args(args)?;
    Ok(ConsoleAction::Local(query))
}

fn remote(method: &'static str, params: Value) -> ConsoleAction {
    ConsoleAction::Remote { method, params }
}

fn navigation_no_args(args: &str, action: &'static str) -> Result<ConsoleAction, String> {
    no_args(args)?;
    Ok(remote("navigate", json!({"action":action})))
}

fn step(args: &str, action: &'static str) -> Result<ConsoleAction, String> {
    if args.is_empty() {
        return navigation_no_args(args, action);
    }
    let count = exact_integer(args, 1, 1000)?;
    Ok(remote("navigate", json!({"action":action, "count":count})))
}

fn info(args: &str) -> Result<ConsoleAction, String> {
    let query =
        match args {
            "locals" => LocalQuery::Pane("locals"),
            "args" => LocalQuery::Pane("arguments"),
            "gas" => LocalQuery::Pane("gas"),
            "logs" => LocalQuery::Pane("logs"),
            "registers" => LocalQuery::Pane("registers"),
            "status" => LocalQuery::Status,
            "storage" => LocalQuery::Pane("storage"),
            "breakpoints" => LocalQuery::Pane("breakpoints"),
            _ => return Err(
                "Choose info locals, args, gas, logs, registers, status, storage, or breakpoints."
                    .into(),
            ),
        };
    Ok(ConsoleAction::Local(query))
}

fn exact_integer(text: &str, minimum: u64, maximum: u64) -> Result<u64, String> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("Enter one exact non-negative decimal integer.".into());
    }
    let value = text
        .parse::<u64>()
        .map_err(|_| "Integer is outside the allowed range.".to_string())?;
    if !(minimum..=maximum).contains(&value) {
        return Err(format!("Integer must be between {minimum} and {maximum}."));
    }
    Ok(value)
}

fn address_integer(text: &str, maximum: u64) -> Result<u64, String> {
    let Some(digits) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) else {
        return exact_integer(text, 0, maximum);
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Enter one exact decimal or hexadecimal integer.".into());
    }
    let value = u64::from_str_radix(digits, 16)
        .map_err(|_| "Integer is outside the allowed range.".to_string())?;
    if value > maximum {
        return Err(format!("Integer must be between 0 and {maximum}."));
    }
    Ok(value)
}

fn window(
    args: &str,
    key: &str,
    minimum: u64,
    maximum: u64,
    allow_star: bool,
) -> Result<ConsoleAction, String> {
    if args.is_empty() {
        return Ok(remote("snapshot", json!({})));
    }
    let value = if allow_star {
        args.strip_prefix('*').unwrap_or(args)
    } else {
        args
    };
    let value = if allow_star {
        address_integer(value, maximum)?
    } else {
        exact_integer(value, minimum, maximum)?
    };
    Ok(remote("snapshot", json!({(key): value.to_string()})))
}

fn location(text: &str, allow_line: bool) -> Result<String, String> {
    if text.len() > 1024 || text.chars().any(char::is_whitespace) {
        return Err("Enter one location of at most 1024 bytes.".into());
    }
    let lower = text.to_ascii_lowercase();
    if lower.contains("://") || lower.starts_with("file:") {
        return Err("URLs and file access are not supported.".into());
    }
    if let Some(pc) = text.strip_prefix('*') {
        return Ok(format!("*{}", address_integer(pc, u32::MAX as u64)?));
    }
    if let Some((file, line)) = text.rsplit_once(':') {
        if file.is_empty()
            || !file.ends_with(".sol")
            || !file
                .chars()
                .all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/'))
            || file.starts_with('/')
            || file
                .split('/')
                .any(|segment| segment.is_empty() || segment == "." || segment == "..")
        {
            return Err("Use a relative source location such as File.sol:42.".into());
        }
        let line = exact_integer(line, 1, MAX_SAFE_INTEGER)?;
        return Ok(format!("{file}:{line}"));
    }
    if text.bytes().all(|byte| byte.is_ascii_digit()) {
        if allow_line {
            return Ok(exact_integer(text, 1, MAX_SAFE_INTEGER)?.to_string());
        }
        return Err(
            "Use File.sol:line for a source breakpoint; a bare line is not supported.".into(),
        );
    }
    if !allow_line
        && !text.is_empty()
        && text
            .chars()
            .all(|ch| ch.is_alphanumeric() || ch == '_' || ch == '.')
        && text.chars().any(char::is_alphanumeric)
    {
        return Ok(text.to_string());
    }
    Err(if allow_line {
        "Use a line, File.sol:line, or *PC for run-to."
    } else {
        "Use File.sol:line, a function name, or *PC for a breakpoint."
    }
    .into())
}

fn breakpoint(args: &str, action: &'static str) -> Result<ConsoleAction, String> {
    let (target, remainder) = split_first(required(args, "Enter a breakpoint target.")?);
    let condition = if remainder.is_empty() {
        None
    } else {
        let (keyword, condition) = split_first(remainder);
        if keyword != "if" || condition.is_empty() {
            return Err("Use an optional 'if CONDITION' after the breakpoint target.".into());
        }
        reject_vm(condition)?;
        Some(condition)
    };
    let mut params = json!({"action":action});
    if action == "opcode" {
        if target.len() > 32
            || !target
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err(
                "Enter an opcode mnemonic of at most 32 letters, digits, or underscores.".into(),
            );
        }
        params["opcode"] = json!(target.to_ascii_uppercase());
    } else {
        params["location"] = json!(location(target, false)?);
    }
    if let Some(condition) = condition {
        params["condition"] = json!(condition);
    }
    Ok(remote("breakpoint", params))
}

fn reject_vm(expression: &str) -> Result<(), String> {
    // Whitespace between vm and '.' is significant in Solidity too.
    for (index, _) in expression.match_indices("vm") {
        let before = expression[..index].chars().next_back();
        if before.map_or(true, |ch| !ch.is_alphanumeric() && ch != '_' && ch != '$')
            && expression[index + 2..].trim_start().starts_with('.')
        {
            return Err("vm commands are not supported in the console.".into());
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsoleStatus {
    Pending,
    Ok,
    Error,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsoleEntry {
    pub id: u64,
    pub command: String,
    pub output: String,
    pub status: ConsoleStatus,
    pub operation_key: Option<u64>,
}

pub struct ConsoleState {
    draft: String,
    saved_draft: String,
    history: VecDeque<String>,
    history_index: Option<usize>,
    entries: VecDeque<ConsoleEntry>,
    next_id: u64,
    pub scroll_y: f32,
    pub follow: bool,
}

impl Default for ConsoleState {
    fn default() -> Self {
        Self {
            draft: String::new(),
            saved_draft: String::new(),
            history: VecDeque::new(),
            history_index: None,
            entries: VecDeque::new(),
            next_id: 1,
            scroll_y: 0.0,
            follow: true,
        }
    }
}

impl ConsoleState {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn draft(&self) -> &str {
        &self.draft
    }
    pub fn history(&self) -> &VecDeque<String> {
        &self.history
    }
    pub fn entries(&self) -> &VecDeque<ConsoleEntry> {
        &self.entries
    }

    pub fn set_draft(&mut self, draft: &str) {
        let draft = bounded(draft, MAX_COMMAND_BYTES);
        if draft != self.draft {
            self.draft = draft;
            self.history_index = None;
            self.saved_draft.clear();
        }
    }

    /// Stores the command once; callers must parse and authorize it separately.
    pub fn submit(&mut self, command: &str) -> u64 {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .expect("console entry IDs exhausted");
        let valid = parse(command).is_ok();
        let safe = if command.len() > MAX_COMMAND_BYTES {
            "[invalid command]".to_string()
        } else if sensitive(command) {
            "[redacted command]".to_string()
        } else if command.chars().any(forbidden_input_char) {
            "[invalid command]".to_string()
        } else {
            command.trim().to_string()
        };
        if valid && safe != "[redacted command]" && !safe.is_empty() {
            self.history.retain(|item| item != &safe);
            self.history.push_back(safe.clone());
        }
        self.entries.push_back(ConsoleEntry {
            id,
            command: safe,
            output: String::new(),
            status: ConsoleStatus::Pending,
            operation_key: None,
        });
        self.draft.clear();
        self.saved_draft.clear();
        self.history_index = None;
        self.enforce_limits();
        id
    }

    pub fn set_operation_key(&mut self, id: u64, key: u64) -> bool {
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.id == id && entry.status == ConsoleStatus::Pending)
        {
            entry.operation_key = Some(key);
            true
        } else {
            false
        }
    }

    /// A terminal result can be applied only once. Unknown or evicted IDs are ignored.
    pub fn finish(&mut self, id: u64, result: Result<String, String>) -> bool {
        let (status, output) = match result {
            Ok(output) => (ConsoleStatus::Ok, output),
            Err(output) => (ConsoleStatus::Error, output),
        };
        self.complete(id, status, &output)
    }

    pub fn cancel(&mut self, id: u64, reason: &str) -> bool {
        self.complete(id, ConsoleStatus::Cancelled, reason)
    }

    pub fn cancel_all_pending(&mut self, reason: &str) {
        let output = safe_output(reason);
        for entry in &mut self.entries {
            if entry.status == ConsoleStatus::Pending {
                entry.status = ConsoleStatus::Cancelled;
                entry.output = output.clone();
                entry.operation_key = None;
            }
        }
        self.enforce_limits();
    }

    fn complete(&mut self, id: u64, status: ConsoleStatus, output: &str) -> bool {
        let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.id == id && entry.status == ConsoleStatus::Pending)
        else {
            return false;
        };
        entry.status = status;
        entry.output = safe_output(output);
        entry.operation_key = None;
        self.enforce_limits();
        true
    }

    pub fn record_local(&mut self, command: &str, result: Result<String, String>) -> u64 {
        let id = self.submit(command);
        self.finish(id, result);
        id
    }

    pub fn previous(&mut self) -> &str {
        if self.history.is_empty() {
            return &self.draft;
        }
        let index = match self.history_index {
            None => {
                self.saved_draft = self.draft.clone();
                self.history.len() - 1
            }
            Some(index) => index.saturating_sub(1),
        };
        self.history_index = Some(index);
        self.draft = self.history[index].clone();
        &self.draft
    }

    pub fn next(&mut self) -> &str {
        if let Some(index) = self.history_index {
            if index + 1 < self.history.len() {
                self.history_index = Some(index + 1);
                self.draft = self.history[index + 1].clone();
            } else {
                self.history_index = None;
                self.draft = std::mem::take(&mut self.saved_draft);
            }
        }
        &self.draft
    }

    /// Clear completed entries without losing an in-flight operation.
    pub fn clear(&mut self) {
        self.entries
            .retain(|entry| entry.status == ConsoleStatus::Pending);
    }

    pub fn transcript_text(&self) -> String {
        let mut text = String::new();
        for entry in &self.entries {
            let status = match entry.status {
                ConsoleStatus::Pending => "pending",
                ConsoleStatus::Ok => "ok",
                ConsoleStatus::Error => "error",
                ConsoleStatus::Cancelled => "cancelled",
            };
            let row = format!("> {}\n[{}] {}\n", entry.command, status, entry.output);
            let remaining = MAX_STORED_BYTES - text.len();
            if row.len() > remaining {
                text.push_str(&bounded(&row, remaining));
                break;
            }
            text.push_str(&row);
        }
        text
    }

    fn enforce_limits(&mut self) {
        while self.history.len() > MAX_HISTORY {
            self.drop_oldest_history();
        }
        while self.entries.len() > MAX_ENTRIES {
            self.evict_entry();
        }
        // Reserve space for both the editable draft and the saved history draft.
        while self.stored_bytes() > MAX_STORED_BYTES - 2 * MAX_COMMAND_BYTES {
            if let Some(index) = self
                .entries
                .iter()
                .position(|entry| entry.status != ConsoleStatus::Pending)
            {
                self.entries.remove(index);
            } else if !self.history.is_empty() {
                self.drop_oldest_history();
            } else {
                self.entries.pop_front();
            }
        }
    }

    fn drop_oldest_history(&mut self) {
        self.history.pop_front();
        if let Some(index) = self.history_index {
            if index == 0 {
                self.history_index = None;
                self.draft = std::mem::take(&mut self.saved_draft);
            } else {
                self.history_index = Some(index - 1);
            }
        }
    }

    fn evict_entry(&mut self) {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.status != ConsoleStatus::Pending)
            .unwrap_or(0);
        self.entries.remove(index);
    }

    fn stored_bytes(&self) -> usize {
        self.history.iter().map(String::len).sum::<usize>()
            + self
                .entries
                .iter()
                .map(|entry| entry.command.len() + entry.output.len())
                .sum::<usize>()
    }
}

fn bounded(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    const MARKER: &str = "[truncated]";
    let reserve = if max >= MARKER.len() { MARKER.len() } else { 0 };
    let mut end = max - reserve;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut result = text[..end].to_string();
    if reserve != 0 {
        result.push_str(MARKER);
    }
    result
}

// This is a best-effort screen, not a guarantee that arbitrary free text has no secrets.
fn sensitive(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "privatekey",
        "private_key",
        "password",
        "passphrase",
        "mnemonic",
        "seedphrase",
        "seed_phrase",
        "secret",
        "api_key",
        "apikey",
        "access_token",
        "authorization",
        "://",
    ]
    .iter()
    .any(|term| lower.contains(term))
}

fn safe_output(output: &str) -> String {
    let output = bounded(output, MAX_OUTPUT_BYTES);
    if sensitive(&output) {
        "[redacted output]".into()
    } else {
        let cleaned: String = output
            .chars()
            .map(|ch| {
                if ch != '\n' && forbidden_input_char(ch) {
                    '\u{fffd}'
                } else {
                    ch
                }
            })
            .collect();
        bounded(&cleaned, MAX_OUTPUT_BYTES)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote_params(input: &str, method: &'static str, params: Value) {
        assert_eq!(parse(input), Ok(remote(method, params)), "{input}");
    }

    #[test]
    fn aliases_and_gateway_shapes() {
        for command in ["c", "cont", "continue"] {
            remote_params(command, "navigate", json!({"action":"continue"}));
        }
        for command in ["s", "step"] {
            remote_params(command, "navigate", json!({"action":"step_line"}));
        }
        for command in ["si 1000", "stepi 1000"] {
            remote_params(
                command,
                "navigate",
                json!({"action":"step_opcode","count":1000}),
            );
        }
        for command in ["n", "next"] {
            remote_params(command, "navigate", json!({"action":"next_line"}));
        }
        for command in ["fin", "finish"] {
            remote_params(command, "navigate", json!({"action":"finish"}));
        }
        for command in ["r", "run", "reset", "restart"] {
            remote_params(command, "navigate", json!({"action":"restart"}));
        }
        for command in ["until", "u", "advance"] {
            remote_params(command, "navigate", json!({"action":"next_line"}));
        }
        for command in ["p x + 1", "print x + 1", "inspect x + 1", "ptype x + 1"] {
            remote_params(
                command,
                "evaluate",
                json!({"expression":"x + 1","keep":false}),
            );
        }
        for command in ["bt", "backtrace", "where"] {
            assert_eq!(
                parse(command),
                Ok(ConsoleAction::Local(LocalQuery::Pane("frames")))
            );
        }
        assert_eq!(
            parse("i args"),
            Ok(ConsoleAction::Local(LocalQuery::Pane("arguments")))
        );
        assert_eq!(
            parse("info status"),
            Ok(ConsoleAction::Local(LocalQuery::Status))
        );
        remote_params(
            "until *0x20",
            "navigate",
            json!({"action":"run_to","location":"*32"}),
        );
        remote_params(
            "u 42",
            "navigate",
            json!({"action":"run_to","location":"42"}),
        );
        remote_params("list 32", "snapshot", json!({"sourceStart":"32"}));
        remote_params("disas *0x20", "snapshot", json!({"aroundPc":"32"}));
        remote_params("x/32xb 0x40", "snapshot", json!({"memoryOffset":"64"}));
        remote_params(
            "b Token.sol:42 if x > 1",
            "breakpoint",
            json!({"action":"add","location":"Token.sol:42","condition":"x > 1"}),
        );
        remote_params(
            "br Token.transfer",
            "breakpoint",
            json!({"action":"add","location":"Token.transfer"}),
        );
        remote_params(
            "breakop sstore if x > 0",
            "breakpoint",
            json!({"action":"opcode","opcode":"SSTORE","condition":"x > 0"}),
        );
        remote_params(
            "watch balances[msg.sender]",
            "breakpoint",
            json!({"action":"watch","expression":"balances[msg.sender]"}),
        );
        remote_params("d 42", "breakpoint", json!({"action":"delete","id":42}));
    }

    #[test]
    fn rejects_unsupported_and_out_of_range_inputs() {
        for command in [
            "",
            " ",
            "c;run",
            "step\nnext",
            "step\t2",
            "step\u{202e}2",
            "step 0",
            "step 1001",
            "step 2.0",
            "si -1",
            "next 1 2",
            "run input",
            "ni",
            "nexti",
            "up",
            "down",
            "frame 1",
            "$1",
            "p $1",
            "source a",
            "shell ls",
            "!python",
            "set x=1",
            "call x()",
            "keep",
            "asm",
            "yul",
            "vm",
            "file",
            "env",
            "@file",
            "eth_call",
            "watch x if y",
            "p vm.warp(1)",
            "b 42",
            "until transfer",
            "until file:42",
            "b ../../etc/passwd.sol:42",
            "b file:///tmp/key.sol:42",
            "b Token.sol:0x2a",
            "step 0x2",
            "d 0",
            "d -1",
            "d 9007199254740992",
            "list 0",
            "disas *0x100000000",
            "x/32xb 16777217",
            "breakop SSTORE if",
            "breakop bad-op",
        ] {
            assert!(parse(command).is_err(), "unexpectedly accepted {command:?}");
        }
        assert!(parse(&"x".repeat(MAX_COMMAND_BYTES + 1)).is_err());
        remote_params(
            "list 9007199254740991",
            "snapshot",
            json!({"sourceStart":"9007199254740991"}),
        );
        remote_params(
            "d 9007199254740991",
            "breakpoint",
            json!({"action":"delete","id":9007199254740991_u64}),
        );
    }

    #[test]
    fn results_are_terminal_and_clear_preserves_pending() {
        let mut state = ConsoleState::new();
        let first = state.submit("step");
        let pending = state.submit("next");
        assert!(state.set_operation_key(first, 7));
        assert!(state.finish(first, Ok("done".into())));
        assert!(!state.finish(first, Err("late".into())));
        assert!(!state.cancel(first, "late"));
        state.clear();
        assert_eq!(state.entries().len(), 1);
        assert_eq!(state.entries()[0].id, pending);
        state.cancel_all_pending("context changed");
        assert_eq!(state.entries()[0].status, ConsoleStatus::Cancelled);
        assert!(!state.finish(pending, Ok("late".into())));
    }

    #[test]
    fn history_deduplicates_and_restores_draft() {
        let mut state = ConsoleState::new();
        for command in ["step", "next", "step"] {
            state.record_local(command, Ok(String::new()));
        }
        assert_eq!(
            state
                .history()
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["next", "step"]
        );
        state.set_draft("unfinished");
        assert_eq!(state.previous(), "step");
        assert_eq!(state.previous(), "next");
        assert_eq!(state.previous(), "next");
        assert_eq!(state.next(), "step");
        assert_eq!(state.next(), "unfinished");
    }

    #[test]
    fn host_resync_keeps_history_position_and_preserves_the_original_draft() {
        let mut state = ConsoleState::new();
        state.record_local("step", Ok(String::new()));
        state.record_local("next", Ok(String::new()));
        state.set_draft("unfinished");
        assert_eq!(state.previous(), "next");
        state.set_draft("next");
        assert_eq!(state.previous(), "step");
        state.set_draft("step");
        assert_eq!(state.next(), "next");
        state.set_draft("next");
        assert_eq!(state.next(), "unfinished");
        assert_eq!(state.previous(), "next");
        state.set_draft("changed selection");
        assert_eq!(state.previous(), "next");
        assert_eq!(state.next(), "changed selection");
    }

    #[test]
    fn multiline_submission_is_not_trimmed_into_a_history_command() {
        let mut state = ConsoleState::new();
        for input in [
            "step\n",
            "\nstep",
            "step\r\nnext",
            "step\u{2028}next",
            "step;next",
        ] {
            assert!(parse(input).is_err());
            let entry = state.submit(input);
            assert!(state.finish(entry, Err("rejected".into())));
        }
        assert!(state.history().is_empty());
        assert!(!state.transcript_text().contains("step\r"));
    }

    #[test]
    fn limits_are_utf8_safe_and_secrets_are_not_added_to_history() {
        let mut state = ConsoleState::new();
        for index in 0..120 {
            let id = state.submit(&format!("p {index}"));
            assert!(state.finish(id, Ok("界".repeat(10_000))));
        }
        assert!(state.history().len() <= MAX_HISTORY);
        assert!(state.entries().len() <= MAX_ENTRIES);
        assert!(
            state.stored_bytes() + state.draft.len() + state.saved_draft.len() <= MAX_STORED_BYTES
        );
        assert!(state.transcript_text().len() <= MAX_STORED_BYTES);
        state.submit("p password");
        assert!(!state.history().iter().any(|item| item.contains("password")));
        assert_eq!(
            state.entries().back().unwrap().command,
            "[redacted command]"
        );
        state.set_draft(&"界".repeat(5000));
        assert!(state.draft().len() <= MAX_COMMAND_BYTES);
    }
}
