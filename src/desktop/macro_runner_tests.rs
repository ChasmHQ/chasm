use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

const ADDRESS: &str = "0x1111111111111111111111111111111111111111";
const OTHER_ADDRESS: &str = "0x2222222222222222222222222222222222222222";
const HASH: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const GENESIS: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TX_HASH: &str = "0xcccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const WORD_ZERO: &str = "0x0000000000000000000000000000000000000000000000000000000000000000";
const WORD_ONE: &str = "0x0000000000000000000000000000000000000000000000000000000000000001";

fn fixture_source() -> SourceIdentity {
    SourceIdentity {
        chain_id: "0x7a69".into(),
        genesis_hash: GENESIS.into(),
        block_number: 10,
        block_hash: HASH.into(),
    }
}

fn token() -> ExecutionToken {
    ExecutionToken {
        workspace_epoch: 7,
        network_epoch: 11,
        rpc_token: 42,
    }
}

fn literal(value: impl Into<Value>) -> Input {
    Input::Literal(value.into())
}

fn transaction() -> RawTransactionInput {
    RawTransactionInput {
        to: literal(ADDRESS),
        data: literal("0xabcd"),
        value: literal("0"),
        from: Some(literal(OTHER_ADDRESS)),
    }
}

fn probe() -> StorageProbe {
    StorageProbe {
        address: literal(ADDRESS),
        slot: literal("0"),
    }
}

fn plan(dry_run: bool, operations: Vec<Operation>) -> ExecutionPlan {
    ExecutionPlan {
        token: token(),
        dry_run,
        bundle: false,
        steps: operations
            .into_iter()
            .enumerate()
            .map(|(index, operation)| PlanStep {
                id: StepId(index as u64 + 1),
                operation,
            })
            .collect(),
    }
}

#[derive(Clone, Copy)]
enum FakeSubmission {
    Confirm,
    Unknown,
    Fail,
    BlockUntilCancelled,
}

struct FakeState {
    events: Vec<String>,
    source: SourceIdentity,
    historical: SourceIdentity,
    word: String,
    snapshot_word: String,
    submission: FakeSubmission,
    revert_ok: bool,
    bad_readback: bool,
    sweep_match: Option<u64>,
    sweep_revert: bool,
    exact_sends: Vec<RawSendRequest>,
    cancel_during_revert: bool,
    corrupt_snapshot_head: bool,
    post_revert_wrong_head: bool,
    raw_override: Option<String>,
    deploy_gas: Vec<u64>,
    abi_send_gas: Vec<Option<u64>>,
    block_number: Option<String>,
    gas_used: Option<String>,
    /// Bundle: sends queue while automine is off; `mine` confirms them.
    automine: bool,
    queued: Vec<String>,
    mined: Vec<String>,
    fail_hash: Option<usize>,
}

struct FakeBackend {
    state: Mutex<FakeState>,
    current: AtomicBool,
    lease: AtomicBool,
    send_started: tokio::sync::Notify,
}

struct FakeLease<'a>(&'a AtomicBool);
impl Drop for FakeLease<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl FakeBackend {
    fn new() -> Self {
        Self {
            state: Mutex::new(FakeState {
                events: Vec::new(),
                source: fixture_source(),
                historical: fixture_source(),
                word: WORD_ZERO.into(),
                snapshot_word: WORD_ZERO.into(),
                submission: FakeSubmission::Confirm,
                revert_ok: true,
                bad_readback: false,
                sweep_match: None,
                sweep_revert: false,
                exact_sends: Vec::new(),
                cancel_during_revert: false,
                corrupt_snapshot_head: false,
                post_revert_wrong_head: false,
                raw_override: None,
                deploy_gas: Vec::new(),
                abi_send_gas: Vec::new(),
                block_number: Some("0x0a".into()),
                gas_used: Some("21000".into()),
                automine: true,
                queued: Vec::new(),
                mined: Vec::new(),
                fail_hash: None,
            }),
            current: AtomicBool::new(true),
            lease: AtomicBool::new(false),
            send_started: tokio::sync::Notify::new(),
        }
    }

    fn events(&self) -> Vec<String> {
        self.state.lock().unwrap().events.clone()
    }

    async fn submission(
        &self,
        kind: &str,
        cancel: &CancellationToken,
        contract_address: Option<String>,
    ) -> MutationOutcome {
        {
            // Automine off: the node only queues the transaction.
            let mut state = self.state.lock().unwrap();
            if !state.automine {
                state.events.push(format!("queue:{kind}"));
                let hash = format!("0x{:064x}", state.queued.len() + 1);
                state.queued.push(hash.clone());
                return MutationOutcome::Pending { tx_hash: hash };
            }
        }
        let (outcome, block_number, gas_used) = {
            let mut state = self.state.lock().unwrap();
            state.events.push(kind.into());
            (
                state.submission,
                state.block_number.clone(),
                state.gas_used.clone(),
            )
        };
        match outcome {
            FakeSubmission::Confirm => MutationOutcome::Confirmed(ConfirmedMutation {
                contract_address,
                tx_hash: TX_HASH.into(),
                reverted: false,
                block_number,
                gas_used,
            }),
            FakeSubmission::Unknown => MutationOutcome::Unknown {
                tx_hash: Some(TX_HASH.into()),
                reason: UnknownReason::Receipt,
            },
            FakeSubmission::Fail => MutationOutcome::Failed(FailureCode::Rpc),
            FakeSubmission::BlockUntilCancelled => {
                self.send_started.notify_one();
                cancel.cancelled().await;
                MutationOutcome::Unknown {
                    tx_hash: Some(TX_HASH.into()),
                    reason: UnknownReason::Submission,
                }
            }
        }
    }
}

impl MacroBackend for FakeBackend {
    fn is_current(&self, value: &ExecutionToken) -> bool {
        self.current.load(Ordering::SeqCst) && *value == token()
    }

    fn acquire_mutation_lease<'a>(
        &'a self,
        _: &'a ExecutionToken,
        _: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<Box<dyn MutationLease + 'a>, FailureCode>> {
        Box::pin(async move {
            self.state.lock().unwrap().events.push("lease".into());
            if self.lease.swap(true, Ordering::SeqCst) {
                return Err(FailureCode::Unavailable);
            }
            Ok(Box::new(FakeLease(&self.lease)) as Box<dyn MutationLease + 'a>)
        })
    }

    fn source<'a>(
        &'a self,
        _: &'a ExecutionToken,
        block: Option<u64>,
    ) -> BoxFuture<'a, Result<SourceIdentity, FailureCode>> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            state
                .events
                .push(if block.is_some() { "source_at" } else { "head" }.into());
            Ok(if block.is_some() {
                state.historical.clone()
            } else {
                state.source.clone()
            })
        })
    }

    fn deploy<'a>(
        &'a self,
        _: &'a ExecutionToken,
        _: &'a str,
        _: &'a str,
        exact_gas: u64,
        _: SignerAlias,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, MutationOutcome> {
        Box::pin(async move {
            self.state.lock().unwrap().deploy_gas.push(exact_gas);
            self.submission("deploy", cancel, Some(ADDRESS.into()))
                .await
        })
    }

    fn abi_call<'a>(
        &'a self,
        _: &'a ExecutionToken,
        _: &'a AbiRequest,
        _: &'a SourceIdentity,
    ) -> BoxFuture<'a, ReadOutcome> {
        Box::pin(async move {
            self.state.lock().unwrap().events.push("abi_call".into());
            ReadOutcome::Value("0x01".into())
        })
    }

    fn abi_send<'a>(
        &'a self,
        _: &'a ExecutionToken,
        request: &'a AbiRequest,
        _: SignerAlias,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, MutationOutcome> {
        Box::pin(async move {
            self.state.lock().unwrap().abi_send_gas.push(request.gas);
            self.submission("abi_send", cancel, None).await
        })
    }

    fn raw_call<'a>(
        &'a self,
        _: &'a ExecutionToken,
        request: &'a RawCallRequest,
    ) -> BoxFuture<'a, ReadOutcome> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            state
                .events
                .push(format!("raw_call:{}", request.gas.unwrap_or(0)));
            if let Some(value) = &state.raw_override {
                return ReadOutcome::Value(value.clone());
            }
            if let Some(target) = state.sweep_match {
                if request.gas == Some(target) {
                    if state.sweep_revert {
                        ReadOutcome::Revert
                    } else {
                        ReadOutcome::Value("0x01".into())
                    }
                } else {
                    ReadOutcome::Value("0x00".into())
                }
            } else {
                ReadOutcome::Value("0x1234".into())
            }
        })
    }

    fn probe_gas<'a>(
        &'a self,
        _: &'a ExecutionToken,
        request: &'a ProbeGasRequest,
    ) -> BoxFuture<'a, ReadOutcome> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            state.events.push(format!("probe_gas:{}", request.gas));
            match state.sweep_match {
                Some(gas) if gas == request.gas => ReadOutcome::Value(format!("0x{}", "00".repeat(32))),
                _ => ReadOutcome::Value(format!("0x{}", "01".repeat(32))),
            }
        })
    }

    fn raw_send<'a>(
        &'a self,
        _: &'a ExecutionToken,
        request: &'a RawSendRequest,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, MutationOutcome> {
        Box::pin(async move {
            self.state.lock().unwrap().exact_sends.push(request.clone());
            self.submission("raw_send", cancel, None).await
        })
    }

    fn storage<'a>(
        &'a self,
        _: &'a ExecutionToken,
        request: &'a StorageRequest,
    ) -> BoxFuture<'a, Result<String, FailureCode>> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            state
                .events
                .push(format!("storage:{}:{}", request.address, request.slot));
            Ok(state.word.clone())
        })
    }

    fn snapshot<'a>(
        &'a self,
        _: &'a ExecutionToken,
        _: &'a CancellationToken,
    ) -> BoxFuture<'a, ControlOutcome<String>> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            state.events.push("snapshot".into());
            state.snapshot_word = state.word.clone();
            if state.corrupt_snapshot_head {
                state.source.block_hash = TX_HASH.into();
            }
            ControlOutcome::Confirmed("0x1".into())
        })
    }

    fn revert<'a>(
        &'a self,
        _: &'a ExecutionToken,
        _: &'a str,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, ControlOutcome<bool>> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            state.events.push("revert".into());
            if state.cancel_during_revert {
                cancel.cancel();
            }
            if !state.revert_ok {
                return ControlOutcome::Confirmed(false);
            }
            state.word = if state.bad_readback {
                WORD_ONE.into()
            } else {
                state.snapshot_word.clone()
            };
            state.source = fixture_source();
            state.historical = fixture_source();
            if state.post_revert_wrong_head {
                state.source.block_hash = TX_HASH.into();
            }
            ControlOutcome::Confirmed(true)
        })
    }

    fn set_automine<'a>(&'a self, _: &'a ExecutionToken, enabled: bool) -> BoxFuture<'a, ControlOutcome<bool>> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            state.events.push(format!("automine:{enabled}"));
            state.automine = enabled;
            ControlOutcome::Confirmed(true)
        })
    }

    fn mine<'a>(&'a self, _: &'a ExecutionToken) -> BoxFuture<'a, ControlOutcome<bool>> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            state.events.push("mine".into());
            let queued = std::mem::take(&mut state.queued);
            state.mined.extend(queued);
            state.word = WORD_ONE.into();
            state.source.block_number += 1;
            state.source.block_hash = TX_HASH.into();
            state.historical = state.source.clone();
            ControlOutcome::Confirmed(true)
        })
    }

    fn confirm_pending<'a>(&'a self, _: &'a ExecutionToken, tx_hash: &'a str) -> BoxFuture<'a, MutationOutcome> {
        Box::pin(async move {
            let state = self.state.lock().unwrap();
            let Some(position) = state.mined.iter().position(|hash| hash == tx_hash) else {
                return MutationOutcome::Unknown { tx_hash: Some(tx_hash.into()), reason: UnknownReason::Receipt };
            };
            MutationOutcome::Confirmed(ConfirmedMutation {
                contract_address: None,
                tx_hash: tx_hash.into(),
                reverted: state.fail_hash == Some(position),
                block_number: Some(format!("{:#x}", state.source.block_number)),
                gas_used: Some("0x5208".into()),
            })
        })
    }
}

async fn run(plan: ExecutionPlan, backend: &FakeBackend) -> RunReport {
    execute(plan, backend, &CancellationToken::new(), |_| {})
        .await
        .unwrap()
}

#[tokio::test]
async fn ordered_steps_resolve_previous_results_and_export_redacted_evidence() {
    let backend = FakeBackend::new();
    let input = plan(
        false,
        vec![
            Operation::Deploy {
                creation: literal("0x6000"),
                value: literal("0"),
                gas: literal("50000"),
                signer: SignerAlias::Handle(777),
            },
            Operation::Storage(StorageProbe {
                address: Input::Previous {
                    step: StepId(1),
                    pointer: "/address".into(),
                },
                slot: literal("0"),
            }),
            Operation::RawCall {
                transaction: transaction(),
                gas: None,
            },
        ],
    );
    let mut events = Vec::new();
    let report = execute(input, &backend, &CancellationToken::new(), |event| {
        events.push(event.clone())
    })
    .await
    .unwrap();
    assert_eq!(report.status, RunStatus::Completed);
    assert!(report
        .steps
        .iter()
        .all(|step| step.status == StepStatus::Succeeded));
    let calls = backend.events();
    let deploy = calls.iter().position(|call| call == "deploy").unwrap();
    let storage = calls
        .iter()
        .position(|call| call.starts_with("storage:"))
        .unwrap();
    let raw = calls
        .iter()
        .position(|call| call.starts_with("raw_call:"))
        .unwrap();
    assert!(deploy < storage && storage < raw);
    assert_eq!(calls[0], "lease");
    assert!(!backend.lease.load(Ordering::SeqCst));
    assert!(matches!(events.first(), Some(RunEvent::Started { .. })));
    assert!(matches!(
        events.last(),
        Some(RunEvent::Finished {
            status: RunStatus::Completed,
            ..
        })
    ));
    let evidence = export_evidence(&report);
    let serialized = evidence.to_string();
    assert!(!serialized.contains("0x6000"));
    assert!(!serialized.contains(ADDRESS));
    assert!(!serialized.contains("777"));
    assert!(!serialized.contains("rpc_token"));
    assert_eq!(evidence["digest"]["algorithm"], "keccak256");
    let encoded = serde_json::to_vec(&evidence["evidence"]).unwrap();
    assert_eq!(evidence["digest"]["value"], digest(&encoded));
}

#[tokio::test]
async fn invalid_ids_and_forward_references_are_rejected_before_any_operation() {
    let backend = FakeBackend::new();
    let mut input = plan(
        false,
        vec![Operation::Storage(StorageProbe {
            address: Input::Previous {
                step: StepId(2),
                pointer: "/value".into(),
            },
            slot: literal("0"),
        })],
    );
    let report = run(input.clone(), &backend).await;
    assert_eq!(report.status, RunStatus::InvalidPlan);
    assert_eq!(report.failure, Some(FailureCode::InvalidReference));
    input.steps[0].id = StepId(2);
    let report = run(input, &backend).await;
    assert_eq!(report.failure, Some(FailureCode::InvalidPlan));
    assert!(backend.events().is_empty());
}

#[tokio::test]
async fn cancellation_interrupts_delay_without_starting_following_steps() {
    let backend = FakeBackend::new();
    let cancel = CancellationToken::new();
    let on_start = cancel.clone();
    let report = execute(
        plan(
            false,
            vec![
                Operation::Delay {
                    milliseconds: 60_000,
                },
                Operation::RawCall {
                    transaction: transaction(),
                    gas: None,
                },
            ],
        ),
        &backend,
        &cancel,
        move |event| {
            if matches!(
                event,
                RunEvent::Step {
                    step: StepReport {
                        status: StepStatus::Running,
                        ..
                    },
                    ..
                }
            ) {
                on_start.cancel();
            }
        },
    )
    .await
    .unwrap();
    assert_eq!(report.status, RunStatus::Cancelled);
    assert_eq!(report.steps[0].status, StepStatus::Cancelled);
    assert_eq!(report.steps[1].skipped, Some(SkipReason::Stopped));
    assert!(backend.events().is_empty());
}

#[tokio::test]
async fn reverted_receipt_preserves_confirmed_hash_and_stops() {
    let mut state = State {
        outputs: HashMap::new(),
        skipped: HashSet::new(),
        snapshots: HashMap::new(),
        sweeps: HashMap::new(),
        remaining_output: MAX_TOTAL_OUTPUT_BYTES,
        pending: Vec::new(),
    };
    let mut report = StepReport {
        id: StepId(1),
        kind: StepKind::RawSend,
        status: StepStatus::Running,
        failure: None,
        unknown: None,
        skipped: None,
        output: OutputSummary::default(),
    };
    let stop = mutation_result(
        MutationOutcome::Confirmed(ConfirmedMutation {
            tx_hash: TX_HASH.into(),
            contract_address: None,
            reverted: true,
            block_number: Some("0x4".into()),
            gas_used: Some("0x5208".into()),
        }),
        StepId(1),
        &mut state,
        &mut report,
        false,
    );
    assert!(stop);
    assert_eq!(report.status, StepStatus::Failed);
    assert_eq!(report.failure, Some(FailureCode::Reverted));
    assert_eq!(report.output.submission, Some(SubmissionState::Confirmed));
    assert_eq!(report.output.transaction_hash.as_deref(), Some(TX_HASH));
    assert!(state.outputs.is_empty());
}

#[tokio::test]
async fn unknown_submission_stops_without_retry() {
    let backend = FakeBackend::new();
    backend.state.lock().unwrap().submission = FakeSubmission::Unknown;
    let report = run(
        plan(
            false,
            vec![
                Operation::RawSend {
                    transaction: transaction(),
                    gas: GasLimit::Exact(70_000),
                    signer: SignerAlias::Active,
                },
                Operation::RawSend {
                    transaction: transaction(),
                    gas: GasLimit::Exact(80_000),
                    signer: SignerAlias::Active,
                },
            ],
        ),
        &backend,
    )
    .await;
    assert_eq!(report.status, RunStatus::Unknown);
    assert_eq!(
        report.steps[0].output.submission,
        Some(SubmissionState::Unknown)
    );
    assert_eq!(
        report.steps[0].output.transaction_hash.as_deref(),
        Some(TX_HASH)
    );
    assert_eq!(report.steps[1].status, StepStatus::Skipped);
    assert_eq!(
        backend
            .events()
            .iter()
            .filter(|event| *event == "raw_send")
            .count(),
        1
    );
}

#[tokio::test]
async fn failed_submission_stops_without_retry() {
    let backend = FakeBackend::new();
    backend.state.lock().unwrap().submission = FakeSubmission::Fail;
    let report = run(
        plan(
            false,
            vec![
                Operation::Deploy {
                    creation: literal("0x6000"),
                    value: literal("0"),
                    gas: literal("50000"),
                    signer: SignerAlias::Active,
                },
                Operation::Delay { milliseconds: 0 },
            ],
        ),
        &backend,
    )
    .await;
    assert_eq!(report.status, RunStatus::Failed);
    assert_eq!(report.steps[0].failure, Some(FailureCode::Rpc));
    assert_eq!(report.steps[1].status, StepStatus::Skipped);
    assert_eq!(
        backend
            .events()
            .iter()
            .filter(|event| *event == "deploy")
            .count(),
        1
    );
}

#[tokio::test]
async fn cancellation_during_submission_preserves_unknown_outcome() {
    let backend = Arc::new(FakeBackend::new());
    backend.state.lock().unwrap().submission = FakeSubmission::BlockUntilCancelled;
    let cancel = CancellationToken::new();
    let runner_backend = backend.clone();
    let runner_cancel = cancel.clone();
    let task = tokio::spawn(async move {
        execute(
            plan(
                false,
                vec![Operation::RawSend {
                    transaction: transaction(),
                    gas: GasLimit::Exact(50_000),
                    signer: SignerAlias::Active,
                }],
            ),
            &*runner_backend,
            &runner_cancel,
            |_| {},
        )
        .await
        .unwrap()
    });
    backend.send_started.notified().await;
    cancel.cancel();
    let report = task.await.unwrap();
    assert_eq!(report.status, RunStatus::Unknown);
    assert_eq!(report.steps[0].unknown, Some(UnknownReason::Submission));
    assert_eq!(
        backend
            .events()
            .iter()
            .filter(|event| *event == "raw_send")
            .count(),
        1
    );
}

#[tokio::test]
async fn dry_run_skips_all_mutations_and_dependent_reads_but_runs_independent_reads() {
    let backend = FakeBackend::new();
    let report = run(
        plan(
            true,
            vec![
                Operation::Deploy {
                    creation: literal("0x6000"),
                    value: literal("0"),
                    gas: literal("50000"),
                    signer: SignerAlias::Active,
                },
                Operation::Storage(StorageProbe {
                    address: Input::Previous {
                        step: StepId(1),
                        pointer: "/address".into(),
                    },
                    slot: literal("0"),
                }),
                Operation::RawCall {
                    transaction: transaction(),
                    gas: None,
                },
                Operation::Snapshot {
                    probes: vec![probe()],
                },
                Operation::Revert {
                    snapshot: StepId(4),
                },
                Operation::RawSend {
                    transaction: transaction(),
                    gas: GasLimit::Exact(10),
                    signer: SignerAlias::Active,
                },
            ],
        ),
        &backend,
    )
    .await;
    assert_eq!(report.status, RunStatus::PreflightComplete);
    assert_eq!(report.steps[0].skipped, Some(SkipReason::DryRunMutation));
    assert_eq!(report.steps[1].skipped, Some(SkipReason::DryRunDependency));
    assert_eq!(report.steps[2].status, StepStatus::Succeeded);
    assert!(report.steps[3..]
        .iter()
        .all(|step| step.skipped == Some(SkipReason::DryRunMutation)));
    let calls = backend.events();
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.starts_with("raw_call:"))
            .count(),
        1
    );
    assert!(!calls.iter().any(|call| matches!(
        call.as_str(),
        "lease" | "deploy" | "snapshot" | "revert" | "raw_send"
    )));
}

#[tokio::test]
async fn sweep_uses_one_pinned_source_and_sends_exact_selected_gas() {
    let backend = FakeBackend::new();
    backend.state.lock().unwrap().sweep_match = Some(30_000);
    let report = run(
        plan(
            false,
            vec![
                Operation::GasSweep {
                    transaction: transaction(),
                    start: 10_000,
                    end: 100_000,
                    increment: 10_000,
                    max_attempts: 10,
                    predicate: SweepPredicate::NonZero,
                },
                Operation::RawSend {
                    transaction: transaction(),
                    gas: GasLimit::FromSweep(StepId(1)),
                    signer: SignerAlias::Handle(1),
                },
            ],
        ),
        &backend,
    )
    .await;
    assert_eq!(report.status, RunStatus::Completed);
    assert_eq!(report.steps[0].output.attempts, Some(3));
    assert_eq!(report.steps[0].output.selected_gas, Some(30_000));
    let state = backend.state.lock().unwrap();
    assert_eq!(
        state
            .events
            .iter()
            .filter(|call| call.starts_with("raw_call:"))
            .cloned()
            .collect::<Vec<_>>(),
        vec!["raw_call:10000", "raw_call:20000", "raw_call:30000"]
    );
    assert_eq!(state.exact_sends.len(), 1);
    assert_eq!(state.exact_sends[0].exact_gas, 30_000);
    assert_eq!(state.exact_sends[0].expected_source, Some(fixture_source()));
}

#[tokio::test]
async fn routed_probe_calibrates_and_pins_only_matching_final_transaction() {
    let backend = FakeBackend::new();
    backend.state.lock().unwrap().sweep_match = Some(0x340000 + 0x2000);
    let operation = Operation::ProbeGasSweep {
        probe: literal(ADDRESS),
        router: literal(OTHER_ADDRESS),
        data: literal("0x0011"),
        token: literal(ADDRESS),
        treasury: literal(OTHER_ADDRESS),
        from: literal(ADDRESS),
        start: 0x340000,
        end: 0x340000 + 0x4000,
        increment: 0x1000,
        max_attempts: 5,
        outer_gas_margin: 0x200000,
        final_gas_margin: 0x1000,
    };
    let final_tx = RawTransactionInput {
        to: literal(OTHER_ADDRESS), data: literal("0x0011"), value: literal("0"),
        from: Some(literal(ADDRESS)),
    };
    let report = run(plan(false, vec![operation, Operation::RawSend {
        transaction: final_tx, gas: GasLimit::FromSweep(StepId(1)), signer: SignerAlias::Active,
    }]), &backend).await;
    assert_eq!(report.status, RunStatus::Completed);
    assert_eq!(report.steps[0].output.attempts, Some(3));
    let expected = 0x340000 + 0x2000 + 0x1000 + 21_000 + 4 + 16;
    assert_eq!(report.steps[0].output.selected_gas, Some(expected));
    let state = backend.state.lock().unwrap();
    assert_eq!(state.exact_sends.len(), 1);
    assert_eq!(state.exact_sends[0].exact_gas, expected);
    assert_eq!(state.exact_sends[0].expected_source, Some(fixture_source()));
}

#[tokio::test]
async fn sweep_fails_closed_for_changed_transaction_or_source() {
    let backend = FakeBackend::new();
    backend.state.lock().unwrap().sweep_match = Some(1);
    let mut changed = transaction();
    changed.data = literal("0xff");
    let report = run(
        plan(
            false,
            vec![
                Operation::GasSweep {
                    transaction: transaction(),
                    start: 1,
                    end: 2,
                    increment: 1,
                    max_attempts: 2,
                    predicate: SweepPredicate::Success,
                },
                Operation::RawSend {
                    transaction: changed,
                    gas: GasLimit::FromSweep(StepId(1)),
                    signer: SignerAlias::Active,
                },
            ],
        ),
        &backend,
    )
    .await;
    assert_eq!(report.status, RunStatus::Failed);
    assert_eq!(report.steps[1].failure, Some(FailureCode::InvalidInput));
    assert!(backend.state.lock().unwrap().exact_sends.is_empty());

    let backend = FakeBackend::new();
    let mut pinned = fixture_source();
    pinned.block_hash = TX_HASH.into();
    backend.state.lock().unwrap().historical = pinned;
    let report = run(
        plan(
            false,
            vec![Operation::GasSweep {
                transaction: transaction(),
                start: 1,
                end: 2,
                increment: 1,
                max_attempts: 2,
                predicate: SweepPredicate::Success,
            }],
        ),
        &backend,
    )
    .await;
    assert_eq!(report.steps[0].failure, Some(FailureCode::SourceChanged));
    assert!(!backend
        .events()
        .iter()
        .any(|call| call.starts_with("raw_call:")));
}

#[tokio::test]
async fn sweep_has_hard_attempt_limit_and_revert_predicate() {
    let backend = FakeBackend::new();
    backend.state.lock().unwrap().sweep_match = Some(10_000);
    let input = |attempts| {
        plan(
            false,
            vec![Operation::GasSweep {
                transaction: transaction(),
                start: 1,
                end: 20_000,
                increment: 1,
                max_attempts: attempts,
                predicate: SweepPredicate::NonZero,
            }],
        )
    };
    let report = run(input(513), &backend).await;
    assert_eq!(report.status, RunStatus::InvalidPlan);
    assert!(backend.events().is_empty());
    let report = run(input(512), &backend).await;
    assert_eq!(report.status, RunStatus::Failed);
    assert_eq!(report.steps[0].failure, Some(FailureCode::NoMatch));
    assert_eq!(report.steps[0].output.attempts, Some(512));
    assert_eq!(
        backend
            .events()
            .iter()
            .filter(|call| call.starts_with("raw_call:"))
            .count(),
        512
    );

    let backend = FakeBackend::new();
    {
        let mut state = backend.state.lock().unwrap();
        state.sweep_match = Some(2);
        state.sweep_revert = true;
    }
    let report = run(
        plan(
            false,
            vec![Operation::GasSweep {
                transaction: transaction(),
                start: 1,
                end: 4,
                increment: 1,
                max_attempts: 4,
                predicate: SweepPredicate::Revert,
            }],
        ),
        &backend,
    )
    .await;
    assert_eq!(report.status, RunStatus::Completed);
    assert_eq!(report.steps[0].output.selected_gas, Some(2));
}

#[tokio::test]
async fn snapshot_revert_requires_successful_readback_of_each_probe() {
    let backend = FakeBackend::new();
    let report = run(
        plan(
            false,
            vec![
                Operation::Snapshot {
                    probes: vec![probe()],
                },
                Operation::Revert {
                    snapshot: StepId(1),
                },
            ],
        ),
        &backend,
    )
    .await;
    assert_eq!(report.status, RunStatus::Completed);
    assert_eq!(report.steps[1].output.verified_probes, Some(1));
    assert_eq!(
        backend
            .events()
            .iter()
            .filter(|call| call.starts_with("storage:"))
            .count(),
        2
    );
    assert!(!backend.lease.load(Ordering::SeqCst));

    let backend = FakeBackend::new();
    backend.state.lock().unwrap().bad_readback = true;
    let report = run(
        plan(
            false,
            vec![
                Operation::Snapshot {
                    probes: vec![probe()],
                },
                Operation::Revert {
                    snapshot: StepId(1),
                },
                Operation::Delay { milliseconds: 0 },
            ],
        ),
        &backend,
    )
    .await;
    assert_eq!(report.status, RunStatus::RollbackFailed);
    assert_eq!(
        report.steps[1].failure,
        Some(FailureCode::VerificationFailed)
    );
    assert_eq!(report.steps[2].status, StepStatus::Skipped);

    let backend = FakeBackend::new();
    backend.state.lock().unwrap().revert_ok = false;
    let report = run(
        plan(
            false,
            vec![
                Operation::Snapshot {
                    probes: vec![probe()],
                },
                Operation::Revert {
                    snapshot: StepId(1),
                },
            ],
        ),
        &backend,
    )
    .await;
    assert_eq!(report.status, RunStatus::RollbackFailed);
    assert_eq!(report.steps[1].failure, Some(FailureCode::RevertRejected));
}

#[tokio::test]
async fn empty_probe_revert_is_not_reported_as_success() {
    let backend = FakeBackend::new();
    let report = run(
        plan(
            false,
            vec![
                Operation::Snapshot { probes: vec![] },
                Operation::Revert {
                    snapshot: StepId(1),
                },
                Operation::Delay { milliseconds: 0 },
            ],
        ),
        &backend,
    )
    .await;
    assert_eq!(report.status, RunStatus::RevertAcknowledgedUnverified);
    assert_eq!(
        report.steps[1].status,
        StepStatus::RevertAcknowledgedUnverified
    );
    assert_eq!(report.steps[1].output.verified_probes, Some(0));
    assert_eq!(report.steps[2].status, StepStatus::Skipped);
}

#[tokio::test]
async fn revert_readback_finishes_even_if_cancellation_arrives_after_acknowledgement() {
    let backend = FakeBackend::new();
    backend.state.lock().unwrap().cancel_during_revert = true;
    let report = run(
        plan(
            false,
            vec![
                Operation::Snapshot {
                    probes: vec![probe()],
                },
                Operation::Revert {
                    snapshot: StepId(1),
                },
                Operation::Delay { milliseconds: 0 },
            ],
        ),
        &backend,
    )
    .await;
    assert_eq!(report.steps[1].status, StepStatus::Succeeded);
    assert_eq!(report.steps[1].output.verified_probes, Some(1));
    assert_eq!(report.status, RunStatus::Cancelled);
    assert_eq!(
        backend
            .events()
            .iter()
            .filter(|call| call.starts_with("storage:"))
            .count(),
        2
    );
}

#[tokio::test]
async fn snapshot_rejects_head_change_before_capture() {
    let backend = FakeBackend::new();
    backend.state.lock().unwrap().corrupt_snapshot_head = true;
    let report = run(
        plan(
            false,
            vec![
                Operation::Snapshot {
                    probes: vec![probe()],
                },
                Operation::Revert {
                    snapshot: StepId(1),
                },
            ],
        ),
        &backend,
    )
    .await;
    assert_eq!(report.status, RunStatus::Failed);
    assert_eq!(report.steps[0].failure, Some(FailureCode::SourceChanged));
    assert!(!backend.events().iter().any(|call| call == "revert"));
}

fn abi_input(signer: Option<SignerAlias>) -> AbiInput {
    AbiInput {
        address: literal(ADDRESS),
        abi: Input::Literal(json!([])),
        signature: "count()".into(),
        arguments: Input::Literal(json!([])),
        value: literal("0"),
        from: None,
        gas: Some(literal("42000")),
        signer,
    }
}

#[tokio::test]
async fn dry_run_never_dispatches_abi_mutations_and_rejects_malformed_metadata() {
    let backend = FakeBackend::new();
    let report = run(
        plan(
            true,
            vec![
                Operation::AbiCall(abi_input(Some(SignerAlias::Active))),
                Operation::AbiCall(abi_input(None)),
                Operation::RawSend {
                    transaction: transaction(),
                    gas: GasLimit::Exact(8),
                    signer: SignerAlias::Active,
                },
            ],
        ),
        &backend,
    )
    .await;
    assert_eq!(report.status, RunStatus::PreflightComplete);
    assert_eq!(report.steps[0].skipped, Some(SkipReason::DryRunMutation));
    assert_eq!(report.steps[1].status, StepStatus::Succeeded);
    assert_eq!(report.steps[2].skipped, Some(SkipReason::DryRunMutation));
    assert!(backend.events().iter().any(|event| event == "abi_call"));
    assert!(!backend
        .events()
        .iter()
        .any(|event| matches!(event.as_str(), "abi_send" | "raw_send" | "lease")));

    let backend = FakeBackend::new();
    let report = run(
        plan(
            true,
            vec![
                Operation::Snapshot { probes: vec![] },
                Operation::Revert {
                    snapshot: StepId(9),
                },
            ],
        ),
        &backend,
    )
    .await;
    assert_eq!(report.status, RunStatus::InvalidPlan);
    assert!(backend.events().is_empty());
}

#[tokio::test]
async fn oversized_plan_is_rejected_without_unbounded_report_or_backend_calls() {
    let backend = FakeBackend::new();
    let input = plan(
        true,
        (0..MAX_STEPS + 1)
            .map(|_| Operation::Delay { milliseconds: 0 })
            .collect(),
    );
    let report = run(input, &backend).await;
    assert_eq!(report.status, RunStatus::InvalidPlan);
    assert_eq!(report.steps.len(), MAX_STEPS);
    assert!(backend.events().is_empty());
}

#[tokio::test]
async fn lease_denial_or_stale_token_prevents_all_mutations() {
    let backend = FakeBackend::new();
    backend.lease.store(true, Ordering::SeqCst);
    let report = run(
        plan(
            false,
            vec![Operation::RawSend {
                transaction: transaction(),
                gas: GasLimit::Exact(1),
                signer: SignerAlias::Active,
            }],
        ),
        &backend,
    )
    .await;
    assert_eq!(report.status, RunStatus::Failed);
    assert_eq!(report.failure, Some(FailureCode::Unavailable));
    assert!(!backend.events().iter().any(|event| event == "raw_send"));

    let backend = FakeBackend::new();
    backend.current.store(false, Ordering::SeqCst);
    let report = run(plan(false, vec![Operation::Storage(probe())]), &backend).await;
    assert_eq!(report.status, RunStatus::Failed);
    assert_eq!(report.steps[0].failure, Some(FailureCode::StaleContext));
    assert!(backend.events().is_empty());
}

#[tokio::test]
async fn rollback_checks_head_identity_even_if_storage_matches() {
    let backend = FakeBackend::new();
    backend.state.lock().unwrap().post_revert_wrong_head = true;
    let report = run(
        plan(
            false,
            vec![
                Operation::Snapshot {
                    probes: vec![probe()],
                },
                Operation::Revert {
                    snapshot: StepId(1),
                },
            ],
        ),
        &backend,
    )
    .await;
    assert_eq!(report.status, RunStatus::RollbackFailed);
    assert_eq!(
        report.steps[1].failure,
        Some(FailureCode::VerificationFailed)
    );
    assert_ne!(report.steps[1].status, StepStatus::Succeeded);
}

#[tokio::test]
async fn sweeps_support_byte_equality_zero_success_and_reject_bad_results() {
    for predicate in [
        SweepPredicate::Equals(json!("0x00")),
        SweepPredicate::Zero,
        SweepPredicate::Success,
    ] {
        let backend = FakeBackend::new();
        backend.state.lock().unwrap().sweep_match = Some(10);
        let report = run(
            plan(
                false,
                vec![Operation::GasSweep {
                    transaction: transaction(),
                    start: 1,
                    end: 5,
                    increment: 1,
                    max_attempts: 5,
                    predicate,
                }],
            ),
            &backend,
        )
        .await;
        assert_eq!(report.status, RunStatus::Completed);
        assert_eq!(report.steps[0].output.selected_gas, Some(1));
    }
    let backend = FakeBackend::new();
    backend.state.lock().unwrap().raw_override = Some("secret provider message".into());
    let report = run(
        plan(
            false,
            vec![Operation::RawCall {
                transaction: transaction(),
                gas: None,
            }],
        ),
        &backend,
    )
    .await;
    assert_eq!(
        report.steps[0].failure,
        Some(FailureCode::InvalidBackendResult)
    );
    assert!(!export_evidence(&report)
        .to_string()
        .contains("secret provider message"));

    let backend = FakeBackend::new();
    backend.state.lock().unwrap().raw_override = Some("0x".into());
    let report = run(
        plan(
            false,
            vec![Operation::GasSweep {
                transaction: transaction(),
                start: 1,
                end: 1,
                increment: 1,
                max_attempts: 1,
                predicate: SweepPredicate::Zero,
            }],
        ),
        &backend,
    )
    .await;
    assert_eq!(
        report.steps[0].failure,
        Some(FailureCode::InvalidBackendResult)
    );
}

#[tokio::test]
async fn malformed_or_zero_deployment_address_cannot_be_confirmed() {
    let backend = FakeBackend::new();
    let mut state = State {
        outputs: HashMap::new(),
        skipped: HashSet::new(),
        snapshots: HashMap::new(),
        sweeps: HashMap::new(),
        remaining_output: MAX_TOTAL_OUTPUT_BYTES,
        pending: Vec::new(),
    };
    let mut step = StepReport {
        id: StepId(1),
        kind: StepKind::Deploy,
        status: StepStatus::Running,
        failure: None,
        unknown: None,
        skipped: None,
        output: OutputSummary::default(),
    };
    let stop = mutation_result(
        MutationOutcome::Confirmed(ConfirmedMutation {
            tx_hash: TX_HASH.into(),
            contract_address: Some("0x0000000000000000000000000000000000000000".into()),
            reverted: false,
            block_number: None,
            gas_used: None,
        }),
        StepId(1),
        &mut state,
        &mut step,
        true,
    );
    assert!(stop);
    assert_eq!(step.status, StepStatus::Unknown);
    assert_eq!(step.output.submission, Some(SubmissionState::Unknown));
    assert!(state.outputs.is_empty());
    assert!(backend.events().is_empty());
}

#[tokio::test]
async fn deploy_and_abi_send_receive_exact_positive_gas() {
    let backend = FakeBackend::new();
    let report = run(
        plan(
            false,
            vec![
                Operation::Deploy {
                    creation: literal("0x6000"),
                    value: literal("0"),
                    gas: literal("0x1234"),
                    signer: SignerAlias::Active,
                },
                Operation::AbiCall(abi_input(Some(SignerAlias::Active))),
            ],
        ),
        &backend,
    )
    .await;
    assert_eq!(report.status, RunStatus::Completed);
    let state = backend.state.lock().unwrap();
    assert_eq!(state.deploy_gas, vec![0x1234]);
    assert_eq!(state.abi_send_gas, vec![Some(42_000)]);
    drop(state);
    assert_eq!(report.steps[0].output.selected_gas, Some(0x1234));
    assert_eq!(report.steps[1].output.selected_gas, Some(42_000));

    let backend = FakeBackend::new();
    let report = run(
        plan(
            false,
            vec![Operation::Deploy {
                creation: literal("0x6000"),
                value: literal("0"),
                gas: literal("0"),
                signer: SignerAlias::Active,
            }],
        ),
        &backend,
    )
    .await;
    assert_eq!(report.steps[0].failure, Some(FailureCode::InvalidInput));
    assert!(backend.state.lock().unwrap().deploy_gas.is_empty());

    let backend = FakeBackend::new();
    let mut input = abi_input(Some(SignerAlias::Active));
    input.gas = None;
    let report = run(plan(false, vec![Operation::AbiCall(input)]), &backend).await;
    assert_eq!(report.status, RunStatus::InvalidPlan);
    assert!(backend.events().is_empty());

    let backend = FakeBackend::new();
    let mut input = abi_input(Some(SignerAlias::Active));
    input.gas = Some(literal("0"));
    let report = run(plan(false, vec![Operation::AbiCall(input)]), &backend).await;
    assert_eq!(report.steps[0].failure, Some(FailureCode::InvalidInput));
    assert!(backend.state.lock().unwrap().abi_send_gas.is_empty());
}

#[tokio::test]
async fn receipt_quantities_are_canonical_and_absent_fields_fail_references() {
    let backend = FakeBackend::new();
    let report = run(
        plan(
            false,
            vec![
                Operation::Deploy {
                    creation: literal("0x6000"),
                    value: literal("0"),
                    gas: literal("50000"),
                    signer: SignerAlias::Active,
                },
                Operation::Assert {
                    left: Input::Previous {
                        step: StepId(1),
                        pointer: "/block_number".into(),
                    },
                    comparator: AssertComparator::Eq,
                    right: Some(literal("0xa")),
                },
                Operation::Assert {
                    left: Input::Previous {
                        step: StepId(1),
                        pointer: "/gas_used".into(),
                    },
                    comparator: AssertComparator::Eq,
                    right: Some(literal("0x5208")),
                },
            ],
        ),
        &backend,
    )
    .await;
    assert_eq!(report.status, RunStatus::Completed);
    assert_eq!(report.steps[1].kind, StepKind::Assert);
    assert_eq!(
        export_evidence(&report)["evidence"]["steps"][1]["kind"],
        "assert"
    );

    let backend = FakeBackend::new();
    backend.state.lock().unwrap().block_number = None;
    let report = run(
        plan(
            false,
            vec![
                Operation::Deploy {
                    creation: literal("0x6000"),
                    value: literal("0"),
                    gas: literal("50000"),
                    signer: SignerAlias::Active,
                },
                Operation::Assert {
                    left: Input::Previous {
                        step: StepId(1),
                        pointer: "/block_number".into(),
                    },
                    comparator: AssertComparator::NonZero,
                    right: None,
                },
            ],
        ),
        &backend,
    )
    .await;
    assert_eq!(report.steps[0].status, StepStatus::Succeeded);
    assert_eq!(report.steps[1].failure, Some(FailureCode::InvalidReference));

    let backend = FakeBackend::new();
    backend.state.lock().unwrap().gas_used = Some("provider error text".into());
    let report = run(
        plan(
            false,
            vec![Operation::Deploy {
                creation: literal("0x6000"),
                value: literal("0"),
                gas: literal("50000"),
                signer: SignerAlias::Active,
            }],
        ),
        &backend,
    )
    .await;
    assert_eq!(report.status, RunStatus::Failed);
    assert_eq!(
        report.steps[0].failure,
        Some(FailureCode::InvalidBackendResult)
    );
    assert_eq!(
        report.steps[0].output.submission,
        Some(SubmissionState::Confirmed)
    );
    assert!(!export_evidence(&report)
        .to_string()
        .contains("provider error text"));
}

#[tokio::test]
async fn typed_assertions_stop_on_false_or_invalid_types() {
    let inputs = vec![
        (json!("0x01"), AssertComparator::Eq, Some(json!("0x01"))),
        (json!(2), AssertComparator::NotEq, Some(json!("2"))),
        (json!("0x0000"), AssertComparator::Zero, None),
        (json!("123"), AssertComparator::NonZero, None),
        (json!(true), AssertComparator::True, None),
        (json!(false), AssertComparator::False, None),
        (
            json!("0x1234"),
            AssertComparator::HashEq,
            Some(json!(digest(&[0x12, 0x34]))),
        ),
        (
            json!({"b": 1, "a": [2]}),
            AssertComparator::HashEq,
            Some(json!(digest(b"{\"a\":[2],\"b\":1}"))),
        ),
    ];
    let operations = inputs
        .into_iter()
        .map(|(left, comparator, right)| Operation::Assert {
            left: Input::Literal(left),
            comparator,
            right: right.map(Input::Literal),
        })
        .collect();
    let backend = FakeBackend::new();
    let report = run(plan(false, operations), &backend).await;
    assert_eq!(report.status, RunStatus::Completed);
    assert!(report
        .steps
        .iter()
        .all(|step| step.status == StepStatus::Succeeded));
    assert!(backend.events().is_empty());

    let backend = FakeBackend::new();
    let report = run(
        plan(
            false,
            vec![
                Operation::Assert {
                    left: literal("x"),
                    comparator: AssertComparator::Eq,
                    right: Some(literal("y")),
                },
                Operation::RawCall {
                    transaction: transaction(),
                    gas: None,
                },
            ],
        ),
        &backend,
    )
    .await;
    assert_eq!(report.status, RunStatus::Failed);
    assert_eq!(report.steps[0].failure, Some(FailureCode::AssertionFailed));
    assert!(report.steps[0].output.digest.is_some());
    assert_eq!(report.steps[1].status, StepStatus::Skipped);
    assert!(backend.events().is_empty());

    let backend = FakeBackend::new();
    let report = run(
        plan(
            false,
            vec![Operation::Assert {
                left: literal("true"),
                comparator: AssertComparator::True,
                right: None,
            }],
        ),
        &backend,
    )
    .await;
    assert_eq!(report.steps[0].failure, Some(FailureCode::InvalidInput));
    let report = run(
        plan(
            false,
            vec![Operation::Assert {
                left: literal(true),
                comparator: AssertComparator::Eq,
                right: None,
            }],
        ),
        &backend,
    )
    .await;
    assert_eq!(report.status, RunStatus::InvalidPlan);
}

#[tokio::test]
async fn assertions_can_reference_prior_pass_and_dry_run_dependency_is_skipped() {
    let backend = FakeBackend::new();
    let report = run(
        plan(
            false,
            vec![
                Operation::Assert {
                    left: literal(true),
                    comparator: AssertComparator::True,
                    right: None,
                },
                Operation::Assert {
                    left: Input::Previous {
                        step: StepId(1),
                        pointer: "/passed".into(),
                    },
                    comparator: AssertComparator::True,
                    right: None,
                },
            ],
        ),
        &backend,
    )
    .await;
    assert_eq!(report.status, RunStatus::Completed);
    let report = run(
        plan(
            true,
            vec![
                Operation::Deploy {
                    creation: literal("0x6000"),
                    value: literal("0"),
                    gas: literal("50000"),
                    signer: SignerAlias::Active,
                },
                Operation::Assert {
                    left: Input::Previous {
                        step: StepId(1),
                        pointer: "/transaction_hash".into(),
                    },
                    comparator: AssertComparator::NonZero,
                    right: None,
                },
            ],
        ),
        &backend,
    )
    .await;
    assert_eq!(report.status, RunStatus::PreflightComplete);
    assert_eq!(report.steps[1].skipped, Some(SkipReason::DryRunDependency));
}

#[tokio::test]
async fn the_chain_effect_says_sends_are_separate_and_what_stays_applied() {
    let send = || Operation::RawSend { transaction: transaction(), gas: GasLimit::Exact(70_000), signer: SignerAlias::Active };
    let backend = FakeBackend::new();
    let report = run(plan(false, vec![send(), send()]), &backend).await;
    let text = chain_effect(&report);
    assert!(text.contains("2 transactions were confirmed") && text.contains("not atomic"), "{text}");

    let unknown = FakeBackend::new();
    unknown.state.lock().unwrap().submission = FakeSubmission::Unknown;
    let text = chain_effect(&run(plan(false, vec![send(), send()]), &unknown).await);
    assert!(text.contains("No transaction was confirmed") && text.contains("unknown outcome"), "{text}");

    let dry = chain_effect(&run(plan(true, vec![send()]), &FakeBackend::new()).await);
    assert!(dry.starts_with("Dry run: nothing was signed"), "{dry}");
}

fn bundle_plan(operations: Vec<Operation>) -> ExecutionPlan {
    let mut plan = plan(false, operations);
    plan.bundle = true;
    plan
}

fn send() -> Operation {
    Operation::RawSend { transaction: transaction(), gas: GasLimit::Exact(70_000), signer: SignerAlias::Active }
}

#[tokio::test]
async fn a_bundle_mines_its_sends_in_one_block_before_reads() {
    let backend = FakeBackend::new();
    let report = run(bundle_plan(vec![send(), send(), Operation::Storage(probe())]), &backend).await;
    assert_eq!(report.status, RunStatus::Completed, "{:?}", report.steps.iter().map(|step| step.status).collect::<Vec<_>>());
    assert_eq!(report.bundle, Some(BundleOutcome::Mined { block: 11 }));
    let events = backend.events();
    let position = |event: &str| events.iter().position(|item| item == event).unwrap_or_else(|| panic!("{event} missing in {events:?}"));
    assert!(position("snapshot") < position("automine:false"));
    assert!(position("automine:false") < position("queue:raw_send"));
    assert_eq!(events.iter().filter(|event| *event == "mine").count(), 1, "one block for both sends");
    assert!(position("mine") < events.iter().position(|item| item.starts_with("storage:")).unwrap(), "reads see the mined block");
    assert_eq!(events.last().map(String::as_str), Some("automine:true"));
    assert!(report.steps[..2].iter().all(|step| step.status == StepStatus::Succeeded && step.output.submission == Some(SubmissionState::Confirmed)));
    assert!(chain_effect(&report).contains("one block"), "{}", chain_effect(&report));
}

#[tokio::test]
async fn a_failing_send_in_a_bundle_rolls_everything_back() {
    let backend = FakeBackend::new();
    backend.state.lock().unwrap().fail_hash = Some(1);
    let report = run(bundle_plan(vec![send(), send(), Operation::Storage(probe())]), &backend).await;
    assert_eq!(report.status, RunStatus::Failed);
    assert_eq!(report.bundle, Some(BundleOutcome::RolledBack { block: 10 }));
    assert_eq!(report.steps[1].failure, Some(FailureCode::Reverted));
    assert_eq!(report.steps[2].status, StepStatus::Skipped);
    let events = backend.events();
    assert!(events.contains(&"revert".to_string()) && events.contains(&"automine:true".to_string()), "{events:?}");
    let text = chain_effect(&report);
    assert!(text.contains("rolled back") && !text.contains("stay applied"), "{text}");
}

#[tokio::test]
async fn a_bundle_whose_rollback_cannot_be_verified_says_so() {
    let backend = FakeBackend::new();
    {
        let mut state = backend.state.lock().unwrap();
        state.fail_hash = Some(0);
        state.post_revert_wrong_head = true;
    }
    let report = run(bundle_plan(vec![send()]), &backend).await;
    assert_eq!(report.status, RunStatus::RollbackFailed);
    assert_eq!(report.bundle, Some(BundleOutcome::RollbackFailed));
}
