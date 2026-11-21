use super::super::{call_result::ResultContext, macro_runner::MacroBackend};
use super::*;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};

const ADDRESS: &str = "0x1111111111111111111111111111111111111111";
const HASH: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OTHER_HASH: &str = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn context(state: &str) -> ResultContext {
    ResultContext {
        contract: String::new(),
        address: String::new(),
        signature: String::new(),
        request: json!({"submissionState":state,"transactionHash":HASH}),
        workspace_epoch: 4,
        network_epoch: 7,
        source: json!({"chainId":"0x7a69","genesisHash":HASH,"block":{"number":"0x5","hash":OTHER_HASH}}),
    }
}

fn call(state: &str, receipt: Value) -> CallResult {
    CallResult::from_receipt(context(state), &Abi::default(), json!({"result":receipt}))
}

fn receipt(status: &str) -> Value {
    json!({"status":status,"transactionHash":HASH,"contractAddress":ADDRESS,"blockNumber":"0x5","blockHash":OTHER_HASH,"gasUsed":"0x5208"})
}

#[test]
fn confirmed_receipts_expose_only_allowlisted_values() {
    let mut result = call("confirmed", receipt("0x1"));
    result.raw["result"]["logs"] = json!([{"data":"must-not-appear"}]);
    let runner::MutationOutcome::Confirmed(value) = map_submission(Ok(result), true, 31337) else {
        panic!("expected confirmation")
    };
    assert_eq!(value.tx_hash, HASH);
    assert_eq!(value.contract_address.as_deref(), Some(ADDRESS));
    assert_eq!(value.block_number.as_deref(), Some("0x5"));
    assert_eq!(value.gas_used.as_deref(), Some("0x5208"));
    let runner::MutationOutcome::Confirmed(reverted) =
        map_submission(Ok(call("confirmed", receipt("0x0"))), false, 31337)
    else {
        panic!("expected verified reverted receipt")
    };
    assert!(reverted.reverted);
    assert_eq!(reverted.tx_hash, HASH);
}

#[test]
fn unknown_or_malformed_confirmation_is_never_success() {
    let result = call("unknown", Value::Null);
    assert!(matches!(
        map_submission(Ok(result), false, 31337),
        runner::MutationOutcome::Unknown {
            tx_hash: Some(_),
            ..
        }
    ));
    let mut invalid = receipt("0x1");
    invalid["transactionHash"] = json!(OTHER_HASH);
    assert!(matches!(
        map_submission(Ok(call("confirmed", invalid)), false, 31337),
        runner::MutationOutcome::Unknown {
            reason: runner::UnknownReason::InvalidConfirmation,
            ..
        }
    ));
    let mut invalid = receipt("0x1");
    invalid["blockHash"] = json!(HASH);
    assert!(matches!(
        map_submission(Ok(call("confirmed", invalid)), false, 31337),
        runner::MutationOutcome::Unknown { .. }
    ));
    let mut invalid = receipt("0x1");
    invalid["gasUsed"] = json!("broken");
    assert!(matches!(
        map_submission(Ok(call("confirmed", invalid)), false, 31337),
        runner::MutationOutcome::Unknown { .. }
    ));
    assert!(matches!(
        map_submission(Err(()), false, 31337),
        runner::MutationOutcome::Failed(runner::FailureCode::Unavailable)
    ));
}

#[test]
fn control_responses_require_boolean_quantity_and_matching_id() {
    assert!(
        matches!(parse_snapshot(json!({"jsonrpc":"2.0","id":1,"result":"0x12"})), runner::ControlOutcome::Confirmed(id) if id == "0x12")
    );
    assert!(matches!(
        parse_revert(json!({"jsonrpc":"2.0","id":1,"result":false})),
        runner::ControlOutcome::Confirmed(false)
    ));
    for response in [
        json!({"jsonrpc":"2.0","id":2,"result":"0x12"}),
        json!({"jsonrpc":"2.0","id":1,"result":12}),
        json!({"jsonrpc":"2.0","id":1,"result":"0x12","error":{"message":"secret"}}),
    ] {
        assert!(matches!(
            parse_snapshot(response),
            runner::ControlOutcome::Unknown(_)
        ));
    }
    assert!(matches!(
        parse_revert(json!({"jsonrpc":"2.0","id":1,"result":"true"})),
        runner::ControlOutcome::Unknown(_)
    ));
    assert!(matches!(
        parse_snapshot(
            json!({"jsonrpc":"2.0","id":1,"result":"0x1", "padding": "x".repeat(CONTROL_LIMIT)})
        ),
        runner::ControlOutcome::Unknown(_)
    ));
}

#[test]
fn deploy_creation_limit_does_not_relax_read_data_limit() {
    let creation = format!("0x{}", "60".repeat(40 * 1024));
    assert!(valid_creation(&creation));
    assert!(!valid_data(&creation));
    assert!(!valid_creation("0x"));
    assert!(!valid_creation("0x601"));
    assert!(!valid_creation(&format!("0x{}", "60".repeat(MAX_CREATION_HEX / 2))));
}

#[test]
fn signer_address_mapping_keeps_account_two_distinct() {
    let accounts = [
        "0x1111111111111111111111111111111111111111",
        "0x2222222222222222222222222222222222222222",
        "0x3333333333333333333333333333333333333333",
    ]
    .map(str::to_owned);
    let addresses = SignerAddresses::new(accounts, None).unwrap();
    assert_eq!(
        addresses.address(runner::SignerAlias::Handle(2)),
        Some("0x3333333333333333333333333333333333333333")
    );
    assert_eq!(addresses.address(runner::SignerAlias::Active), None);
    assert_eq!(addresses.address(runner::SignerAlias::Handle(3)), None);
}

struct Authority(Arc<AtomicBool>);
impl ContextAuthority for Authority {
    fn is_current(&self, _: &runner::ExecutionToken) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

struct Signers {
    keys: [String; 3],
    addresses: [String; 3],
    /// The unlocked signer, as (address, key), when the fixture models one.
    active: Option<(String, String)>,
}
impl SignerResolver for Signers {
    fn address(&self, alias: runner::SignerAlias) -> Option<String> {
        match alias {
            runner::SignerAlias::Handle(index) => self.addresses.get(index as usize).cloned(),
            runner::SignerAlias::Active => self.active.as_ref().map(|(address, _)| address.clone()),
        }
    }
    fn key(&self, alias: runner::SignerAlias) -> Option<String> {
        match alias {
            runner::SignerAlias::Handle(index) => self.keys.get(index as usize).cloned(),
            runner::SignerAlias::Active => self.active.as_ref().map(|(_, key)| key.clone()),
        }
    }
}

struct Submissions(Arc<Mutex<Vec<(u64, String)>>>);
impl GuardedSubmission for Submissions {
    fn deploy<'a>(
        &'a self,
        guard: &'a (dyn Fn() -> bool + Send + Sync),
        _: Arc<OwnedNode>,
        key: String,
        _: &'a str,
        _: &'a str,
        exact_gas: u64,
    ) -> BoxFuture<'a, Result<CallResult, ()>> {
        Box::pin(async move {
            if !guard() {
                return Err(());
            }
            use ethers::signers::Signer;
            let address = format!(
                "{:#x}",
                key.parse::<ethers::signers::LocalWallet>()
                    .map_err(|_| ())?
                    .address()
            );
            self.0.lock().unwrap().push((exact_gas, address));
            Ok(call("confirmed", receipt("0x1")))
        })
    }
    fn abi_send<'a>(
        &'a self,
        _: &'a (dyn Fn() -> bool + Send + Sync),
        _: Arc<OwnedNode>,
        _: String,
        _: contract_service::Request,
        _: u64,
    ) -> BoxFuture<'a, Result<CallResult, ()>> {
        Box::pin(async { Err(()) })
    }
    fn raw_send<'a>(
        &'a self,
        _: &'a (dyn Fn() -> bool + Send + Sync),
        _: Arc<OwnedNode>,
        _: String,
        _: &'a runner::RawSendRequest,
    ) -> BoxFuture<'a, Result<CallResult, ()>> {
        Box::pin(async { Err(()) })
    }
}

struct Controls;
impl OwnedControls for Controls {
    fn snapshot<'a>(
        &'a self,
        guard: &'a (dyn Fn() -> bool + Send + Sync),
    ) -> BoxFuture<'a, Result<Value, ControlTransport>> {
        Box::pin(async move {
            if guard() {
                Ok(json!({"jsonrpc":"2.0","id":1,"result":"0x7b"}))
            } else {
                Err(ControlTransport::NotDispatched)
            }
        })
    }
    fn revert<'a>(
        &'a self,
        guard: &'a (dyn Fn() -> bool + Send + Sync),
        _: &'a str,
    ) -> BoxFuture<'a, Result<Value, ControlTransport>> {
        Box::pin(async move {
            if guard() {
                Ok(json!({"jsonrpc":"2.0","id":1,"result":true}))
            } else {
                Err(ControlTransport::NotDispatched)
            }
        })
    }
}

/// `owned_listener` binds the fixed owned port 8545, so tests that need it
/// take turns.
#[cfg(windows)]
static OWNED_PORT: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(windows)]
fn owned_listener() -> (std::net::TcpListener, Arc<OwnedNode>) {
    use windows_sys::Win32::{
        Foundation::{DuplicateHandle, DUPLICATE_SAME_ACCESS},
        System::Threading::GetCurrentProcess,
    };
    let listener = std::net::TcpListener::bind("127.0.0.1:8545").unwrap();
    let process = unsafe { GetCurrentProcess() };
    let mut handle = std::ptr::null_mut();
    assert_ne!(
        unsafe {
            DuplicateHandle(
                process,
                process,
                process,
                &mut handle,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        },
        0
    );
    let node = OwnedNode::from_child_handle(handle as usize, listener.local_addr().unwrap().port());
    (listener, node)
}

#[cfg(windows)]
#[tokio::test]
async fn context_and_owned_recheck_exact_gas_and_shared_lease() {
    use ethers::signers::Signer;
    let _owned_port = OWNED_PORT.lock().unwrap();
    let (_listener, node) = owned_listener();
    let keys = [1u8, 2, 3].map(|value| format!("0x{:064x}", value));
    let account_addresses = keys.clone().map(|key| {
        format!(
            "{:#x}",
            key.parse::<ethers::signers::LocalWallet>()
                .unwrap()
                .address()
        )
    });
    let addresses = SignerAddresses::new(account_addresses.clone(), None).unwrap();
    let current = Arc::new(AtomicBool::new(true));
    let submitted = Arc::new(Mutex::new(Vec::new()));
    let gate = MutationGate::default();
    let token = runner::ExecutionToken {
        workspace_epoch: 4,
        network_epoch: 7,
        rpc_token: 9,
    };
    let backend = DesktopMacroBackend::new(
        Authority(current.clone()),
        Signers {
            keys,
            addresses: account_addresses.clone(),
            active: None,
        },
        Submissions(submitted.clone()),
        Controls,
        node.clone(),
        gate.clone(),
        token.clone(),
        addresses,
        31337,
        false,
    )
    .unwrap();
    let cancel = CancellationToken::new();
    let lease = backend
        .acquire_mutation_lease(&token, &cancel)
        .await
        .unwrap();
    assert!(gate.try_acquire().is_none());
    assert!(matches!(
        backend
            .deploy(
                &token,
                "0x6001",
                "0",
                123456,
                runner::SignerAlias::Handle(2),
                &cancel
            )
            .await,
        runner::MutationOutcome::Confirmed(_)
    ));
    assert_eq!(
        submitted.lock().unwrap().as_slice(),
        &[(123456, account_addresses[2].clone())]
    );
    assert!(!backend.is_current(&runner::ExecutionToken {
        workspace_epoch: 5,
        ..token.clone()
    }));
    assert!(!backend.is_current(&runner::ExecutionToken {
        network_epoch: 8,
        ..token.clone()
    }));
    assert!(!backend.is_current(&runner::ExecutionToken {
        rpc_token: 10,
        ..token.clone()
    }));
    current.store(false, Ordering::SeqCst);
    assert!(matches!(
        backend
            .deploy(
                &token,
                "0x6001",
                "0",
                2,
                runner::SignerAlias::Handle(2),
                &cancel
            )
            .await,
        runner::MutationOutcome::Failed(runner::FailureCode::StaleContext)
    ));
    current.store(true, Ordering::SeqCst);
    node.revoke();
    assert!(!backend.is_current(&token));
    drop(lease);
    assert!(gate.try_acquire().is_some());
}

#[test]
fn the_source_envelope_must_carry_the_expected_chain() {
    // The read result itself: chain_id, genesis hash and the block header.
    let envelope = json!({"chain_id": "1", "genesis_block_hash": HASH, "block": {"number": "0x5", "hash": OTHER_HASH}});
    let source = source_from_envelope(&envelope, 1).unwrap();
    assert_eq!(source.chain_id, "0x1");
    assert!(source_from_envelope(&envelope, 31337).is_err(), "a Source profile never accepts the local id as its source");
    let local = json!({"chain_id": "31337", "genesis_block_hash": HASH, "block": {"number": "0x5", "hash": OTHER_HASH}});
    assert_eq!(source_from_envelope(&local, 31337).unwrap().chain_id, "0x7a69");
}

#[cfg(windows)]
#[tokio::test]
async fn a_source_profile_blocks_a_personal_keystore_before_any_send() {
    use ethers::signers::Signer;
    let _owned_port = OWNED_PORT.lock().unwrap();
    let (_listener, node) = owned_listener();
    // Only the public Anvil dev keys are allowed on a Source profile, so the
    // fixture uses them rather than arbitrary keys.
    let dev: [(String, String); 3] = [0, 1, 2].map(|index| {
        let (address, key) = super::super::session::anvil_dev_account(index).unwrap();
        (address.to_string(), key.to_string())
    });
    let dev_addresses: [String; 3] = dev.clone().map(|(address, _)| address);
    let dev_keys: [String; 3] = dev.map(|(_, key)| key);
    // A personal keystore key: any 32-byte key that is not one of the dev keys.
    let personal_key = format!("0x{}", "11".repeat(32));
    let personal_address = format!("{:#x}", personal_key.parse::<ethers::signers::LocalWallet>().unwrap().address());
    let addresses = SignerAddresses::new(dev_addresses.clone(), Some(personal_address.clone())).unwrap();
    let current = Arc::new(AtomicBool::new(true));
    let submitted = Arc::new(Mutex::new(Vec::new()));
    let gate = MutationGate::default();
    let token = runner::ExecutionToken { workspace_epoch: 1, network_epoch: 2, rpc_token: 3 };
    let backend = DesktopMacroBackend::new(
        Authority(current),
        Signers { keys: dev_keys, addresses: dev_addresses, active: Some((personal_address, personal_key)) },
        Submissions(submitted.clone()),
        Controls,
        node,
        gate,
        token.clone(),
        addresses,
        1,    // Source profile: the forked chain's own id.
        true, // ...so personal keystores are blocked.
    )
    .unwrap();
    let cancel = CancellationToken::new();
    let _lease = backend.acquire_mutation_lease(&token, &cancel).await.unwrap();
    assert!(
        matches!(
            backend.deploy(&token, "0x6001", "0", 100_000, runner::SignerAlias::Active, &cancel).await,
            runner::MutationOutcome::Failed(runner::FailureCode::Unauthorized)
        ),
        "a personal keystore must not sign on a Source profile"
    );
    assert!(submitted.lock().unwrap().is_empty(), "nothing reached the submission layer");
    // A dev key passes the gate and reaches the submission layer. (The fake
    // receipt carries the local chain id, so the outcome here is not
    // Confirmed; what matters is that it was not rejected as Unauthorized.)
    let submitted_before = submitted.lock().unwrap().len();
    assert!(
        !matches!(
            backend.deploy(&token, "0x6001", "0", 100_000, runner::SignerAlias::Handle(0), &cancel).await,
            runner::MutationOutcome::Failed(runner::FailureCode::Unauthorized)
        ),
        "Anvil dev keys still send"
    );
    assert!(submitted.lock().unwrap().len() > submitted_before, "the dev key reached the submission layer");
}
