use revm::{
    bytecode::opcode,
    primitives::{Address, Bytes, U256, address, keccak256},
};
use sevm_revm_core::{
    AccountSpec, Breakpoint, CHEATCODE_ADDRESS, ChainConfig, DEFAULT_CALLER, DEFAULT_TARGET,
    DebugEngine, DebugEvent, PauseReason, PrototypeSession, SessionConfig, TransactionRequest,
};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(5);

fn paused(session: &PrototypeSession) -> sevm_revm_core::Snapshot {
    match session.wait(TIMEOUT).unwrap() {
        DebugEvent::Paused(snapshot) => snapshot,
        event => panic!("expected pause, got {event:?}"),
    }
}

fn finished(session: &PrototypeSession) -> sevm_revm_core::Finished {
    match session.wait(TIMEOUT).unwrap() {
        DebugEvent::Finished(result) => result,
        event => panic!("expected finish, got {event:?}"),
    }
}

fn push(code: &mut Vec<u8>, value: &[u8]) {
    assert!(!value.is_empty() && value.len() <= 32);
    code.push(opcode::PUSH0 + value.len() as u8);
    code.extend_from_slice(value);
}

fn push_u64(code: &mut Vec<u8>, value: u64) {
    if value == 0 {
        code.push(opcode::PUSH0);
        return;
    }
    let bytes = value.to_be_bytes();
    let start = bytes.iter().position(|byte| *byte != 0).unwrap();
    push(code, &bytes[start..]);
}

fn emit_call(code: &mut Vec<u8>, target: Address, input_offset: u64, input_len: u64) {
    push_u64(code, 0);
    push_u64(code, 0);
    push_u64(code, input_len);
    push_u64(code, input_offset);
    push_u64(code, 0);
    push(code, target.as_slice());
    push_u64(code, 50_000);
    code.push(opcode::CALL);
    code.push(opcode::POP);
}

#[test]
fn mutates_live_stack_memory_storage_gas_and_pc() {
    let code = Bytes::from_static(&[
        opcode::JUMPDEST,
        opcode::PUSH1,
        1,
        opcode::PUSH0,
        opcode::SSTORE,
        opcode::STOP,
        opcode::JUMPDEST,
        opcode::PUSH1,
        2,
        opcode::PUSH0,
        opcode::SSTORE,
        opcode::STOP,
    ]);
    let session = PrototypeSession::start(
        SessionConfig::new(DEFAULT_TARGET, code).with_breakpoint(DEFAULT_TARGET, 0),
    );

    let snapshot = paused(&session);
    assert_eq!(snapshot.reason, PauseReason::Breakpoint);
    assert_eq!(snapshot.pc, 0);
    session
        .write_memory(3, Bytes::from_static(&[0xaa, 0xbb]))
        .unwrap();
    session.write_storage(U256::from(7), U256::from(8)).unwrap();
    session.set_gas(90_000).unwrap();
    session.set_pc(6).unwrap();
    let changed = session.snapshot().unwrap();
    assert_eq!(&changed.memory[3..5], &[0xaa, 0xbb]);
    assert_eq!(changed.gas_remaining, 90_000);
    assert_eq!(changed.pc, 6);
    session.resume().unwrap();

    let result = finished(&session);
    assert!(result.success);
    assert_eq!(result.storage_at(DEFAULT_TARGET, U256::ZERO), U256::from(2));
    assert_eq!(
        result.storage_at(DEFAULT_TARGET, U256::from(7)),
        U256::from(8)
    );
}

#[test]
fn rewrites_the_operand_before_sstore_consumes_it() {
    let code = Bytes::from_static(&[
        opcode::PUSH1,
        1,
        opcode::PUSH0,
        opcode::SSTORE,
        opcode::STOP,
    ]);
    let session = PrototypeSession::start(
        SessionConfig::new(DEFAULT_TARGET, code).with_breakpoint(DEFAULT_TARGET, 3),
    );

    let snapshot = paused(&session);
    assert_eq!(snapshot.stack, vec![U256::ZERO, U256::from(1)]);
    session.set_stack(1, U256::from(9)).unwrap();
    session.resume().unwrap();

    let result = finished(&session);
    assert!(result.success);
    assert_eq!(result.storage_at(DEFAULT_TARGET, U256::ZERO), U256::from(9));
}

#[test]
fn rescues_out_of_gas_and_retries_the_failed_opcode() {
    let code = Bytes::from_static(&[
        opcode::PUSH1,
        7,
        opcode::PUSH0,
        opcode::SSTORE,
        opcode::STOP,
    ]);
    let mut config = SessionConfig::new(DEFAULT_TARGET, code);
    config.gas_limit = 22_000;
    let session = PrototypeSession::start(config);

    let snapshot = paused(&session);
    assert_eq!(snapshot.reason, PauseReason::OutOfGas);
    assert_eq!(snapshot.pc, 3);
    assert_eq!(snapshot.stack, vec![U256::ZERO, U256::from(7)]);
    session.set_gas(50_000).unwrap();
    session.resume().unwrap();

    let result = finished(&session);
    assert!(result.success);
    assert_eq!(result.storage_at(DEFAULT_TARGET, U256::ZERO), U256::from(7));
}

#[test]
fn speculative_execution_reverts_its_journal_checkpoint() {
    let session = PrototypeSession::start(
        SessionConfig::new(DEFAULT_TARGET, Bytes::from_static(&[opcode::STOP]))
            .with_breakpoint(DEFAULT_TARGET, 0),
    );
    paused(&session);

    let code = Bytes::from_static(&[
        opcode::PUSH1,
        42,
        opcode::PUSH1,
        9,
        opcode::SSTORE,
        opcode::PUSH1,
        42,
        opcode::PUSH0,
        opcode::MSTORE,
        opcode::PUSH1,
        32,
        opcode::PUSH0,
        opcode::RETURN,
    ]);
    let output = session.evaluate(code, false).unwrap();
    assert_eq!(output.len(), 32);
    assert_eq!(output[31], 42);
    assert_eq!(session.read_storage(U256::from(9)).unwrap(), U256::ZERO);
    session.resume().unwrap();
    assert!(finished(&session).success);
}

#[test]
fn persists_state_across_create_and_call_transactions() {
    let runtime = [
        &[opcode::JUMPDEST; 16][..],
        &[
            opcode::PUSH0,
            opcode::SLOAD,
            opcode::PUSH1,
            1,
            opcode::ADD,
            opcode::PUSH0,
            opcode::SSTORE,
            opcode::STOP,
        ],
    ]
    .concat();
    let mut init_code = vec![
        opcode::PUSH1,
        runtime.len() as u8,
        opcode::PUSH1,
        10,
        opcode::PUSH0,
        opcode::CODECOPY,
        opcode::PUSH1,
        runtime.len() as u8,
        opcode::PUSH0,
        opcode::RETURN,
    ];
    init_code.extend(runtime);
    let created = DEFAULT_CALLER.create(0);
    let engine = DebugEngine::new(ChainConfig {
        accounts: vec![AccountSpec {
            address: DEFAULT_CALLER,
            code: Bytes::new(),
            balance: U256::MAX,
            storage: Vec::new(),
        }],
        breakpoints: Vec::new(),
    });

    engine
        .transact(TransactionRequest::create(DEFAULT_CALLER, init_code))
        .unwrap();
    let deployment = finished(&engine);
    assert!(deployment.success);
    assert_eq!(deployment.created_address, Some(created));
    assert_eq!(
        engine
            .set_breakpoints(vec![Breakpoint {
                address: created,
                pc: 22,
            }])
            .unwrap(),
        1
    );

    for expected in [U256::from(1), U256::from(2)] {
        engine
            .transact(TransactionRequest::call(
                DEFAULT_CALLER,
                created,
                Bytes::new(),
            ))
            .unwrap();
        assert_eq!(paused(&engine).pc, 22);
        engine.resume().unwrap();
        let result = finished(&engine);
        assert!(result.success);
        assert_eq!(result.storage_at(created, U256::ZERO), expected);
    }
}

#[test]
fn foundry_style_prank_changes_the_next_nested_caller() {
    let child = address!("3000000000000000000000000000000000000003");
    let pranked = address!("4000000000000000000000000000000000000004");
    let mut parent_code = Vec::new();
    let mut selector_word = [0u8; 32];
    selector_word[..4].copy_from_slice(&keccak256("prank(address)")[..4]);
    push(&mut parent_code, &selector_word);
    push_u64(&mut parent_code, 0);
    parent_code.push(opcode::MSTORE);
    push(&mut parent_code, pranked.as_slice());
    push_u64(&mut parent_code, 4);
    parent_code.push(opcode::MSTORE);
    emit_call(&mut parent_code, CHEATCODE_ADDRESS, 0, 36);
    emit_call(&mut parent_code, child, 0, 0);
    parent_code.push(opcode::STOP);

    let child_code =
        Bytes::from_static(&[opcode::CALLER, opcode::PUSH0, opcode::SSTORE, opcode::STOP]);
    let session = PrototypeSession::start(
        SessionConfig::new(DEFAULT_TARGET, parent_code)
            .with_account(AccountSpec::new(child, child_code))
            .with_breakpoint(child, 0),
    );

    let snapshot = paused(&session);
    assert_eq!(snapshot.address, child);
    assert_eq!(snapshot.depth, 1);
    session.resume().unwrap();

    let result = finished(&session);
    assert!(result.success);
    assert_eq!(
        result.storage_at(child, U256::ZERO),
        U256::from_be_slice(pranked.as_slice())
    );
}
