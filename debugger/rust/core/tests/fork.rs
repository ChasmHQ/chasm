use chasm_revm_core::{
    Breakpoint, CommandValue, DebugEngine, DebugEvent, ForkConfig, RemoteDb, StateCommand,
    transaction_from_json,
};
use revm::{
    database::CacheDB,
    database_interface::{Database, DatabaseRef},
    primitives::{Address, Bytes, U256},
};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

const CALLER: &str = "0x2000000000000000000000000000000000000002";
const TARGET: &str = "0x1000000000000000000000000000000000000001";
const COINBASE: &str = "0x3000000000000000000000000000000000000003";
const BEACON: &str = "0x000f3df6d732807ef1319fb7b8bb8522d0beac02";
fn addr(value: &str) -> Address {
    value.parse().unwrap()
}
fn h(byte: &str) -> String {
    format!("0x{}", byte.repeat(32))
}
fn config() -> Value {
    json!({"rpc_url":null,"anchor":{"chain_id":1,"number":100,"hash":h("aa"),"genesis_hash":"0xd4e56740f876aef8c010b86a40d5f56745a118d0906a34e69aec8c0db1cb8fa3"},"block":{"number":101,"timestamp":1710338135u64,"beneficiary":COINBASE,"gas_limit":30_000_000,"base_fee":"0x1","prevrandao":h("bb"),"excess_blob_gas":0},"strict":true,"witness":null})
}
fn empty_account() -> Value {
    json!({"balance":"0x0","nonce":0,"code":"0x"})
}
fn offline(code: &str) -> Value {
    let mut value = config();
    value["witness"] = json!({"version":1,"anchor":value["anchor"],"accounts":{CALLER:{"balance":"0xffffffffffffffff","nonce":0,"code":"0x"}, TARGET:{"balance":"0x0","nonce":0,"code":code}, COINBASE:empty_account()},"storage":{TARGET:{"0x0":"0x7","0x1":"0x9"}},"block_hashes":{},"headers":{}});
    value
}
fn parse(value: Value) -> ForkConfig {
    ForkConfig::from_json(&value.to_string()).unwrap()
}
fn transaction() -> Value {
    json!({"caller":CALLER,"to":TARGET,"data":"0x","value":"0x0","gas_limit":100_000,"nonce":0,"chain_id":1,"tx_type":0,"gas_price":"0x2","max_priority_fee_per_gas":null,"access_list":[],"blob_hashes":[],"max_fee_per_blob_gas":null,"commit":true})
}
fn finish(engine: &DebugEngine) -> chasm_revm_core::Finished {
    loop {
        match engine.wait(Duration::from_secs(15)).unwrap() {
            DebugEvent::Finished(value) => return value,
            DebugEvent::Paused(_) => engine.resume().unwrap(),
            event => panic!("unexpected {event:?}"),
        }
    }
}
struct Mock {
    endpoint: String,
    requests: Arc<Mutex<Vec<Value>>>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Mock {
    fn new(responses: Vec<Value>) -> Self {
        Self::delayed(responses, Duration::ZERO)
    }
    fn delayed(responses: Vec<Value>, delay: Duration) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/secret-token", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        listener.set_nonblocking(true).unwrap();
        let worker = thread::spawn(move || {
            for response in responses {
                let start = std::time::Instant::now();
                let (mut stream, _) = loop {
                    match listener.accept() {
                        Ok(stream) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(
                                start.elapsed() < Duration::from_secs(10),
                                "mock request not received"
                            );
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("{error}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if line.to_ascii_lowercase().starts_with("content-length:") {
                        length = line
                            .split_once(':')
                            .unwrap()
                            .1
                            .trim()
                            .parse::<usize>()
                            .unwrap();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let request: Value = serde_json::from_slice(&body).unwrap();
                let body = if response.get("jsonrpc").is_some() {
                    response
                } else {
                    json!({"jsonrpc":"2.0","id":request["id"],"result":response})
                }
                .to_string();
                recorded.lock().unwrap().push(request);
                thread::sleep(delay);
                let result = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                if delay.is_zero() {
                    result.unwrap();
                }
            }
        });
        Self {
            endpoint,
            requests,
            worker: Some(worker),
        }
    }
}
impl Drop for Mock {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

#[test]
fn pinned_rpc_is_read_only_cached_and_exported_without_network() {
    let mock = Mock::new(vec![
        json!("0x10"),
        json!("0x2"),
        json!("0x6000"),
        json!(h("00")),
    ]);
    let mut value = config();
    value["rpc_url"] = json!(mock.endpoint);
    let db = RemoteDb::new(&parse(value)).unwrap();
    let info = db.basic_ref(addr(TARGET)).unwrap().unwrap();
    assert_eq!(info.balance, U256::from(16));
    assert_eq!(info.nonce, 2);
    assert_eq!(
        db.storage_ref(addr(TARGET), U256::ZERO).unwrap(),
        U256::ZERO
    );
    db.basic_ref(addr(TARGET)).unwrap();
    db.storage_ref(addr(TARGET), U256::ZERO).unwrap();
    let requests = mock.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    for request in requests.iter() {
        let selector = request["params"].as_array().unwrap().last().unwrap();
        assert_eq!(
            selector,
            &json!({"blockHash":h("aa"),"requireCanonical":true})
        );
        assert!(
            [
                "eth_getBalance",
                "eth_getTransactionCount",
                "eth_getCode",
                "eth_getStorageAt"
            ]
            .contains(&request["method"].as_str().unwrap())
        );
    }
    let exported = db.export().unwrap();
    assert!(!exported.contains("secret-token"));
    assert!(!exported.contains("http"));
    drop(requests);
    let mut value = config();
    value["witness"] = serde_json::from_str(&exported).unwrap();
    let offline = RemoteDb::new(&parse(value)).unwrap();
    assert_eq!(offline.basic_ref(addr(TARGET)).unwrap().unwrap().nonce, 2);
    assert_eq!(
        offline.storage_ref(addr(TARGET), U256::ZERO).unwrap(),
        U256::ZERO
    );
    assert!(
        offline
            .storage_ref(addr(TARGET), U256::ONE)
            .unwrap_err()
            .to_string()
            .contains("missing offline witness")
    );
    assert!(offline.basic_ref(addr(CALLER)).is_err());
}

#[test]
fn overlay_zero_never_falls_through_and_untouched_slot_uses_base() {
    let remote = RemoteDb::new(&parse(offline("0x00"))).unwrap();
    let mut db = CacheDB::new(remote.clone());
    db.insert_account_storage(addr(TARGET), U256::ZERO, U256::ZERO)
        .unwrap();
    assert_eq!(db.storage(addr(TARGET), U256::ZERO).unwrap(), U256::ZERO);
    assert_eq!(db.storage(addr(TARGET), U256::ONE).unwrap(), U256::from(9));
    let mut clone = db.clone();
    clone
        .insert_account_storage(addr(TARGET), U256::ONE, U256::from(99))
        .unwrap();
    assert_eq!(db.storage(addr(TARGET), U256::ONE).unwrap(), U256::from(9));
    let witness: Value = serde_json::from_str(&remote.export().unwrap()).unwrap();
    assert_eq!(witness["storage"][TARGET]["0x0"], "0x7");
}

#[test]
fn provider_failures_null_wrong_id_and_types_are_not_zero_or_secret_leaks() {
    for response in [
        json!({"jsonrpc":"2.0","id":1,"error":{"message":"secret-token"}}),
        json!({"jsonrpc":"2.0","id":7,"result":"0x0"}),
        json!(false),
        json!({"jsonrpc":"2.0","id":1,"result":null}),
        json!("0x00"),
    ] {
        let mock = Mock::new(vec![response]);
        let mut value = config();
        value["rpc_url"] = json!(mock.endpoint);
        let db = RemoteDb::new(&parse(value)).unwrap();
        let message = db.basic_ref(addr(TARGET)).unwrap_err().to_string();
        assert!(!message.contains("secret-token"));
        assert!(!message.contains("http"));
        let exported: Value = serde_json::from_str(&db.export().unwrap()).unwrap();
        assert!(exported["accounts"].as_object().unwrap().is_empty());
    }
}

#[test]
fn ancestry_walk_is_hash_pinned_and_witness_checks_links() {
    let mock = Mock::new(vec![
        json!({"hash":h("aa"),"number":"0x64","parentHash":h("99")}),
        json!({"hash":h("99"),"number":"0x63","parentHash":h("98")}),
    ]);
    let mut value = config();
    value["rpc_url"] = json!(mock.endpoint);
    let db = RemoteDb::new(&parse(value)).unwrap();
    assert_eq!(format!("{:#x}", db.block_hash_ref(98).unwrap()), h("98"));
    assert!(db.block_hash_ref(101).is_err());
    assert_eq!(
        mock.requests.lock().unwrap()[0]["method"],
        "eth_getBlockByHash"
    );
    let mut offline = config();
    offline["witness"] = serde_json::from_str(&db.export().unwrap()).unwrap();
    let imported = RemoteDb::new(&parse(offline.clone())).unwrap();
    assert_eq!(
        db.block_hash_ref(98).unwrap(),
        imported.block_hash_ref(98).unwrap()
    );
    offline["witness"]["headers"][h("aa")]["parent_hash"] = json!(h("ff"));
    assert!(ForkConfig::from_json(&offline.to_string()).is_err());
}

#[test]
fn strict_transactions_keep_nonce_fees_and_discard_isolation() {
    let engine = DebugEngine::fork(parse(offline("0x5f5f5500"))).unwrap();
    let mut tx = transaction();
    tx["commit"] = json!(false);
    engine.transact_json(&tx.to_string()).unwrap();
    assert!(finish(&engine).success);
    assert_eq!(
        engine.state(StateCommand::ReadNonce(addr(CALLER))).unwrap(),
        CommandValue::Number(0)
    );
    assert_eq!(
        engine
            .state(StateCommand::ReadStorage {
                address: addr(TARGET),
                key: U256::ZERO
            })
            .unwrap(),
        CommandValue::Word(U256::from(7))
    );
    tx["commit"] = json!(true);
    engine.transact_json(&tx.to_string()).unwrap();
    let result = finish(&engine);
    assert!(result.success);
    assert_eq!(
        engine.state(StateCommand::ReadNonce(addr(CALLER))).unwrap(),
        CommandValue::Number(1)
    );
    assert_eq!(
        engine
            .state(StateCommand::ReadStorage {
                address: addr(TARGET),
                key: U256::ZERO
            })
            .unwrap(),
        CommandValue::Word(U256::ZERO)
    );
    let witness: Value = serde_json::from_str(&engine.export_fork().unwrap()).unwrap();
    assert_eq!(witness["accounts"][CALLER]["nonce"], 0);
    assert_eq!(witness["storage"][TARGET]["0x0"], "0x7");
    engine.transact_json(&tx.to_string()).unwrap();
    assert!(matches!(
        engine.wait(Duration::from_secs(15)).unwrap(),
        DebugEvent::Failed(_)
    ));
}

#[test]
fn inspection_and_discarded_evaluation_do_not_warm_replay_slots() {
    let engine = DebugEngine::fork(parse(offline("0x5b60005400"))).unwrap();
    engine
        .set_breakpoints(vec![Breakpoint {
            address: addr(TARGET),
            pc: 0,
        }])
        .unwrap();
    engine.transact_json(&transaction().to_string()).unwrap();
    assert!(matches!(
        engine.wait(Duration::from_secs(15)).unwrap(),
        DebugEvent::Paused(_)
    ));
    assert_eq!(engine.read_storage(U256::ZERO).unwrap(), U256::from(7));
    engine
        .state(StateCommand::ReadStorage {
            address: addr(TARGET),
            key: U256::ZERO,
        })
        .unwrap();
    engine
        .evaluate(Bytes::from_static(&[0x5f, 0x54, 0x00]), false)
        .unwrap();
    assert_eq!(
        engine
            .state(StateCommand::IsStorageWarm {
                address: addr(TARGET),
                key: U256::ZERO
            })
            .unwrap(),
        CommandValue::Bool(false)
    );
    let mut nested_code = vec![0x5f, 0x5f, 0x5f, 0x5f, 0x5f, 0x73];
    nested_code.extend_from_slice(addr(TARGET).as_slice());
    nested_code.extend_from_slice(&[0x61, 0xff, 0xff, 0xf1, 0x00]);
    engine.evaluate(nested_code.into(), false).unwrap();
    assert_eq!(
        engine
            .state(StateCommand::IsStorageWarm {
                address: addr(TARGET),
                key: U256::ZERO
            })
            .unwrap(),
        CommandValue::Bool(false)
    );
    engine.resume().unwrap();
    let inspected = finish(&engine);
    let baseline = DebugEngine::fork(parse(offline("0x5b60005400"))).unwrap();
    baseline.transact_json(&transaction().to_string()).unwrap();
    assert_eq!(inspected.gas_used, finish(&baseline).gas_used);
}

#[test]
fn system_call_is_noninteractive_and_does_not_pay_or_increment_nonce() {
    let mut value = offline("0x00");
    // Store caller, calldata root, and GAS: the call must retain contract execution semantics.
    value["witness"]["accounts"][BEACON] =
        json!({"balance":"0x0","nonce":1,"code":"0x335f555f356001555a60025500"});
    value["witness"]["storage"][BEACON] = json!({"0x0":"0x0","0x1":"0x0","0x2":"0x0"});
    let engine = DebugEngine::fork(parse(value)).unwrap();
    engine
        .set_breakpoints(vec![Breakpoint {
            address: addr(BEACON),
            pc: 0,
        }])
        .unwrap();
    engine
        .apply_beacon_root(Bytes::from(vec![0x42; 32]))
        .unwrap();
    assert_eq!(
        engine.state(StateCommand::ReadNonce(addr(CALLER))).unwrap(),
        CommandValue::Number(0)
    );
    assert_eq!(
        engine
            .state(StateCommand::ReadBalance(addr(COINBASE)))
            .unwrap(),
        CommandValue::Word(U256::ZERO)
    );
    let caller = engine
        .state(StateCommand::ReadStorage {
            address: addr(BEACON),
            key: U256::ZERO,
        })
        .unwrap();
    assert_eq!(
        caller,
        CommandValue::Word(U256::from_be_slice(
            addr("0xfffffffffffffffffffffffffffffffffffffffe").as_slice()
        ))
    );
    assert_eq!(
        engine
            .state(StateCommand::ReadStorage {
                address: addr(BEACON),
                key: U256::ONE
            })
            .unwrap(),
        CommandValue::Word(U256::from_be_slice(&[0x42; 32]))
    );
    match engine
        .state(StateCommand::ReadStorage {
            address: addr(BEACON),
            key: U256::from(2),
        })
        .unwrap()
    {
        CommandValue::Word(gas) => assert!(gas < U256::from(30_000_000)),
        other => panic!("{other:?}"),
    }
}

#[test]
fn eip4788_runtime_updates_ring_buffer_and_serves_timestamp_lookup() {
    let mut value = offline("0x00");
    value["witness"]["accounts"][BEACON] = json!({"balance":"0x0","nonce":1,"code":"0x3373fffffffffffffffffffffffffffffffffffffffe14604d57602036146024575f5ffd5b5f35801560495762001fff810690815414603c575f5ffd5b62001fff01545f5260205ff35b5f5ffd5b62001fff42064281555f359062001fff015500"});
    let timestamp = value["block"]["timestamp"].as_u64().unwrap();
    let index = timestamp % 8191;
    value["witness"]["storage"][BEACON] =
        json!({format!("0x{index:x}"):"0x0",format!("0x{:x}",index+8191):"0x0"});
    let engine = DebugEngine::fork(parse(value)).unwrap();
    engine
        .apply_beacon_root(Bytes::from(vec![0x42; 32]))
        .unwrap();
    assert_eq!(
        engine
            .state(StateCommand::ReadStorage {
                address: addr(BEACON),
                key: U256::from(index)
            })
            .unwrap(),
        CommandValue::Word(U256::from(timestamp))
    );
    let mut tx = transaction();
    tx["to"] = json!(BEACON);
    tx["data"] = json!(format!("0x{timestamp:064x}"));
    engine.transact_json(&tx.to_string()).unwrap();
    let result = finish(&engine);
    assert!(result.success);
    assert_eq!(result.output.as_ref(), &[0x42; 32]);
}

#[test]
fn strict_oog_and_reverts_are_terminal_and_commit_nonce_fees() {
    for code in ["0x5b600056", "0x5f5ffd"] {
        let engine = DebugEngine::fork(parse(offline(code))).unwrap();
        let mut tx = transaction();
        tx["gas_limit"] = json!(30_000);
        engine.transact_json(&tx.to_string()).unwrap();
        let result = finish(&engine);
        assert!(!result.success);
        if code == "0x5b600056" {
            assert_eq!(result.gas_used, 30_000);
        }
        assert_eq!(
            engine.state(StateCommand::ReadNonce(addr(CALLER))).unwrap(),
            CommandValue::Number(1)
        );
        assert_eq!(
            engine
                .state(StateCommand::ReadBalance(addr(COINBASE)))
                .unwrap(),
            CommandValue::Word(U256::from(result.gas_used))
        );
        tx["nonce"] = json!(1);
        engine.transact_json(&tx.to_string()).unwrap();
        assert!(!finish(&engine).success);
        assert_eq!(
            engine.state(StateCommand::ReadNonce(addr(CALLER))).unwrap(),
            CommandValue::Number(2)
        );
    }
}

#[test]
fn native_configuration_enforces_mainnet_cancun_boundaries() {
    for timestamp in [1_710_338_134u64, 1_746_612_311] {
        let mut bad = config();
        bad["block"]["timestamp"] = json!(timestamp);
        assert!(ForkConfig::from_json(&bad.to_string()).is_err());
    }
    let mut bad = config();
    bad["anchor"]["genesis_hash"] = json!(h("00"));
    assert!(ForkConfig::from_json(&bad.to_string()).is_err());
    let mut bad = config();
    bad["block"]["number"] = json!(102);
    assert!(ForkConfig::from_json(&bad.to_string()).is_err());
}

fn eval_call(target: &str, opcode: u8) -> Bytes {
    let mut code = vec![0x60, 0x20, 0x5f, 0x5f, 0x5f];
    if opcode == 0xf1 {
        code.push(0x5f);
    }
    code.push(0x73);
    code.extend_from_slice(addr(target).as_slice());
    code.extend_from_slice(&[0x62, 0x01, 0x86, 0xa0, opcode, 0x50, 0x60, 0x20, 0x5f, 0xf3]);
    code.into()
}

#[test]
fn nested_call_delegate_static_revert_errors_and_keep_are_isolated() {
    let helper = "0x4000000000000000000000000000000000000004";
    let mut value = offline("0x5b00");
    value["witness"]["accounts"][helper] =
        json!({"balance":"0x0","nonce":0,"code":"0x602a5f55335f5260205ff3"});
    value["witness"]["storage"][helper] = json!({"0x0":"0x0"});
    let engine = DebugEngine::fork(parse(value)).unwrap();
    engine
        .set_breakpoints(vec![Breakpoint {
            address: addr(TARGET),
            pc: 0,
        }])
        .unwrap();
    engine.transact_json(&transaction().to_string()).unwrap();
    assert!(matches!(
        engine.wait(Duration::from_secs(15)).unwrap(),
        DebugEvent::Paused(_)
    ));
    let snapshot = engine.snapshot().unwrap();
    let called = engine
        .evaluate_call(
            eval_call(helper, 0xf1),
            Bytes::new(),
            addr(CALLER),
            U256::ZERO,
            200_000,
            false,
        )
        .unwrap();
    assert!(called.success);
    assert_eq!(
        U256::from_be_slice(&called.output),
        U256::from_be_slice(addr(TARGET).as_slice())
    );
    assert_eq!(
        engine
            .state(StateCommand::ReadStorage {
                address: addr(helper),
                key: U256::ZERO
            })
            .unwrap(),
        CommandValue::Word(U256::ZERO)
    );
    let delegated = engine
        .evaluate_call(
            eval_call(helper, 0xf4),
            Bytes::new(),
            addr(CALLER),
            U256::ZERO,
            200_000,
            true,
        )
        .unwrap();
    assert!(delegated.success);
    assert_eq!(
        U256::from_be_slice(&delegated.output),
        U256::from_be_slice(addr(CALLER).as_slice())
    );
    assert_eq!(engine.read_storage(U256::ZERO).unwrap(), U256::from(42));
    assert_eq!(
        engine
            .state(StateCommand::ReadStorage {
                address: addr(helper),
                key: U256::ZERO
            })
            .unwrap(),
        CommandValue::Word(U256::ZERO)
    );
    let statically = engine
        .evaluate_call(
            eval_call(helper, 0xfa),
            Bytes::new(),
            addr(CALLER),
            U256::ZERO,
            200_000,
            true,
        )
        .unwrap();
    assert!(statically.success);
    assert_eq!(statically.output.as_ref(), &[0; 32]);
    let mut reverting = eval_call(helper, 0xf1).to_vec();
    reverting.truncate(reverting.len() - 4);
    reverting.extend_from_slice(&[0x5f, 0x5f, 0xfd]);
    let reverted = engine
        .evaluate_call(
            reverting.into(),
            Bytes::new(),
            addr(CALLER),
            U256::ZERO,
            200_000,
            true,
        )
        .unwrap();
    assert!(!reverted.success);
    assert_eq!(
        engine
            .state(StateCommand::ReadStorage {
                address: addr(helper),
                key: U256::ZERO
            })
            .unwrap(),
        CommandValue::Word(U256::ZERO)
    );
    for target in [
        "0x5000000000000000000000000000000000000005",
        "0x7109709ecfa91a80626ff3989d68f67f5b1dd12d",
    ] {
        assert!(
            engine
                .evaluate_call(
                    eval_call(target, 0xf1),
                    Bytes::new(),
                    addr(CALLER),
                    U256::ZERO,
                    200_000,
                    false
                )
                .is_err()
        );
        assert_eq!(engine.snapshot().unwrap(), snapshot);
    }
    assert!(
        engine
            .evaluate_call(
                Bytes::from_static(&[0x60, 0x02, 0x54, 0]),
                Bytes::new(),
                addr(CALLER),
                U256::ZERO,
                100_000,
                false
            )
            .unwrap_err()
            .to_string()
            .contains("missing offline witness")
    );
    engine.resume().unwrap();
    assert!(finish(&engine).success);
}

#[test]
fn nested_create_discards_then_keeps_code_storage_and_creator_nonce() {
    let created = addr(TARGET).create(0);
    let created_key = format!("{created:#x}");
    let mut value = offline("0x5b00");
    value["witness"]["accounts"][&created_key] = empty_account();
    value["witness"]["storage"][&created_key] = json!({"0x0":"0x0"});
    let engine = DebugEngine::fork(parse(value)).unwrap();
    engine
        .set_breakpoints(vec![Breakpoint {
            address: addr(TARGET),
            pc: 0,
        }])
        .unwrap();
    engine.transact_json(&transaction().to_string()).unwrap();
    assert!(matches!(
        engine.wait(Duration::from_secs(15)).unwrap(),
        DebugEvent::Paused(_)
    ));
    let before = engine.snapshot().unwrap();
    let runtime = Bytes::from_static(&[0x5f, 0x54, 0x5f, 0x52, 0x60, 0x20, 0x5f, 0xf3]);
    let mut init = vec![
        0x60, 0x2a, 0x5f, 0x55, 0x60, 8, 0x60, 14, 0x5f, 0x39, 0x60, 8, 0x5f, 0xf3,
    ];
    init.extend_from_slice(&runtime);
    let mut code = vec![
        0x60,
        init.len() as u8,
        0x60,
        17,
        0x5f,
        0x39,
        0x60,
        init.len() as u8,
        0x5f,
        0x5f,
        0xf0,
        0x5f,
        0x52,
        0x60,
        0x20,
        0x5f,
        0xf3,
    ];
    code.extend_from_slice(&init);
    for keep in [false, true] {
        let result = engine
            .evaluate_call(
                Bytes::copy_from_slice(&code),
                Bytes::new(),
                addr(CALLER),
                U256::ZERO,
                200_000,
                keep,
            )
            .unwrap();
        assert!(result.success);
        assert_eq!(
            U256::from_be_slice(&result.output),
            U256::from_be_slice(created.as_slice())
        );
        assert_eq!(engine.snapshot().unwrap(), before);
        assert_eq!(
            engine.state(StateCommand::ReadNonce(addr(TARGET))).unwrap(),
            CommandValue::Number(u64::from(keep))
        );
        assert_eq!(
            engine.state(StateCommand::ReadNonce(created)).unwrap(),
            CommandValue::Number(u64::from(keep))
        );
        assert_eq!(
            engine.state(StateCommand::ReadNonce(addr(CALLER))).unwrap(),
            CommandValue::Number(1)
        );
        assert_eq!(
            engine.state(StateCommand::ReadCode(created)).unwrap(),
            CommandValue::Bytes(if keep { runtime.clone() } else { Bytes::new() })
        );
        assert_eq!(
            engine
                .state(StateCommand::ReadStorage {
                    address: created,
                    key: U256::ZERO,
                })
                .unwrap(),
            CommandValue::Word(U256::from(if keep { 42 } else { 0 }))
        );
    }
    let called = engine
        .evaluate_call(
            eval_call(&created_key, 0xf1),
            Bytes::new(),
            addr(CALLER),
            U256::ZERO,
            100_000,
            false,
        )
        .unwrap();
    assert!(called.success);
    assert_eq!(U256::from_be_slice(&called.output), U256::from(42));
    engine.resume().unwrap();
    assert!(finish(&engine).success);
    assert_eq!(
        engine.state(StateCommand::ReadNonce(addr(TARGET))).unwrap(),
        CommandValue::Number(1)
    );
    assert_eq!(
        engine.state(StateCommand::ReadCode(created)).unwrap(),
        CommandValue::Bytes(runtime)
    );
    assert_eq!(
        engine
            .state(StateCommand::ReadStorage {
                address: created,
                key: U256::ZERO,
            })
            .unwrap(),
        CommandValue::Word(U256::from(42))
    );
}

#[test]
fn nested_nonempty_calldata_and_return_memory_are_shared_only_with_eval() {
    let helper = "0x4000000000000000000000000000000000000004";
    let mut value = offline("0x5b00");
    value["witness"]["accounts"][helper] =
        json!({"balance":"0x0","nonce":0,"code":"0x5f355f5260205ff3"});
    let engine = DebugEngine::fork(parse(value)).unwrap();
    engine
        .set_breakpoints(vec![Breakpoint {
            address: addr(TARGET),
            pc: 0,
        }])
        .unwrap();
    engine.transact_json(&transaction().to_string()).unwrap();
    engine.wait(Duration::from_secs(15)).unwrap();
    engine.write_memory(0, Bytes::from(vec![0x99; 32])).unwrap();
    let before = engine.snapshot().unwrap();
    let mut code = vec![
        0x60, 0x42, 0x5f, 0x52, 0x60, 0x20, 0x5f, 0x60, 0x20, 0x5f, 0x5f, 0x73,
    ];
    code.extend_from_slice(addr(helper).as_slice());
    code.extend_from_slice(&[0x62, 0x01, 0x86, 0xa0, 0xf1, 0x50, 0x60, 0x20, 0x5f, 0xf3]);
    let result = engine
        .evaluate_call(
            code.into(),
            Bytes::new(),
            addr(CALLER),
            U256::ZERO,
            200_000,
            false,
        )
        .unwrap();
    assert!(result.success);
    assert_eq!(U256::from_be_slice(&result.output), U256::from(0x42));
    assert_eq!(engine.snapshot().unwrap(), before);
    engine.resume().unwrap();
    finish(&engine);
}

#[test]
fn type_one_two_three_execute_access_lists_and_blob_environment() {
    for kind in [1, 2, 3] {
        // Return GASPRICE, BLOBHASH(0), BLOBBASEFEE and CHAINID.
        let engine =
            DebugEngine::fork(parse(offline("0x3a5f525f496020524a6040524660605260805ff3")))
                .unwrap();
        let mut tx = transaction();
        tx["tx_type"] = json!(kind);
        tx["access_list"] = json!([{"address":TARGET,"storage_keys":[h("00")]}]);
        if kind >= 2 {
            tx["max_priority_fee_per_gas"] = json!("0x1");
        }
        let blob = format!("0x01{}", "23".repeat(31));
        if kind == 3 {
            tx["blob_hashes"] = json!([blob]);
            tx["max_fee_per_blob_gas"] = json!("0x2");
        }
        engine.transact_json(&tx.to_string()).unwrap();
        let result = finish(&engine);
        assert!(result.success);
        assert_eq!(U256::from_be_slice(&result.output[0..32]), U256::from(2));
        let expected = if kind == 3 {
            U256::from_str_radix(blob.trim_start_matches("0x"), 16).unwrap()
        } else {
            U256::ZERO
        };
        assert_eq!(U256::from_be_slice(&result.output[32..64]), expected);
        assert_eq!(U256::from_be_slice(&result.output[64..96]), U256::ONE);
        assert_eq!(U256::from_be_slice(&result.output[96..128]), U256::ONE);
        assert!(result.gas_used >= 25_300);
        let fee = result.gas_used as u128 * 2 + if kind == 3 { 131_072 } else { 0 };
        assert_eq!(
            engine
                .state(StateCommand::ReadBalance(addr(CALLER)))
                .unwrap(),
            CommandValue::Word(U256::from(u64::MAX) - U256::from(fee))
        );
    }
    let base = DebugEngine::fork(parse(offline("0x5f5400"))).unwrap();
    base.transact_json(&transaction().to_string()).unwrap();
    let cold = finish(&base).gas_used;
    let listed = DebugEngine::fork(parse(offline("0x5f5400"))).unwrap();
    let mut tx = transaction();
    tx["tx_type"] = json!(1);
    tx["access_list"] = json!([{"address":TARGET,"storage_keys":[h("00")]}]);
    listed.transact_json(&tx.to_string()).unwrap();
    assert_eq!(finish(&listed).gas_used, cold + 2_300);
}

#[test]
fn slow_single_frame_remote_reads_timeout_without_leaking_keep_or_discard() {
    for keep in [false, true] {
        let mock = Mock::delayed(vec![json!(h("00")); 6], Duration::from_millis(1450));
        let mut value = offline("0x5b00");
        value["rpc_url"] = json!(mock.endpoint);
        let engine = DebugEngine::fork(parse(value)).unwrap();
        engine
            .set_breakpoints(vec![Breakpoint {
                address: addr(TARGET),
                pc: 0,
            }])
            .unwrap();
        engine.transact_json(&transaction().to_string()).unwrap();
        engine.wait(Duration::from_secs(15)).unwrap();
        let before = engine.snapshot().unwrap();
        let mut code = vec![0x60, 0x2a, 0x5f, 0x55];
        for slot in 2..=7 {
            code.extend_from_slice(&[0x60, slot, 0x54, 0x50]);
        }
        code.push(0);
        let started = std::time::Instant::now();
        let failed = engine
            .evaluate_call(
                code.into(),
                Bytes::new(),
                addr(CALLER),
                U256::ZERO,
                200_000,
                keep,
            )
            .unwrap_err()
            .to_string();
        assert!(started.elapsed() >= Duration::from_secs(7));
        assert!(started.elapsed() < Duration::from_secs(12), "{failed}");
        assert!(failed.contains("evaluation_failed"), "{failed}");
        assert_eq!(engine.read_storage(U256::ZERO).unwrap(), U256::from(7));
        assert_eq!(
            engine
                .state(StateCommand::IsStorageWarm {
                    address: addr(TARGET),
                    key: U256::from(2)
                })
                .unwrap(),
            CommandValue::Bool(false)
        );
        assert_eq!(engine.snapshot().unwrap(), before);
        // Successful follow-up keep must clear its private read deadline before continuation.
        assert!(
            engine
                .evaluate_call(
                    Bytes::from_static(&[0x5f, 0x54, 0]),
                    Bytes::new(),
                    addr(CALLER),
                    U256::ZERO,
                    100_000,
                    true
                )
                .unwrap()
                .success
        );
        assert_eq!(engine.snapshot().unwrap(), before);
        engine.resume().unwrap();
        assert!(finish(&engine).success);
        assert_eq!(
            engine.state(StateCommand::ReadNonce(addr(CALLER))).unwrap(),
            CommandValue::Number(1)
        );
        assert_eq!(mock.requests.lock().unwrap().len(), 6);
    }
}

#[test]
fn imported_orphan_headers_and_wrong_numbers_are_rejected() {
    let mut value = offline("0x00");
    value["witness"]["headers"][h("aa")] = json!({"number":100,"parent_hash":h("99")});
    assert!(ForkConfig::from_json(&value.to_string()).is_err());
    value["witness"]["block_hashes"]["99"] = json!(h("99"));
    assert!(ForkConfig::from_json(&value.to_string()).is_ok());
    value["witness"]["headers"][h("aa")]["number"] = json!(99);
    assert!(ForkConfig::from_json(&value.to_string()).is_err());
}

#[test]
fn supported_envelope_fields_and_rejections_are_explicit() {
    let mut value = transaction();
    value["tx_type"] = json!(2);
    value["max_priority_fee_per_gas"] = json!("0x1");
    value["nonce"] = json!(12);
    value["access_list"] = json!([{"address":TARGET,"storage_keys":[h("00")]}]);
    let (tx, _) = transaction_from_json(&value.to_string()).unwrap();
    assert_eq!(tx.tx_type, 2);
    assert_eq!(tx.nonce, 12);
    assert_eq!(tx.gas_price, 2);
    assert_eq!(tx.gas_priority_fee, Some(1));
    assert_eq!(tx.access_list.0.len(), 1);
    value["tx_type"] = json!(3);
    value["blob_hashes"] = json!([format!("0x01{}", "00".repeat(31))]);
    value["max_fee_per_blob_gas"] = json!("0x3");
    let (tx, _) = transaction_from_json(&value.to_string()).unwrap();
    assert_eq!(tx.max_fee_per_blob_gas, 3);
    assert_eq!(tx.blob_hashes.len(), 1);
    for field in ["authorization_list", "unknown"] {
        let mut invalid = value.clone();
        invalid[field] = json!([]);
        assert!(transaction_from_json(&invalid.to_string()).is_err());
    }
    for kind in [4, 127] {
        value["tx_type"] = json!(kind);
        assert!(transaction_from_json(&value.to_string()).is_err());
    }
    let mut bad = transaction();
    bad["nonce"] = json!("0x0");
    assert!(transaction_from_json(&bad.to_string()).is_err());
}
