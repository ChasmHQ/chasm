use super::*;
use serde_json::json;

const ADDRESS: &str = "0x1111111111111111111111111111111111111111";

fn step(id: &str, kind: StepKind) -> MacroStep {
    MacroStep {
        id: StepId(id.into()),
        enabled: true,
        kind,
    }
}

fn definition(steps: Vec<MacroStep>) -> MacroDefinition {
    MacroDefinition {
        schema_version: MACRO_SCHEMA_VERSION,
        id: MacroId("macro-1".into()),
        name: "Balance scenario".into(),
        description: "Local scenario".into(),
        variables: BTreeMap::new(),
        snapshot_before_run: false,
        revert_on_failure: false,
        rollback_probes: Vec::new(),
        steps,
        legacy_notes: Vec::new(),
    }
}

fn raw_call(target: &str) -> StepKind {
    StepKind::RawCall {
        target: target.into(),
        data: "0x1234".into(),
        signer: Signer::AnvilAccount0,
        value: "0".into(),
        gas_limit: "100000".into(),
    }
}

fn raw_send(target: &str) -> StepKind {
    StepKind::RawSend {
        target: target.into(),
        data: "0x".into(),
        signer: Signer::AnvilAccount2,
        value: "0x0".into(),
        gas_limit: "0x10000".into(),
    }
}

fn deploy() -> StepKind {
    StepKind::DeployArtifact {
        artifact_id: "artifact-1234".into(),
        constructor_signature: "constructor(uint256,address)".into(),
        args: vec![json!("1"), json!("${account.1.address}")],
        signer: Signer::AnvilAccount1,
        value: "0".into(),
        gas_limit: "3000000".into(),
    }
}

fn sweep(start: &str, end: &str, increment: &str) -> StepKind {
    StepKind::GasSweep {
        target: ADDRESS.into(),
        data: "0x1234".into(),
        signer: Signer::UnlockedWalletRef,
        value: "0".into(),
        start: start.into(),
        end: end.into(),
        increment: increment.into(),
        max_attempts: 512,
        predicate: Some(SweepPredicate::Success {}),
    }
}

fn probe(address: &str, slot: &str) -> StorageProbe {
    StorageProbe {
        address: address.into(),
        slot: slot.into(),
    }
}

fn has(errors: &[ValidationError], code: &str) -> bool {
    errors.iter().any(|error| error.code == code)
}

#[test]
fn full_schema_round_trip_and_preflight_counts() {
    let mut definition = definition(vec![
        step("deploy", deploy()),
        step(
            "read",
            StepKind::AbiCall {
                artifact_id: "artifact-1234".into(),
                signature: "balanceOf(address)".into(),
                mode: AbiCallMode::Read,
                target: "${step.deploy.contractAddress}".into(),
                args: vec![json!("${account.0.address}")],
                signer: Signer::AnvilAccount0,
                value: "0".into(),
                gas_limit: "500000".into(),
            },
        ),
        step(
            "write",
            StepKind::AbiCall {
                artifact_id: "artifact-1234".into(),
                signature: "transfer(address,uint256)".into(),
                mode: AbiCallMode::Send,
                target: "${step.deploy.contractAddress}".into(),
                args: vec![json!("${account.2.address}"), json!("42")],
                signer: Signer::UnlockedWalletRef,
                value: "0".into(),
                gas_limit: "500000".into(),
            },
        ),
        step("raw-read", raw_call(ADDRESS)),
        step("raw-write", raw_send(ADDRESS)),
        step(
            "storage",
            StepKind::StorageRead {
                target: ADDRESS.into(),
                slot: "0x00".into(),
            },
        ),
        step("snapshot", StepKind::Snapshot { probes: Vec::new() }),
        step("sweep", sweep("21000", "21004", "2")),
        step(
            "check",
            StepKind::Assert {
                left: "${step.sweep.selectedGas}".into(),
                comparator: AssertComparator::Nonzero,
                right: None,
            },
        ),
        step(
            "revert",
            StepKind::Revert {
                snapshot: "${step.snapshot.snapshotId}".into(),
            },
        ),
        step("wait", StepKind::Delay { millis: 30_000 }),
    ]);
    definition
        .variables
        .insert("limit".into(), json!("9007199254740993"));
    let preflight = definition.validate().unwrap();
    assert_eq!(
        (
            preflight.total_steps,
            preflight.enabled_steps,
            preflight.disabled_steps
        ),
        (11, 11, 0)
    );
    assert_eq!(
        (
            preflight.broadcast_count,
            preflight.read_count,
            preflight.max_read_requests
        ),
        (3, 4, 6)
    );
    assert_eq!(
        (preflight.max_sweep_attempts, preflight.total_sweep_attempts),
        (3, 3)
    );
    assert_eq!((preflight.snapshot_count, preflight.revert_count), (1, 1));
    assert_eq!(
        preflight.signers,
        vec![
            Signer::AnvilAccount1,
            Signer::AnvilAccount0,
            Signer::UnlockedWalletRef,
            Signer::AnvilAccount2
        ]
    );
    assert_eq!(
        preflight.targets,
        vec!["${step.deploy.contractAddress}", ADDRESS]
    );
    assert_eq!(preflight.artifact_ids, vec!["artifact-1234"]);
    assert!(preflight.local_only);
    assert!(preflight.revert_policy.contains("No automatic rollback"));
    let bytes = serde_json::to_vec(&definition).unwrap();
    assert_eq!(decode_macro(&bytes).unwrap(), definition);
}

#[test]
fn identifiers_and_metadata_have_exact_bounds() {
    assert!(valid_id("Az_09-"));
    assert!(valid_id(&"a".repeat(128)));
    for value in ["", "with space", "a.b", "../x", "é", &"a".repeat(129)] {
        assert!(!valid_id(value), "{value:?}");
    }
    let mut definition = definition(vec![
        step("first", raw_call(ADDRESS)),
        step("first", raw_call(ADDRESS)),
    ]);
    definition.id = MacroId("invalid.id".into());
    definition.name = " ".into();
    definition.description = "x".repeat(MAX_DESCRIPTION_BYTES + 1);
    let errors = definition.validate().unwrap_err();
    assert!(has(&errors, "id"));
    assert!(has(&errors, "name"));
    assert!(has(&errors, "description"));
    assert!(has(&errors, "duplicate_id"));
    definition.id = MacroId("ok".into());
    definition.name = "x".repeat(MAX_NAME_BYTES);
    definition.description = "x".repeat(MAX_DESCRIPTION_BYTES);
    definition.steps.pop();
    assert!(definition.validate().is_ok());
    definition.name.push('x');
    assert!(has(&definition.validate().unwrap_err(), "name"));
    assert_ne!(MacroId::new(), MacroId::new());
    assert_ne!(StepId::new(), StepId::new());
    assert_ne!(RunId::new(), RunId::new());
}

#[test]
fn exact_u256_gas_and_calldata_are_never_rounded() {
    let max = "115792089237316195423570985008687907853269984665640564039457584007913129639935";
    assert_eq!(parse_quantity(max).unwrap(), U256::MAX);
    assert_eq!(
        parse_quantity("0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff")
            .unwrap(),
        U256::MAX
    );
    for value in [
        "",
        "0x",
        "-1",
        "+1",
        "1.0",
        "1e3",
        " 1",
        "0X1",
        "0xgg",
        "115792089237316195423570985008687907853269984665640564039457584007913129639936",
    ] {
        assert!(parse_quantity(value).is_err(), "{value:?}");
    }
    assert_eq!(parse_quantity(&"0".repeat(80)).unwrap(), U256::zero());
    assert_eq!(parse_quantity("0x000001").unwrap(), U256::one());
    assert_eq!(parse_gas("18446744073709551615").unwrap(), u64::MAX);
    assert!(parse_gas("18446744073709551616").is_err());
    assert!(valid_calldata("0x"));
    assert!(valid_calldata("0xaBcD"));
    for value in ["", "00", "0x1", "0X12", "0xzz", "0x12 "] {
        assert!(!valid_calldata(value), "{value:?}");
    }
    assert!(!valid_calldata(&format!(
        "0x{}",
        "ab".repeat(MAX_STRING_BYTES / 2)
    )));
    assert!(valid_address(ADDRESS));
    assert!(!valid_address("0x1234"));
    let mut definition = definition(vec![step("send", raw_send(ADDRESS))]);
    if let StepKind::RawSend {
        gas_limit, data, ..
    } = &mut definition.steps[0].kind
    {
        *gas_limit = "0".into();
        *data = "0xabc".into();
    }
    let errors = definition.validate().unwrap_err();
    assert_eq!(
        errors.iter().filter(|error| error.code == "value").count(),
        2
    );
}

#[test]
fn references_are_whole_allowlisted_tokens_and_previous_enabled_outputs() {
    assert_eq!(
        parse_reference("${step.one.contractAddress}").unwrap(),
        Reference::Step {
            id: StepId("one".into()),
            field: "contractAddress".into()
        }
    );
    assert_eq!(
        parse_reference("${account.2.address}").unwrap(),
        Reference::Account { index: 2 }
    );
    assert_eq!(
        parse_reference("${var.some-id}").unwrap(),
        Reference::Variable {
            id: "some-id".into()
        }
    );
    for value in [
        "${account.3.address}",
        "${account.0.key}",
        "${step.one.secret}",
        "${step.x.value.extra}",
        "${env.HOME}",
        "${var.a.b}",
        "prefix${var.a}",
        "${var.a}${var.b}",
        "${var.é}",
        "${var.x",
    ] {
        assert!(parse_reference(value).is_err(), "{value}");
    }
    let invalid = definition(vec![
        step("first", raw_call("${step.later.contractAddress}")),
        step("later", raw_call("${step.first.contractAddress}")),
        step("self", raw_call("${step.self.contractAddress}")),
    ]);
    let errors = invalid.validate().unwrap_err();
    assert!(has(&errors, "reference_order"));
    assert!(has(&errors, "reference_field"));
    let mut definition = definition(vec![
        step("a", deploy()),
        step("b", raw_call("${step.a.contractAddress}")),
    ]);
    definition.steps[0].enabled = false;
    assert!(has(&definition.validate().unwrap_err(), "reference_order"));
    definition.steps[0].enabled = true;
    assert!(definition.validate().is_ok());
    if let StepKind::RawCall { target, .. } = &mut definition.steps[1].kind {
        *target = "hello ${step.a.contractAddress}".into();
    }
    assert!(has(&definition.validate().unwrap_err(), "reference"));
    if let StepKind::RawCall { target, .. } = &mut definition.steps[1].kind {
        *target = "${step.a.txHash}".into();
    }
    assert!(has(&definition.validate().unwrap_err(), "reference_type"));
}

#[test]
fn variables_are_bounded_literals_with_correct_resolved_types() {
    let mut definition = definition(vec![step("read", raw_call("${var.destination}"))]);
    assert!(has(
        &definition.validate().unwrap_err(),
        "reference_variable"
    ));
    definition
        .variables
        .insert("destination".into(), json!(ADDRESS));
    assert!(definition.validate().is_ok());
    definition
        .variables
        .insert("destination".into(), json!("0x12"));
    assert!(has(&definition.validate().unwrap_err(), "value"));
    definition.variables.insert("destination".into(), json!(12));
    assert!(has(&definition.validate().unwrap_err(), "number"));
    definition
        .variables
        .insert("destination".into(), json!("${account.0.address}"));
    assert!(has(&definition.validate().unwrap_err(), "reference"));
    definition
        .variables
        .insert("destination".into(), json!({"${var.other}": "value"}));
    assert!(has(&definition.validate().unwrap_err(), "reference"));
    definition.variables.clear();
    if let StepKind::RawCall {
        target, gas_limit, ..
    } = &mut definition.steps[0].kind
    {
        *target = ADDRESS.into();
        *gas_limit = "${account.1.address}".into();
    }
    assert!(has(&definition.validate().unwrap_err(), "reference_type"));
}

#[test]
fn sweep_delay_and_step_limits_fail_closed_at_boundaries() {
    let scenario = definition(vec![
        step("sweep", sweep("100", "611", "1")),
        step("delay", StepKind::Delay { millis: 30_000 }),
    ]);
    assert_eq!(scenario.validate().unwrap().max_sweep_attempts, 512);
    for (start, end, increment) in [
        ("100", "612", "1"),
        ("1", "514", "10"),
        ("0", "1", "1"),
        ("5", "4", "1"),
        ("1", "2", "0"),
        ("${var.start}", "2", "1"),
        ("1", "18446744073709551615", "1"),
    ] {
        let errors = definition(vec![step("s", sweep(start, end, increment))])
            .validate()
            .unwrap_err();
        assert!(
            has(&errors, "sweep") || has(&errors, "sweep_limit"),
            "{start} {end} {increment}"
        );
    }
    assert!(has(
        &definition(vec![step("d", StepKind::Delay { millis: 30_001 })])
            .validate()
            .unwrap_err(),
        "delay_limit"
    ));
    let steps: Vec<_> = (0..MAX_STEPS)
        .map(|i| step(&format!("s{i}"), StepKind::Delay { millis: 0 }))
        .collect();
    assert!(definition(steps.clone()).validate().is_ok());
    let mut steps = steps;
    steps.push(step("extra", StepKind::Delay { millis: 0 }));
    assert!(has(
        &definition(steps).validate().unwrap_err(),
        "step_limit"
    ));
}

#[test]
fn snapshots_are_explicit_prior_and_single_use() {
    let snapshot = || StepKind::Snapshot { probes: Vec::new() };
    let revert = |id: &str| StepKind::Revert {
        snapshot: format!("${{step.{id}.snapshotId}}"),
    };
    let valid = definition(vec![
        step("a", snapshot()),
        step("b", snapshot()),
        step("r1", revert("b")),
        step("r2", revert("a")),
    ]);
    assert!(valid.validate().is_ok());
    let invalid = definition(vec![
        step("a", snapshot()),
        step("b", snapshot()),
        step("r1", revert("a")),
        step("r2", revert("b")),
    ]);
    assert!(has(&invalid.validate().unwrap_err(), "snapshot"));
    let invalid = definition(vec![
        step("a", snapshot()),
        step("r1", revert("a")),
        step("r2", revert("a")),
    ]);
    assert!(has(&invalid.validate().unwrap_err(), "snapshot"));
    let invalid = definition(vec![
        step("r", revert("future")),
        step("future", snapshot()),
    ]);
    assert!(has(&invalid.validate().unwrap_err(), "reference_order"));
    let invalid = definition(vec![step(
        "r",
        StepKind::Revert {
            snapshot: "1".into(),
        },
    )]);
    assert!(has(&invalid.validate().unwrap_err(), "reference"));
}

#[test]
fn pre_run_policies_and_probes_are_validated_before_any_step() {
    let old = br#"{"schemaVersion":1,"id":"old","name":"Old","description":"","steps":[{"id":"snap","enabled":true,"kind":{"type":"snapshot"}}]}"#;
    let old = decode_macro(old).unwrap();
    assert!(!old.snapshot_before_run);
    assert!(!old.revert_on_failure);
    assert!(old.rollback_probes.is_empty());
    assert!(old.validate().is_ok());
    assert!(matches!(&old.steps[0].kind, StepKind::Snapshot { probes } if probes.is_empty()));

    let mut scenario = definition(vec![step("deploy", deploy())]);
    scenario.snapshot_before_run = true;
    scenario.revert_on_failure = true;
    scenario.variables.insert("slot".into(), json!("0x02"));
    scenario.rollback_probes = vec![probe("${account.0.address}", "${var.slot}")];
    let preflight = scenario.validate().unwrap();
    assert!(preflight.snapshot_before_run && preflight.revert_on_failure);
    assert_eq!(preflight.rollback_probe_count, 1);
    assert_eq!(preflight.max_read_requests, 2);
    assert!(preflight.revert_policy.contains("original failure"));
    assert_eq!(
        decode_macro(&serde_json::to_vec(&scenario).unwrap()).unwrap(),
        scenario
    );
    scenario.snapshot_before_run = false;
    assert!(has(&scenario.validate().unwrap_err(), "rollback_policy"));
    scenario.snapshot_before_run = true;
    scenario.rollback_probes.clear();
    assert!(has(&scenario.validate().unwrap_err(), "rollback_probes"));
    scenario
        .rollback_probes
        .push(probe("${step.deploy.contractAddress}", "0"));
    assert!(has(&scenario.validate().unwrap_err(), "reference_order"));
    scenario.rollback_probes = vec![probe(ADDRESS, "0"); MAX_PROBES];
    assert!(scenario.validate().is_ok());
    scenario.rollback_probes.push(probe(ADDRESS, "0"));
    assert!(has(&scenario.validate().unwrap_err(), "probe_limit"));
    scenario.rollback_probes = vec![probe("0x1234", "1.5")];
    let errors = scenario.validate().unwrap_err();
    assert_eq!(
        errors.iter().filter(|error| error.code == "value").count(),
        2
    );
    scenario.rollback_probes = vec![probe(ADDRESS, "0")];
    scenario.snapshot_before_run = false;
    scenario.revert_on_failure = false;
    assert!(has(&scenario.validate().unwrap_err(), "rollback_policy"));
}

#[test]
fn snapshot_probes_require_earlier_valid_values_and_bound_verification() {
    let mut scenario = definition(vec![
        step("deploy", deploy()),
        step(
            "read",
            StepKind::StorageRead {
                target: ADDRESS.into(),
                slot: "0".into(),
            },
        ),
        step(
            "snap",
            StepKind::Snapshot {
                probes: vec![probe(
                    "${step.deploy.contractAddress}",
                    "${step.read.storageValue}",
                )],
            },
        ),
        step(
            "revert",
            StepKind::Revert {
                snapshot: "${step.snap.snapshotId}".into(),
            },
        ),
        step(
            "assert",
            StepKind::Assert {
                left: "${step.revert.passed}".into(),
                comparator: AssertComparator::True,
                right: None,
            },
        ),
    ]);
    let preflight = scenario.validate().unwrap();
    assert_eq!(preflight.max_read_requests, 3);
    if let StepKind::Snapshot { probes } = &mut scenario.steps[2].kind {
        probes.clear();
    }
    assert!(has(&scenario.validate().unwrap_err(), "unverified_revert"));
    scenario.steps.pop();
    assert!(scenario.validate().is_ok());
    if let StepKind::Snapshot { probes } = &mut scenario.steps[2].kind {
        *probes = vec![probe("${step.revert.passed}", "0")];
    }
    assert!(has(&scenario.validate().unwrap_err(), "reference_order"));
    if let StepKind::Snapshot { probes } = &mut scenario.steps[2].kind {
        *probes = vec![probe(ADDRESS, "0"); MAX_PROBES];
    }
    assert!(scenario.validate().is_ok());
    if let StepKind::Snapshot { probes } = &mut scenario.steps[2].kind {
        probes.push(probe(ADDRESS, "0"));
    }
    assert!(has(
        &scenario.validate_storage().unwrap_err(),
        "probe_limit"
    ));
}

#[test]
fn sweep_predicate_and_maximum_attempts_are_explicit_and_validated() {
    for (predicate, expected) in [
        (
            SweepPredicate::Equals {
                value: "0xabcd".into(),
            },
            json!({"type":"equals","value":"0xabcd"}),
        ),
        (SweepPredicate::Zero {}, json!({"type":"zero"})),
        (SweepPredicate::Nonzero {}, json!({"type":"nonzero"})),
        (SweepPredicate::Revert {}, json!({"type":"revert"})),
        (SweepPredicate::Success {}, json!({"type":"success"})),
    ] {
        let mut operation = sweep("100", "104", "2");
        if let StepKind::GasSweep {
            predicate: current,
            max_attempts,
            ..
        } = &mut operation
        {
            *current = Some(predicate.clone());
            *max_attempts = 3;
        }
        let definition = definition(vec![step("sweep", operation)]);
        assert_eq!(definition.validate().unwrap().max_sweep_attempts, 3);
        let encoded = serde_json::to_value(&definition).unwrap();
        assert_eq!(encoded["steps"][0]["kind"]["predicate"], expected);
        assert_eq!(
            decode_macro(&serde_json::to_vec(&encoded).unwrap()).unwrap(),
            definition
        );
    }
    for value in ["", "0x0", "0xgg", "${var.expected}"] {
        let mut scenario = definition(vec![step("sweep", sweep("1", "3", "1"))]);
        if let StepKind::GasSweep { predicate, .. } = &mut scenario.steps[0].kind {
            *predicate = Some(SweepPredicate::Equals {
                value: value.into(),
            });
        }
        assert!(has(&scenario.validate().unwrap_err(), "predicate"));
    }
    let mut scenario = definition(vec![step("sweep", sweep("1", "3", "1"))]);
    if let StepKind::GasSweep { max_attempts, .. } = &mut scenario.steps[0].kind {
        *max_attempts = 2;
    }
    assert!(has(&scenario.validate().unwrap_err(), "sweep_attempts"));
    if let StepKind::GasSweep { max_attempts, .. } = &mut scenario.steps[0].kind {
        *max_attempts = 0;
    }
    assert!(has(&scenario.validate().unwrap_err(), "sweep_limit"));
    if let StepKind::GasSweep { max_attempts, .. } = &mut scenario.steps[0].kind {
        *max_attempts = 513;
    }
    assert!(has(&scenario.validate().unwrap_err(), "sweep_limit"));

    let mut value = serde_json::to_value(&scenario).unwrap();
    value["steps"][0]["kind"]
        .as_object_mut()
        .unwrap()
        .remove("max_attempts");
    value["steps"][0]["kind"]
        .as_object_mut()
        .unwrap()
        .remove("predicate");
    let draft = decode_macro(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(draft.validate_storage().is_ok());
    let errors = draft.validate().unwrap_err();
    assert!(has(&errors, "sweep_limit") && has(&errors, "predicate"));
    value["steps"][0]["kind"]["max_attempts"] = json!(3);
    value["steps"][0]["kind"]["predicate"] = json!({"type":"success","value":"0x00"});
    assert!(decode_macro(&serde_json::to_vec(&value).unwrap()).is_err());
    value["steps"][0]["kind"]["max_attempts"] = json!("3");
    assert!(decode_macro(&serde_json::to_vec(&value).unwrap()).is_err());
}

#[test]
fn public_outputs_exclude_snapshot_handles_and_unsupported_fields() {
    assert!(parse_reference("${step.read.value}").is_err());
    let mut scenario = definition(vec![
        step(
            "read",
            StepKind::AbiCall {
                artifact_id: "artifact".into(),
                signature: "f()".into(),
                mode: AbiCallMode::Read,
                target: ADDRESS.into(),
                args: vec![],
                signer: Signer::AnvilAccount0,
                value: "0".into(),
                gas_limit: "100000".into(),
            },
        ),
        step(
            "check",
            StepKind::Assert {
                left: "${step.read.returnData}".into(),
                comparator: AssertComparator::Nonzero,
                right: None,
            },
        ),
    ]);
    assert!(scenario.validate().is_ok());
    if let StepKind::Assert { left, .. } = &mut scenario.steps[1].kind {
        *left = "${step.read.value}".into();
    }
    assert!(has(&scenario.validate().unwrap_err(), "reference"));

    let mut run = MacroRun {
        run_id: RunId("run".into()),
        macro_id: MacroId("macro".into()),
        status: RunStatus::Success,
        steps: vec![StepRun {
            step_id: StepId("snap".into()),
            status: RunStatus::Success,
            request: None,
            result: None,
        }],
    };
    assert!(run.validate().is_ok());
    for output in [
        json!({"snapshotId":"0x123"}),
        json!({"value":"0x123"}),
        json!({"passed":{"snapshot_id":"0x123"}}),
    ] {
        run.steps[0].result = Some(output);
        assert!(run.validate().is_err());
    }
    run.steps[0].result = None;
    for request in [
        json!({"method":"evm_revert","params":["0x123"]}),
        json!({"nested":{"checkpointId":"0x123"}}),
        json!({"snapshot":"0x123"}),
        json!("${step.snap.snapshotId}"),
    ] {
        run.steps[0].request = Some(request);
        assert!(has(&run.validate().unwrap_err(), "snapshot_handle"));
    }
}

#[test]
fn run_results_require_matching_definition_and_verified_revert_status() {
    let mut definition = definition(vec![
        step(
            "snap",
            StepKind::Snapshot {
                probes: vec![probe(ADDRESS, "0")],
            },
        ),
        step(
            "revert",
            StepKind::Revert {
                snapshot: "${step.snap.snapshotId}".into(),
            },
        ),
        step("read", raw_call(ADDRESS)),
    ]);
    let mut run = MacroRun {
        run_id: RunId("run".into()),
        macro_id: definition.id.clone(),
        status: RunStatus::Success,
        steps: vec![
            StepRun {
                step_id: StepId("snap".into()),
                status: RunStatus::Success,
                request: None,
                result: None,
            },
            StepRun {
                step_id: StepId("revert".into()),
                status: RunStatus::Success,
                request: None,
                result: Some(json!({"passed":true})),
            },
            StepRun {
                step_id: StepId("read".into()),
                status: RunStatus::Success,
                request: Some(json!({"method":"eth_call"})),
                result: Some(json!({"returnData":"0x"})),
            },
        ],
    };
    assert!(run.validate_for(&definition).is_ok());
    run.steps[0].status = RunStatus::Unknown;
    assert!(has(
        &run.validate_for(&definition).unwrap_err(),
        "unverified_revert"
    ));
    run.steps[0].status = RunStatus::Success;
    run.steps.swap(1, 2);
    assert!(has(
        &run.validate_for(&definition).unwrap_err(),
        "run_order"
    ));
    run.steps.swap(1, 2);
    run.steps[0].request = Some(json!({"id":"0x1"}));
    assert!(has(
        &run.validate_for(&definition).unwrap_err(),
        "snapshot_handle"
    ));
    run.steps[0].request = None;
    run.steps[2].result = Some(json!({"txHash":"0x00"}));
    assert!(has(
        &run.validate_for(&definition).unwrap_err(),
        "result_field"
    ));
    run.steps[2].result = Some(json!({"returnData":"0x"}));
    run.steps[1].status = RunStatus::Failed;
    assert!(has(
        &run.validate_for(&definition).unwrap_err(),
        "unverified_revert"
    ));
    run.steps[1].status = RunStatus::Success;
    run.steps[1].result = Some(json!({"passed":false}));
    assert!(has(
        &run.validate_for(&definition).unwrap_err(),
        "unverified_revert"
    ));
    run.steps[1].result = Some(json!({"passed":true}));
    if let StepKind::Snapshot { probes } = &mut definition.steps[0].kind {
        probes.clear();
    }
    assert!(has(
        &run.validate_for(&definition).unwrap_err(),
        "unverified_revert"
    ));
    run.steps[1].result = None;
    assert!(run.validate_for(&definition).is_ok());
    run.macro_id = MacroId("other".into());
    assert!(has(
        &run.validate_for(&definition).unwrap_err(),
        "run_macro"
    ));
    run.macro_id = definition.id.clone();
    run.steps[2].step_id = StepId("other".into());
    assert!(has(&run.validate_for(&definition).unwrap_err(), "run_step"));
}

#[test]
fn assertions_require_typed_comparators_and_right_arity() {
    for (comparator, right) in [
        (AssertComparator::Eq, Some(json!("0"))),
        (AssertComparator::NotEq, Some(json!("${var.expected}"))),
        (AssertComparator::HashEq, Some(json!("0x00"))),
        (AssertComparator::Zero, None),
        (AssertComparator::Nonzero, None),
        (AssertComparator::True, None),
        (AssertComparator::False, None),
    ] {
        let mut definition = definition(vec![
            step("read", raw_call(ADDRESS)),
            step(
                "check",
                StepKind::Assert {
                    left: "${step.read.returnData}".into(),
                    comparator,
                    right,
                },
            ),
        ]);
        definition.variables.insert("expected".into(), json!("0"));
        assert!(definition.validate().is_ok());
    }
    let invalid = definition(vec![
        step("read", raw_call(ADDRESS)),
        step(
            "check",
            StepKind::Assert {
                left: "${step.read.returnData}".into(),
                comparator: AssertComparator::Eq,
                right: None,
            },
        ),
    ]);
    assert!(has(&invalid.validate().unwrap_err(), "assert_arity"));
    let invalid = definition(vec![step(
        "check",
        StepKind::Assert {
            left: "literal".into(),
            comparator: AssertComparator::True,
            right: Some(json!(true)),
        },
    )]);
    let errors = invalid.validate().unwrap_err();
    assert!(has(&errors, "reference"));
    assert!(has(&errors, "assert_arity"));
}

#[test]
fn canonical_signatures_and_artifact_ids_are_not_paths_or_row_numbers() {
    for valid in [
        "f()",
        "transfer(address,uint256)",
        "batch((address,uint256)[])",
        "f(bytes32[2],bool)",
    ] {
        assert!(valid_signature(valid, false), "{valid}");
    }
    assert!(valid_signature("constructor(uint256,address)", true));
    for invalid in [
        "f(uint)",
        "f(int7)",
        "f(bytes33)",
        "f(uint256[0])",
        "f(address x)",
        "f(address) returns (bool)",
        "f(bogus)",
        "f()oops",
        "f(,uint256)",
        "f(uint256,)",
        "f(([)])",
        "f(uint256[[0])",
        "f(([)]))",
    ] {
        assert!(!valid_signature(invalid, false), "{invalid}");
    }
    let mut definition = definition(vec![step("create", deploy())]);
    if let StepKind::DeployArtifact {
        artifact_id,
        constructor_signature,
        ..
    } = &mut definition.steps[0].kind
    {
        *artifact_id = "../out/Contract.json".into();
        *constructor_signature = "constructor(uint)".into();
    }
    let errors = definition.validate().unwrap_err();
    assert!(has(&errors, "artifact_id"));
    assert!(has(&errors, "signature"));
}

#[test]
fn unknown_fields_variants_duplicate_keys_and_oversize_json_are_rejected() {
    let definition = definition(vec![step("read", raw_call(ADDRESS))]);
    let value = serde_json::to_value(&definition).unwrap();
    for (path, extra) in [(0, "unknown"), (1, "private_key"), (2, "code")] {
        let mut mutated = value.clone();
        match path {
            0 => mutated[extra] = json!("anything"),
            1 => mutated["steps"][0][extra] = json!("fixture"),
            _ => mutated["steps"][0]["kind"][extra] = json!("fixture"),
        }
        assert!(decode_macro(&serde_json::to_vec(&mutated).unwrap()).is_err());
    }
    let mut mutated = value.clone();
    mutated["steps"][0]["kind"]["type"] = json!("shell");
    assert!(decode_macro(&serde_json::to_vec(&mutated).unwrap()).is_err());
    mutated = value.clone();
    mutated["schemaVersion"] = json!(2);
    assert!(has(
        &decode_macro(&serde_json::to_vec(&mutated).unwrap()).unwrap_err(),
        "version"
    ));
    let duplicate =
        br#"{"schemaVersion":1,"id":"id","name":"one","name":"two","description":"","steps":[]}"#;
    assert!(has(&decode_macro(duplicate).unwrap_err(), "invalid_json"));
    let duplicate_nested = br#"{"schemaVersion":1,"id":"id","name":"one","description":"","variables":{"x":{"a":1,"a":2}},"steps":[]}"#;
    assert!(has(
        &decode_macro(duplicate_nested).unwrap_err(),
        "invalid_json"
    ));
    assert!(has(
        &decode_macro(&vec![b' '; MAX_MACRO_JSON_BYTES + 1]).unwrap_err(),
        "size_limit"
    ));
    for numeric in ["1.0", "1e0"] {
        let text = format!(
            r#"{{"schemaVersion":{numeric},"id":"id","name":"one","description":"","steps":[]}}"#
        );
        assert!(decode_macro(text.as_bytes()).is_err());
        let text = format!(
            r#"{{"schemaVersion":1,"id":"id","name":"one","description":"","steps":[{{"id":"delay","enabled":true,"kind":{{"type":"delay","millis":{numeric}}}}}]}}"#
        );
        assert!(decode_macro(text.as_bytes()).is_err());
    }
}

#[test]
fn credential_and_url_screening_covers_nested_public_values_without_leaking_data() {
    let fixtures = [
        json!({"deep": [{"private_key": "fixture"}]}),
        json!({"deep": [{"password": "fixture"}]}),
        json!({"deep": [{"mnemonic": "fixture"}]}),
        json!({"deep": [{"token": "fixture"}]}),
        json!({"deep": [{"key": "fixture"}]}),
        json!({"deep": ["note https://user:pass@example.invalid/"]}),
        json!({"deep": ["password=synthetic-marker"]}),
        json!({"deep": ["bearer synthetic-marker"]}),
    ];
    for value in fixtures {
        let errors = validate_public_value(&value).unwrap_err();
        let rendered = format!("{errors:?}");
        assert!(!rendered.contains("fixture"));
        assert!(!rendered.contains("synthetic-marker"));
        assert!(!rendered.contains("pass@example"));
    }
    assert!(validate_public_value(&json!({"txHash":"0x1234","data":["0x00", true]})).is_ok());
    let mut definition = definition(vec![step("read", raw_call(ADDRESS))]);
    definition.name = "password=synthetic-marker".into();
    let errors = definition.validate().unwrap_err();
    assert!(!format!("{errors:?}").contains("synthetic-marker"));
    assert!(has(&errors, "secret_or_url"));
}

#[test]
fn strings_values_depth_and_total_bytes_are_bounded() {
    assert!(validate_public_value(&json!("a".repeat(MAX_STRING_BYTES))).is_ok());
    assert!(has(
        &validate_public_value(&json!("a".repeat(MAX_STRING_BYTES + 1))).unwrap_err(),
        "string_limit"
    ));
    assert!(has(
        &validate_public_value(&json!(vec![false; MAX_VALUE_ELEMENTS])).unwrap_err(),
        "element_limit"
    ));
    assert!(has(
        &validate_public_value(&json!(vec!["a".repeat(MAX_STRING_BYTES); 17])).unwrap_err(),
        "size_limit"
    ));
    let mut nested = json!(null);
    for _ in 0..MAX_VALUE_DEPTH + 1 {
        nested = json!([nested]);
    }
    assert!(has(
        &validate_public_value(&nested).unwrap_err(),
        "depth_limit"
    ));
}

#[test]
fn legacy_notes_migrate_losslessly_but_are_never_runnable() {
    let legacy = br#"[{"name":"Old scenario","body":"Run these notes manually: ${SHELL}\nthen inspect."},{"name":"Second","body":"a\nb"}]"#;
    let migrated = migrate_legacy(legacy).unwrap();
    assert_eq!(migrated.len(), 2);
    assert_eq!(
        migrated[0].legacy_notes[0].body,
        "Run these notes manually: ${SHELL}\nthen inspect."
    );
    assert!(migrated
        .iter()
        .all(|item| item.steps.is_empty() && has(&item.validate().unwrap_err(), "legacy_notes")));
    let persisted = serde_json::to_vec(&migrated[0]).unwrap();
    assert_eq!(decode_macro(&persisted).unwrap(), migrated[0]);
    let rejected = br#"[{"name":"Bad","body":"password=synthetic-marker"}]"#;
    let errors = migrate_legacy(rejected).unwrap_err();
    assert!(has(&errors, "secret_or_url"));
    assert!(!format!("{errors:?}").contains("synthetic-marker"));
    assert!(migrate_legacy(br#"[{"name":"Bad","body":"text","code":"x"}]"#).is_err());
}

#[test]
fn generated_prepare_keeps_dynamic_constructor_bytes_exact() {
    use crate::desktop::deployment::{prepare_creation, ConstructorSpec, DeploymentDraft};
    use ethers::abi::{decode, ParamType, Token};

    let bytes = format!("0x{}", "a1b2c3".repeat(37));
    let payload = ethers::utils::hex::decode(&bytes[2..]).unwrap();
    let artifact = "payload-artifact";
    let prepare = json!({
        "schemaVersion": 1,
        "id": "prepare",
        "name": "Prepare",
        "description": "Synthetic dynamic constructor bytes",
        "steps": [{
            "id": "deploy",
            "enabled": true,
            "kind": {
                "type": "deploy_artifact",
                "artifact_id": artifact,
                "constructor_signature": "constructor(bytes)",
                "args": [bytes],
                "signer": "anvil_account0",
                "value": "0",
                "gas_limit": "3000000"
            }
        }]
    });
    let decoded = decode_macro(&serde_json::to_vec(&prepare).unwrap()).unwrap();
    let summary = decoded.validate().unwrap();
    assert_eq!((summary.broadcast_count, summary.read_count), (1, 0));
    let StepKind::DeployArtifact {
        constructor_signature,
        args,
        ..
    } = &decoded.steps[0].kind
    else {
        panic!("expected a deploy step");
    };
    assert_eq!(constructor_signature, "constructor(bytes)");
    assert_eq!(args, &[json!(bytes)]);
    let abi = json!([{
        "type": "constructor",
        "inputs": [{"name": "payload", "type": "bytes"}],
        "stateMutability": "nonpayable"
    }]);
    let spec = ConstructorSpec::from_abi(&abi).unwrap().unwrap();
    let draft = DeploymentDraft {
        values: vec![args[0].as_str().unwrap().to_owned()],
        value_wei: "0".into(),
    };
    let creation = prepare_creation("0x6001", Some(&spec), &draft).unwrap();
    assert_eq!(&creation[..2], &[0x60, 0x01]);
    assert_eq!(
        decode(&[ParamType::Bytes], &creation[2..]).unwrap(),
        vec![Token::Bytes(payload)]
    );
}

#[test]
fn generated_poison_references_a_large_dynamic_read_for_assertion() {
    use ethers::abi::{encode, Token};

    let payload = vec![0xa5; 40_000];
    let return_data = encode(&[Token::Bytes(payload)]);
    let expected = format!("0x{}", ethers::utils::hex::encode(&return_data));
    assert!(expected.len() > MAX_STRING_BYTES);
    let hash = format!(
        "0x{}",
        ethers::utils::hex::encode(ethers::utils::keccak256(&return_data))
    );
    let poison = json!({
        "schemaVersion": 1,
        "id": "poison",
        "name": "Poison",
        "description": "Synthetic large dynamic return",
        "steps": [
            {
                "id": "read",
                "enabled": true,
                "kind": {
                    "type": "abi_call",
                    "artifact_id": "payload-artifact",
                    "signature": "readPayload()",
                    "mode": "read",
                    "target": ADDRESS,
                    "args": [],
                    "signer": "anvil_account0",
                    "value": "0",
                    "gas_limit": "3000000"
                }
            },
            {
                "id": "check",
                "enabled": true,
                "kind": {
                    "type": "assert",
                    "left": "${step.read.returnData}",
                    "comparator": "hash_eq",
                    "right": hash
                }
            }
        ]
    });
    let decoded = decode_macro(&serde_json::to_vec(&poison).unwrap()).unwrap();
    let summary = decoded.validate().unwrap();
    assert_eq!((summary.broadcast_count, summary.read_count), (0, 1));
    assert_eq!(summary.max_read_requests, 1);
    assert!(matches!(
        &decoded.steps[1].kind,
        StepKind::Assert { left, comparator: AssertComparator::HashEq, right: Some(value) }
            if left == "${step.read.returnData}" && value == &json!(hash)
    ));

    let mut inline = poison;
    inline["steps"][1]["kind"]["comparator"] = json!("eq");
    inline["steps"][1]["kind"]["right"] = json!(expected);
    assert!(has(
        &decode_macro(&serde_json::to_vec(&inline).unwrap()).unwrap_err(),
        "invalid_json"
    ));

    let run = MacroRun {
        run_id: RunId("run".into()),
        macro_id: decoded.id.clone(),
        status: RunStatus::Success,
        steps: vec![StepRun {
            step_id: StepId("read".into()),
            status: RunStatus::Success,
            request: None,
            result: Some(json!({"returnData": expected})),
        }],
    };
    assert!(has(&run.validate().unwrap_err(), "string_limit"));
}

#[test]
fn generated_fire_references_require_prior_enabled_typed_outputs() {
    let fire = json!({
        "schemaVersion": 1,
        "id": "fire-exact-gas",
        "name": "Fire exact gas",
        "description": "Synthetic same-transaction gas sweep and send",
        "steps": [
            {
                "id": "calibrate",
                "enabled": true,
                "kind": {
                    "type": "gas_sweep",
                    "target": ADDRESS,
                    "data": "0x12345678",
                    "signer": "anvil_account0",
                    "value": "0",
                    "start": "100000",
                    "end": "100447",
                    "increment": "1",
                    "max_attempts": 448,
                    "predicate": {"type": "success"}
                }
            },
            {
                "id": "send",
                "enabled": true,
                "kind": {
                    "type": "raw_send",
                    "target": ADDRESS,
                    "data": "0x12345678",
                    "signer": "anvil_account0",
                    "value": "0",
                    "gas_limit": "${step.calibrate.selectedGas}"
                }
            }
        ]
    });
    let decode = |value: &Value| decode_macro(&serde_json::to_vec(value).unwrap()).unwrap();
    let decoded = decode(&fire);
    let summary = decoded.validate().unwrap();
    assert_eq!(summary.max_sweep_attempts, 448);
    assert_eq!(summary.total_sweep_attempts, 448);
    assert_eq!(summary.max_read_requests, 448);
    assert_eq!(summary.broadcast_count, 1);
    assert!(matches!(
        &decoded.steps[1].kind,
        StepKind::RawSend { gas_limit, .. } if gas_limit == "${step.calibrate.selectedGas}"
    ));

    let mut invalid = fire.clone();
    invalid["steps"][0]["enabled"] = json!(false);
    assert!(has(&decode(&invalid).validate().unwrap_err(), "reference_order"));
    invalid = fire.clone();
    invalid["steps"][1]["kind"]["gas_limit"] = json!("${step.calibrate.returnData}");
    assert!(has(&decode(&invalid).validate().unwrap_err(), "reference_type"));
    invalid = fire.clone();
    invalid["steps"][0]["kind"]["max_attempts"] = json!(447);
    assert!(has(&decode(&invalid).validate().unwrap_err(), "sweep_attempts"));
    invalid = fire;
    invalid["steps"].as_array_mut().unwrap().swap(0, 1);
    assert!(has(&decode(&invalid).validate().unwrap_err(), "reference_order"));
}

#[test]
fn generated_fire_probe_sweep_decodes_and_validates_448_attempts() {
    let router = "0x2222222222222222222222222222222222222222";
    let end = (0x340000_u64 + 447 * 0x1000).to_string();
    let fire = json!({
        "schemaVersion": 1,
        "id": "fire",
        "name": "Fire",
        "description": "Synthetic probe and exact-gas send",
        "steps": [
            {
                "id": "calibrate",
                "enabled": true,
                "kind": {
                    "type": "probe_gas_sweep",
                    "artifact_id": "probe-artifact",
                    "probe": ADDRESS,
                    "router": router,
                    "data": "0x12345678",
                    "token": ADDRESS,
                    "treasury": "${account.1.address}",
                    "signer": "anvil_account0",
                    "value": "0",
                    "start": "3407872",
                    "end": end,
                    "increment": "4096",
                    "max_attempts": 448,
                    "outer_gas_margin": "2097152",
                    "final_gas_margin": "4096"
                }
            },
            {
                "id": "send",
                "enabled": true,
                "kind": {
                    "type": "raw_send",
                    "target": router,
                    "data": "0x12345678",
                    "signer": "anvil_account0",
                    "value": "0",
                    "gas_limit": "${step.calibrate.selectedGas}"
                }
            }
        ]
    });
    let bytes = serde_json::to_vec(&fire).unwrap();
    assert!(serde_json::from_slice::<MacroDefinition>(&bytes).is_ok());
    let decoded = decode_macro(&bytes).expect("a public probe token address is not a secret");
    let summary = decoded.validate().expect("the generated fire macro is runnable");
    assert_eq!(summary.broadcast_count, 1);
    assert_eq!(summary.read_count, 1);
    assert_eq!(summary.max_read_requests, 448);
    assert_eq!(summary.max_sweep_attempts, 448);
    assert_eq!(summary.total_sweep_attempts, 448);
    assert!(matches!(
        &decoded.steps[1].kind,
        StepKind::RawSend { gas_limit, .. } if gas_limit == "${step.calibrate.selectedGas}"
    ));
}

#[test]
fn generated_probe_exception_does_not_allow_nested_token_keys() {
    let mut probe = definition(vec![step("calibrate", StepKind::ProbeGasSweep {
        artifact_id: "probe-artifact".into(),
        probe: ADDRESS.into(),
        router: ADDRESS.into(),
        data: "0x12345678".into(),
        token: ADDRESS.into(),
        treasury: ADDRESS.into(),
        signer: Signer::AnvilAccount0,
        value: "0".into(),
        start: "100000".into(),
        end: "100000".into(),
        increment: "1".into(),
        max_attempts: 1,
        outer_gas_margin: "2097152".into(),
        final_gas_margin: "4096".into(),
    })]);
    if let StepKind::ProbeGasSweep { token, .. } = &mut probe.steps[0].kind {
        *token = "synthetic-access-token".into();
    }
    assert!(has(&probe.validate_storage().unwrap_err(), "secret_key"));
    assert!(has(&decode_macro(&serde_json::to_vec(&probe).unwrap()).unwrap_err(), "secret_key"));

    let mut scenario = definition(vec![step("read", raw_call(ADDRESS))]);
    scenario
        .variables
        .insert("details".into(), json!({"nested": [{"token": ADDRESS}]}));
    assert!(has(&scenario.validate_storage().unwrap_err(), "secret_key"));
    assert!(has(
        &decode_macro(&serde_json::to_vec(&scenario).unwrap()).unwrap_err(),
        "secret_key"
    ));

    let run = MacroRun {
        run_id: RunId("run".into()),
        macro_id: scenario.id.clone(),
        status: RunStatus::Success,
        steps: vec![StepRun {
            step_id: StepId("read".into()),
            status: RunStatus::Success,
            request: None,
            result: Some(json!({"returnData": {"nested": [{"token": ADDRESS}]}})),
        }],
    };
    assert!(has(&run.validate().unwrap_err(), "secret_key"));
    assert!(has(
        &decode_run(&serde_json::to_vec(&run).unwrap()).unwrap_err(),
        "secret_key"
    ));
}

#[test]
fn run_models_reject_unknown_fields_duplicate_ids_and_unbounded_private_data() {
    let mut run = MacroRun {
        run_id: RunId("run-1".into()),
        macro_id: MacroId("macro-1".into()),
        status: RunStatus::Running,
        steps: vec![StepRun {
            step_id: StepId("s1".into()),
            status: RunStatus::Success,
            request: Some(json!({"method":"eth_call"})),
            result: Some(json!({"returnData":"0x"})),
        }],
    };
    assert!(run.validate().is_ok());
    let bytes = serde_json::to_vec(&run).unwrap();
    assert_eq!(decode_run(&bytes).unwrap(), run);
    let mut value = serde_json::to_value(&run).unwrap();
    value["steps"][0]["result"]["password"] = json!("synthetic-marker");
    assert!(decode_run(&serde_json::to_vec(&value).unwrap()).is_err());
    value = serde_json::to_value(&run).unwrap();
    value["steps"][0]["unknown"] = json!(true);
    assert!(decode_run(&serde_json::to_vec(&value).unwrap()).is_err());
    run.steps[0].result = Some(json!({"rawProviderMessage":"fixture"}));
    assert!(has(&run.validate().unwrap_err(), "result_field"));
    run.steps[0].result = Some(json!("not an object"));
    assert!(has(&run.validate().unwrap_err(), "result_field"));
    run.steps[0].result = Some(json!({"returnData":"0x"}));
    run.steps.push(run.steps[0].clone());
    assert!(has(&run.validate().unwrap_err(), "duplicate_id"));
    for status in [
        RunStatus::Pending,
        RunStatus::Running,
        RunStatus::Success,
        RunStatus::Failed,
        RunStatus::Unknown,
        RunStatus::Skipped,
        RunStatus::RolledBack,
        RunStatus::Cancelled,
    ] {
        let encoded = serde_json::to_string(&status).unwrap();
        assert_eq!(serde_json::from_str::<RunStatus>(&encoded).unwrap(), status);
    }
}
