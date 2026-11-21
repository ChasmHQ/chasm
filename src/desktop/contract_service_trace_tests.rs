use super::*;
use axum::{
    body::Body,
    extract::State,
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use futures::{stream, StreamExt};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use tokio::{sync::watch, task::JoinHandle};

const FROM: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TARGET: &str = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const OTHER: &str = "0xcccccccccccccccccccccccccccccccccccccccc";

fn hash(byte: &str) -> String {
    format!("0x{}", byte.repeat(32))
}

fn source() -> Value {
    json!({"chainId":"0x7a69", "genesisHash":hash("11"),
        "block":{"number":"0x10", "hash":hash("22")}})
}

fn calldata() -> String {
    format!("0x70a08231{:0>64}", &FROM[2..])
}

fn word(value: u8) -> String {
    format!("0x{value:064x}")
}

fn tree() -> Value {
    json!({"type":"CALL", "from":FROM, "to":TARGET, "input":calldata(),
        "gas":"0x186a0", "gasUsed":"0x100", "value":"0x123", "output":word(42),
        "calls":[{"type":"STATICCALL", "from":TARGET, "to":OTHER, "input":"0x"}]})
}

fn receipt(block_hash: String) -> Value {
    json!({"transactionHash":hash("33"), "blockNumber":"0x10", "blockHash":block_hash,
        "status":"0x1"})
}

fn read_result() -> CallResult {
    let abi: Abi = serde_json::from_value(json!([{
        "type":"function", "name":"balanceOf", "stateMutability":"view",
        "inputs":[{"name":"owner", "type":"address"}],
        "outputs":[{"name":"balance", "type":"uint256"}]
    }]))
    .unwrap();
    let call = json!({"from":FROM, "to":TARGET, "data":calldata(),
        "value":"0x123", "gas":"0x186a0"});
    let context = ResultContext {
        contract: "Fixture".into(),
        address: TARGET.into(),
        signature: "balanceOf(address)".into(),
        request: json!({"method":"eth_call", "params":[call,"0x10"]}),
        workspace_epoch: 1,
        network_epoch: 2,
        source: source(),
    };
    CallResult::from_call(
        context,
        abi.function("balanceOf").unwrap(),
        &abi,
        json!({"jsonrpc":"2.0", "id":1, "result":word(42)}),
    )
}

fn transaction_result() -> CallResult {
    let mut result = read_result();
    result.context.request = json!({"method":"eth_sendRawTransaction",
        "params":["[signed payload omitted]"], "transactionHash":hash("33"),
        "submissionState":"confirmed"});
    result.raw = json!({"jsonrpc":"2.0", "id":1, "result":receipt(hash("22"))});
    result
}

#[derive(Clone, Copy)]
enum Behavior {
    Success,
    Oversized,
    Unsupported,
    ChangedHeader,
    ChangedReceipt,
    Timeout,
}

struct Fixture {
    behavior: Behavior,
    requests: Mutex<Vec<Value>>,
    traces: AtomicUsize,
    stop: watch::Sender<bool>,
}

async fn wait_for_stop(fixture: &Fixture) {
    let mut stop = fixture.stop.subscribe();
    let stopped = *stop.borrow();
    if !stopped {
        tokio::select! {
            _ = stop.changed() => {},
            _ = tokio::time::sleep(Duration::from_secs(30)) => {},
        }
    }
}

async fn handle(State(fixture): State<Arc<Fixture>>, Json(request): Json<Value>) -> Response {
    {
        let mut requests = fixture.requests.lock().unwrap();
        if requests.len() >= 32 {
            return Json(json!({"jsonrpc":"2.0", "id":1,
                "error":{"code":-32000, "message":"fixture request limit"}}))
            .into_response();
        }
        requests.push(request.clone());
    }
    let traced = fixture.traces.load(Ordering::SeqCst) > 0;
    let result = match request["method"].as_str().unwrap_or_default() {
        "eth_chainId" => json!("0x7a69"),
        "eth_getBlockByNumber" if request["params"] == json!(["0x0", false]) => {
            json!({"number":"0x0", "hash":hash("11")})
        }
        "eth_getBlockByNumber" if request["params"] == json!(["0x10", false]) => {
            let changed = matches!(fixture.behavior, Behavior::ChangedHeader) && traced;
            json!({"number":"0x10", "hash":hash(if changed {"44"} else {"22"})})
        }
        "eth_getTransactionReceipt" => {
            let changed = matches!(fixture.behavior, Behavior::ChangedReceipt) && traced;
            receipt(hash(if changed { "44" } else { "22" }))
        }
        "debug_traceCall" | "debug_traceTransaction" => {
            fixture.traces.fetch_add(1, Ordering::SeqCst);
            match fixture.behavior {
                Behavior::Unsupported => {
                    return Json(json!({"jsonrpc":"2.0", "id":1,
                    "error":{"code":-32601, "message":"synthetic unsupported"}}))
                    .into_response()
                }
                Behavior::Timeout => {
                    wait_for_stop(&fixture).await;
                    tree()
                }
                Behavior::Oversized => {
                    let chunks = std::iter::once(Ok::<_, std::io::Error>(
                        b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"data\":\"".to_vec(),
                    ))
                    .chain((0..65).map(|_| Ok(vec![b'x'; 16 * 1024])));
                    let end = fixture.clone();
                    let body = stream::iter(chunks).chain(stream::once(async move {
                        wait_for_stop(&end).await;
                        Ok::<_, std::io::Error>(b"\"}}".to_vec())
                    }));
                    return Response::new(Body::from_stream(body));
                }
                _ => tree(),
            }
        }
        _ => {
            return Json(json!({"jsonrpc":"2.0", "id":1,
            "error":{"code":-32601, "message":"unexpected fixture request"}}))
            .into_response()
        }
    };
    Json(json!({"jsonrpc":"2.0", "id":1, "result":result})).into_response()
}

struct MockServer {
    url: String,
    fixture: Arc<Fixture>,
    task: JoinHandle<()>,
}

impl MockServer {
    async fn start(behavior: Behavior) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (stop, _) = watch::channel(false);
        let fixture = Arc::new(Fixture {
            behavior,
            requests: Mutex::new(Vec::new()),
            traces: AtomicUsize::new(0),
            stop,
        });
        let router = Router::new()
            .route("/", post(handle))
            .with_state(fixture.clone());
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self { url, fixture, task }
    }

    fn assert_requests(&self, expected: Vec<Value>) {
        assert_eq!(*self.fixture.requests.lock().unwrap(), expected);
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        let _ = self.fixture.stop.send(true);
        self.task.abort();
    }
}

fn request(method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0", "id":1, "method":method, "params":params})
}

fn identity_requests() -> Vec<Value> {
    // The anchor probe comes first: an Anvil fork pins its fork block, any
    // other node pins block 0. The fixture answers neither as Anvil, so the
    // probe fails and block 0 is read.
    vec![
        request("eth_chainId", json!([])),
        request("anvil_nodeInfo", json!([])),
        request("eth_getBlockByNumber", json!(["0x0", false])),
        request("eth_getBlockByNumber", json!(["0x10", false])),
    ]
}

fn options() -> Value {
    json!({"tracer":"callTracer", "timeout":"10s", "tracerConfig":{"withLog":true}})
}

fn call_request(result: &CallResult) -> Value {
    request(
        "debug_traceCall",
        json!([result.context.request["params"][0], "0x10", options()]),
    )
}

fn transaction_requests() -> Vec<Value> {
    let mut requests = identity_requests();
    requests.push(request("eth_getTransactionReceipt", json!([hash("33")])));
    requests.push(request(
        "debug_traceTransaction",
        json!([hash("33"), options()]),
    ));
    requests.extend(identity_requests());
    requests.push(request("eth_getTransactionReceipt", json!([hash("33")])));
    requests
}

async fn bounded_trace(server: &MockServer, result: &CallResult) -> Result<String, String> {
    tokio::time::timeout(Duration::from_secs(5), trace(&server.url, result))
        .await
        .expect("trace did not finish within the fixture deadline")
}

#[tokio::test]
async fn read_trace_uses_the_captured_call_and_pinned_block() {
    let server = MockServer::start(Behavior::Success).await;
    let result = read_result();
    let before = result.clone();
    let mut edited_ui_call = result.context.request["params"][0].clone();
    edited_ui_call["from"] = json!(OTHER);
    edited_ui_call["value"] = json!("0x1");
    edited_ui_call["gas"] = json!("0x2");
    assert_ne!(edited_ui_call, result.context.request["params"][0]);
    let text = bounded_trace(&server, &result).await.unwrap();
    assert!(text.contains("Read-only local call trace at block"));
    assert!(text.contains("function: balanceOf(address)"));
    assert!(text.contains("balance: 42"));
    assert!(text.contains("STATICCALL"));
    assert_eq!(format!("{result:?}"), format!("{before:?}"));
    let mut expected = identity_requests();
    expected.push(call_request(&before));
    expected.extend(identity_requests());
    server.assert_requests(expected);
}

#[tokio::test]
async fn trace_response_over_one_mib_is_rejected_while_streaming() {
    let server = MockServer::start(Behavior::Oversized).await;
    let result = read_result();
    let error = bounded_trace(&server, &result).await.unwrap_err();
    assert!(error.contains("Trace response exceeds 1 MiB"), "{error}");
    let mut expected = identity_requests();
    expected.push(call_request(&result));
    server.assert_requests(expected);
}

#[tokio::test]
async fn unsupported_call_tracer_does_not_fall_back() {
    let server = MockServer::start(Behavior::Unsupported).await;
    let result = read_result();
    let error = bounded_trace(&server, &result).await.unwrap_err();
    assert!(error.contains("does not support callTracer"), "{error}");
    let mut expected = identity_requests();
    expected.push(call_request(&result));
    server.assert_requests(expected);
}

#[tokio::test]
async fn changed_header_after_trace_discards_result() {
    let server = MockServer::start(Behavior::ChangedHeader).await;
    let result = read_result();
    let error = bounded_trace(&server, &result).await.unwrap_err();
    assert!(error.contains("changed"), "{error}");
    let mut expected = identity_requests();
    expected.push(call_request(&result));
    expected.extend(identity_requests());
    server.assert_requests(expected);
}

#[tokio::test]
async fn transaction_trace_checks_original_and_current_receipts() {
    let result = transaction_result();
    let before = result.clone();
    let server = MockServer::start(Behavior::Success).await;
    let text = bounded_trace(&server, &result).await.unwrap();
    assert!(text.contains("Read-only local transaction trace at block"));
    assert!(text.contains("balanceOf(address)"));
    server.assert_requests(transaction_requests());
    assert_eq!(format!("{result:?}"), format!("{before:?}"));

    let changed = MockServer::start(Behavior::ChangedReceipt).await;
    let error = bounded_trace(&changed, &result).await.unwrap_err();
    assert!(error.contains("receipt no longer matches"), "{error}");
    changed.assert_requests(transaction_requests());

    let invalid = MockServer::start(Behavior::Success).await;
    let mut wrong_original = result.clone();
    wrong_original.raw["result"]["transactionHash"] = json!(hash("44"));
    let error = bounded_trace(&invalid, &wrong_original).await.unwrap_err();
    assert!(error.contains("original receipt"), "{error}");
    invalid.assert_requests(identity_requests());
}

#[tokio::test]
async fn stalled_trace_request_times_out_without_a_fallback() {
    let server = MockServer::start(Behavior::Timeout).await;
    let result = read_result();
    let error = tokio::time::timeout(Duration::from_secs(15), trace(&server.url, &result))
        .await
        .expect("production RPC timeout did not fire")
        .unwrap_err();
    assert!(error.contains("timed out"), "{error}");
    let mut expected = identity_requests();
    expected.push(call_request(&result));
    server.assert_requests(expected);
}
