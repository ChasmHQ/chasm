use serde_json::{Value, json};
use sevm_revm_headless::{PROTOCOL_VERSION, serve};
use std::{
    io::{Cursor, Read, Write},
    process::{Command, Stdio},
};

const TARGET: &str = "0x1000000000000000000000000000000000000001";

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
        request(4, "resume", json!({})),
        request(5, "wait_event", json!({})),
        request(6, "shutdown", json!({})),
    ]);

    assert_eq!(responses.len(), 6);
    assert_eq!(responses[0]["result"]["started"], true);
    assert_eq!(responses[1]["result"]["type"], "paused");
    assert_eq!(responses[1]["result"]["snapshot"]["pc"], 4);
    assert_eq!(
        responses[1]["result"]["snapshot"]["stack"],
        json!(["0x0", "0x1"])
    );
    assert_eq!(responses[2]["result"], "0x9");
    assert_eq!(responses[3]["result"], Value::Null);
    assert_eq!(responses[4]["result"]["type"], "finished");
    assert_eq!(responses[4]["result"]["success"], true);
    assert_eq!(responses[4]["result"]["storage"][0]["key"], "0x0");
    assert_eq!(responses[4]["result"]["storage"][0]["value"], "0x9");
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
