use revm::primitives::keccak256;
use serde_json::{Value, json};
use sevm_revm_headless::{PROTOCOL_VERSION, serve};
use std::{
    io::{Cursor, Read, Write},
    process::{Command, Stdio},
    str::FromStr,
};

const TARGET: &str = "0x1000000000000000000000000000000000000001";
const CALLER: &str = "0x2000000000000000000000000000000000000002";
const CHEATCODE: &str = "7109709ecfa91a80626ff3989d68f67f5b1dd12d";

fn request(id: u64, method: &str, params: Value) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    })
    .to_string()
}

fn run(requests: &[String]) -> Vec<Value> {
    let input = requests.join("\n") + "\n";
    let mut output = Vec::new();
    serve(Cursor::new(input), &mut output).unwrap();
    String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn drives_a_live_session_over_json_rpc() {
    let responses = run(&[
        request(
            1,
            "start",
            json!({
                "entry": TARGET,
                "accounts": [{ "address": TARGET, "code": "0x600160005500" }],
                "breakpoints": [{ "address": TARGET, "pc": 4 }],
            }),
        ),
        request(2, "wait_event", json!({})),
        request(3, "set_stack", json!({ "index": 1, "value": "0x9" })),
        request(4, "step", json!({})),
        request(5, "wait_event", json!({})),
        request(6, "resume", json!({})),
        request(7, "wait_event", json!({})),
        request(8, "shutdown", json!({})),
    ]);

    assert_eq!(responses.len(), 8);
    assert_eq!(responses[0]["result"]["started"], true);
    assert_eq!(responses[1]["result"]["type"], "paused");
    assert_eq!(responses[1]["result"]["snapshot"]["pc"], 4);
    assert_eq!(responses[1]["result"]["snapshot"]["mnemonic"], "SSTORE");
    assert_eq!(responses[1]["result"]["snapshot"]["caller"], CALLER);
    assert_eq!(responses[1]["result"]["snapshot"]["origin"], CALLER);
    assert_eq!(
        responses[1]["result"]["snapshot"]["frames"][0]["kind"],
        "call"
    );
    assert_eq!(
        responses[1]["result"]["snapshot"]["frames"][0]["code"],
        "0x600160005500"
    );
    assert_eq!(
        responses[1]["result"]["snapshot"]["stack"],
        json!(["0x0", "0x1"])
    );
    assert_eq!(responses[2]["result"], "0x9");
    assert_eq!(responses[3]["result"], Value::Null);
    assert_eq!(responses[4]["result"]["snapshot"]["reason"], "step");
    assert_eq!(responses[4]["result"]["snapshot"]["pc"], 5);
    assert_eq!(responses[6]["result"]["type"], "finished");
    assert_eq!(responses[6]["result"]["success"], true);
    assert_eq!(responses[6]["result"]["storage"][0]["key"], "0x0");
    assert_eq!(responses[6]["result"]["storage"][0]["value"], "0x9");
}

#[test]
fn reports_protocol_and_recovers_after_bad_input() {
    let responses = run(&[
        "{not json}".to_owned(),
        request(1, "hello", json!({})),
        request(2, "missing", json!({})),
        request(3, "snapshot", json!({})),
        request(4, "shutdown", json!({})),
    ]);

    assert_eq!(responses[0]["error"]["code"], -32700);
    assert_eq!(responses[0]["id"], Value::Null);
    assert_eq!(responses[1]["result"]["protocol"], PROTOCOL_VERSION);
    assert_eq!(responses[2]["error"]["code"], -32601);
    assert_eq!(responses[3]["error"]["code"], -32001);
    assert_eq!(responses[4]["result"], Value::Null);
}

#[test]
fn binary_serves_the_protocol_on_stdio() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_sevm-revm-headless"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    writeln!(stdin, "{}", request(1, "hello", json!({}))).unwrap();
    writeln!(stdin, "{}", request(2, "shutdown", json!({}))).unwrap();
    drop(stdin);

    let mut output = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut output)
        .unwrap();
    assert!(child.wait().unwrap().success());
    let responses = output
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["result"]["protocol"], PROTOCOL_VERSION);
    assert_eq!(responses[1]["result"], Value::Null);
}

#[test]
fn persistent_protocol_deploys_then_calls_the_created_contract() {
    let caller = revm::primitives::Address::from_str(CALLER).unwrap();
    let created = format!("{:#x}", caller.create(0));
    let init_code = format!("0x6018600a5f3960185ff3{}5f546001015f5500", "5b".repeat(16));
    let responses = run(&[
        request(1, "open", json!({})),
        request(2, "transact", json!({ "data": init_code })),
        request(3, "wait_event", json!({})),
        request(
            4,
            "set_breakpoints",
            json!({ "breakpoints": [{ "address": created.clone(), "pc": 22 }] }),
        ),
        request(5, "transact", json!({ "to": created.clone() })),
        request(6, "wait_event", json!({})),
        request(7, "resume", json!({})),
        request(8, "wait_event", json!({})),
        request(9, "transact", json!({ "to": created.clone() })),
        request(10, "wait_event", json!({})),
        request(11, "resume", json!({})),
        request(12, "wait_event", json!({})),
        request(13, "shutdown", json!({})),
    ]);

    assert_eq!(responses.len(), 13);
    assert_eq!(responses[0]["result"]["opened"], true);
    assert_eq!(responses[2]["result"]["created_address"], created);
    assert_eq!(responses[3]["result"], 1);
    assert_eq!(responses[5]["result"]["snapshot"]["pc"], 22);
    assert_eq!(responses[7]["result"]["storage"][0]["value"], "0x1");
    assert_eq!(responses[9]["result"]["snapshot"]["pc"], 22);
    assert_eq!(responses[11]["result"]["storage"][0]["value"], "0x2");
}

#[test]
fn delegates_foundry_host_calls_over_json_rpc() {
    let selector = keccak256("assertTrue(bool)");
    let selector = selector[..4]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let code = format!(
        "0x7f{selector}{}5f5260016004525f5f60245f5f73{CHEATCODE}61c350f15000",
        "00".repeat(28)
    );
    let responses = run(&[
        request(
            1,
            "start",
            json!({
                "entry": TARGET,
                "accounts": [{ "address": TARGET, "code": code }],
                "breakpoints": [{ "address": TARGET, "pc": 0 }],
            }),
        ),
        request(2, "wait_event", json!({})),
        request(3, "resume", json!({})),
        request(4, "wait_event", json!({})),
        request(
            5,
            "state",
            json!({ "op": "write_balance", "address": TARGET, "value": "0x2a" }),
        ),
        request(
            6,
            "state",
            json!({ "op": "read_balance", "address": TARGET }),
        ),
        request(
            7,
            "state",
            json!({ "op": "write_timestamp", "value": "0x1092" }),
        ),
        request(8, "state", json!({ "op": "read_timestamp" })),
        request(
            9,
            "state",
            json!({
                "op": "write_storage",
                "address": TARGET,
                "key": "0x7",
                "value": "0x8"
            }),
        ),
        request(
            10,
            "set_prank",
            json!({ "caller": TARGET, "new_sender": CALLER }),
        ),
        request(11, "respond_host", json!({})),
        request(12, "wait_event", json!({})),
        request(13, "shutdown", json!({})),
    ]);

    assert_eq!(responses[3]["result"]["type"], "host_call");
    assert_eq!(responses[3]["result"]["address"], format!("0x{CHEATCODE}"));
    assert_eq!(responses[3]["result"]["caller"], TARGET);
    assert_eq!(responses[4]["result"], "0x2a");
    assert_eq!(responses[5]["result"], "0x2a");
    assert_eq!(responses[6]["result"], "0x1092");
    assert_eq!(responses[7]["result"], "0x1092");
    assert_eq!(responses[8]["result"], "0x8");
    assert_eq!(responses[9]["result"], Value::Null);
    assert_eq!(responses[11]["result"]["type"], "finished");
    assert_eq!(responses[11]["result"]["success"], true);
    assert_eq!(responses[11]["result"]["storage"][0]["key"], "0x7");
    assert_eq!(responses[11]["result"]["storage"][0]["value"], "0x8");
}

#[test]
fn evaluates_with_frame_inputs_over_json_rpc() {
    let responses = run(&[
        request(
            1,
            "start",
            json!({
                "entry": TARGET,
                "accounts": [{ "address": TARGET, "code": "0x00" }],
                "breakpoints": [{ "address": TARGET, "pc": 0 }],
            }),
        ),
        request(2, "wait_event", json!({})),
        request(
            3,
            "evaluate_call",
            json!({
                "bytecode": "0x365f5260205ff3",
                "data": "0x01020304",
                "caller": CALLER,
                "value": "0x2a"
            }),
        ),
        request(4, "resume", json!({})),
        request(5, "wait_event", json!({})),
        request(6, "shutdown", json!({})),
    ]);

    assert_eq!(responses[2]["result"]["success"], true);
    assert_eq!(
        responses[2]["result"]["output"],
        format!("0x{}04", "00".repeat(31))
    );
    assert!(responses[2]["result"]["gas_used"].as_u64().unwrap() > 0);
    assert_eq!(responses[4]["result"]["type"], "finished");
}

#[test]
fn supports_address_independent_breakpoints() {
    let responses = run(&[
        request(
            1,
            "start",
            json!({
                "entry": TARGET,
                "accounts": [{ "address": TARGET, "code": "0x600100" }],
                "breakpoints": [{ "pc": 0 }],
            }),
        ),
        request(2, "wait_event", json!({})),
        request(3, "resume", json!({})),
        request(4, "wait_event", json!({})),
        request(5, "shutdown", json!({})),
    ]);

    assert_eq!(responses[1]["result"]["type"], "paused");
    assert_eq!(responses[1]["result"]["snapshot"]["pc"], 0);
    assert_eq!(responses[3]["result"]["type"], "finished");
}
