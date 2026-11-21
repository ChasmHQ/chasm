use super::*;
use serde_json::json;
use std::collections::BTreeMap;

const ADDRESS: &str = "0x1111111111111111111111111111111111111111";

fn definition(steps: Vec<model::MacroStep>) -> model::MacroDefinition {
    model::MacroDefinition {
        schema_version: model::MACRO_SCHEMA_VERSION,
        id: model::MacroId("macro".into()),
        name: "macro".into(),
        description: String::new(),
        variables: BTreeMap::new(),
        snapshot_before_run: false,
        revert_on_failure: false,
        rollback_probes: Vec::new(),
        bundle: false,
        network: None,
        steps,
        legacy_notes: Vec::new(),
    }
}
fn step(id: &str, kind: model::StepKind) -> model::MacroStep {
    model::MacroStep {
        id: model::StepId(id.into()),
        enabled: true,
        kind,
    }
}
fn token() -> runner::ExecutionToken {
    runner::ExecutionToken {
        workspace_epoch: 1,
        network_epoch: 2,
        rpc_token: 3,
    }
}
fn addresses() -> SignerAddresses {
    SignerAddresses::new(
        [
            ADDRESS.to_owned(),
            "0x2222222222222222222222222222222222222222".into(),
            "0x3333333333333333333333333333333333333333".into(),
        ],
        Some(ADDRESS.into()),
    )
    .unwrap()
}
fn raw_call() -> model::StepKind {
    model::StepKind::RawCall {
        target: ADDRESS.into(),
        data: "0x".into(),
        signer: model::Signer::AnvilAccount2,
        value: "0".into(),
        gas_limit: "123456".into(),
    }
}
fn artifact() -> super::super::foundry_build::FoundryArtifactSummary {
    super::super::foundry_build::FoundryArtifactSummary {
        id: "artifact".into(),
        source: "src/Test.sol".into(),
        contract: "Test".into(),
        abi: json!([{"type":"constructor","inputs":[{"name":"owner","type":"address"}],"stateMutability":"nonpayable"}]),
        abi_keccak256: String::new(),
        creation_bytecode: Some("0x6001".into()),
        runtime_bytecode: None,
        creation_keccak256: None,
        runtime_keccak256: None,
        creation_source_map: None,
        runtime_source_map: None,
        storage_layout: None,
        method_identifiers: None,
        creation_link_references: None,
        runtime_link_references: None,
        immutable_references: None,
        linked: true,
    }
}
fn run(
    definition: &model::MacroDefinition,
    eligible: bool,
    artifacts: &[super::super::foundry_build::FoundryArtifactSummary],
) -> Result<AdapterPlan, AdapterError> {
    adapt_checked(
        definition,
        eligible,
        artifacts,
        &addresses(),
        token(),
        false,
    )
}

#[test]
fn rejects_unverified_build_even_without_artifacts() {
    assert!(matches!(
        run(&definition(vec![step("one", raw_call())]), false, &[]),
        Err(AdapterError::IneligibleBuild)
    ));
}

#[test]
fn rejects_missing_and_non_deployable_artifacts() {
    let mut item = artifact();
    let kind = model::StepKind::DeployArtifact {
        artifact_id: "missing".into(),
        constructor_signature: "constructor()".into(),
        args: vec![],
        signer: model::Signer::AnvilAccount0,
        value: "0".into(),
        gas_limit: "123".into(),
    };
    assert!(matches!(
        run(
            &definition(vec![step("deploy", kind.clone())]),
            true,
            &[item.clone()]
        ),
        Err(AdapterError::ArtifactUnavailable)
    ));
    item.id = "missing".into();
    item.creation_bytecode = None;
    assert!(matches!(
        run(&definition(vec![step("deploy", kind)]), true, &[item]),
        Err(AdapterError::ArtifactUnavailable)
    ));
}

#[test]
fn enabled_ids_and_account_two_are_mapped_without_gaps() {
    let mut disabled = step("disabled", model::StepKind::Delay { millis: 0 });
    disabled.enabled = false;
    let definition = definition(vec![
        disabled,
        step("read", raw_call()),
        step(
            "check",
            model::StepKind::Assert {
                left: "${step.read.returnData}".into(),
                comparator: model::AssertComparator::Nonzero,
                right: None,
            },
        ),
    ]);
    let mapped = run(&definition, true, &[artifact()]).unwrap();
    assert_eq!(mapped.disabled_ids(), &[model::StepId("disabled".into())]);
    assert_eq!(
        mapped.runner_id(&model::StepId("read".into())),
        Some(runner::StepId(1))
    );
    assert_eq!(
        mapped.model_id(runner::StepId(2)),
        Some(&model::StepId("check".into()))
    );
    let runner::Operation::RawCall { transaction, gas } = &mapped.plan.steps[0].operation else {
        panic!("raw call expected")
    };
    assert!(
        matches!(&transaction.from, Some(runner::Input::Literal(Value::String(value))) if value == "0x3333333333333333333333333333333333333333")
    );
    assert!(matches!(gas, Some(runner::Input::Literal(Value::String(value))) if value == "123456"));
    let runner::Operation::Assert { left, .. } = &mapped.plan.steps[1].operation else {
        panic!("assert expected")
    };
    assert!(
        matches!(left, runner::Input::Previous { step: runner::StepId(1), pointer } if pointer == "/value")
    );
}

#[test]
fn rejects_partial_and_unavailable_runtime_references() {
    let mut invalid = raw_call();
    if let model::StepKind::RawCall { target, .. } = &mut invalid {
        *target = "prefix-${account.0.address}".into();
    }
    assert!(matches!(
        run(
            &definition(vec![step("read", invalid)]),
            true,
            &[artifact()]
        ),
        Err(AdapterError::InvalidDefinition)
    ));
    let sweep = model::StepKind::GasSweep {
        target: ADDRESS.into(),
        data: "0x".into(),
        signer: model::Signer::AnvilAccount0,
        value: "0".into(),
        start: "10".into(),
        end: "10".into(),
        increment: "1".into(),
        max_attempts: 1,
        predicate: Some(model::SweepPredicate::Success {}),
    };
    let check = model::StepKind::Assert {
        left: "${step.sweep.returnData}".into(),
        comparator: model::AssertComparator::Nonzero,
        right: None,
    };
    assert!(matches!(
        run(
            &definition(vec![step("sweep", sweep), step("check", check)]),
            true,
            &[artifact()]
        ),
        Err(AdapterError::OutputUnsupported)
    ));
}

#[test]
fn sweep_gas_equality_is_rejected_without_changing_json_type() {
    let sweep = model::StepKind::GasSweep {
        target: ADDRESS.into(),
        data: "0x".into(),
        signer: model::Signer::AnvilAccount0,
        value: "0".into(),
        start: "10".into(),
        end: "10".into(),
        increment: "1".into(),
        max_attempts: 1,
        predicate: Some(model::SweepPredicate::Success {}),
    };
    let check = model::StepKind::Assert {
        left: "${step.sweep.selectedGas}".into(),
        comparator: model::AssertComparator::Eq,
        right: Some(json!("10")),
    };
    assert!(matches!(
        run(
            &definition(vec![step("sweep", sweep), step("check", check)]),
            true,
            &[artifact()]
        ),
        Err(AdapterError::OutputUnsupported)
    ));
}

#[test]
fn automatic_snapshot_policy_is_explicitly_unsupported() {
    let mut definition = definition(vec![step("read", raw_call())]);
    definition.snapshot_before_run = true;
    assert!(matches!(
        run(&definition, true, &[artifact()]),
        Err(AdapterError::AutomaticSnapshotUnsupported)
    ));
    definition.revert_on_failure = true;
    definition.rollback_probes.push(model::StorageProbe {
        address: ADDRESS.into(),
        slot: "0".into(),
    });
    assert!(matches!(
        run(&definition, true, &[artifact()]),
        Err(AdapterError::AutomaticRollbackUnsupported)
    ));
}

#[test]
fn constructor_does_not_coerce_typed_arguments() {
    let mut item = artifact();
    item.abi = json!([{"type":"constructor","inputs":[{"name":"text","type":"string"}],"stateMutability":"nonpayable"}]);
    let deploy = model::StepKind::DeployArtifact {
        artifact_id: "artifact".into(),
        constructor_signature: "constructor(string)".into(),
        args: vec![json!(true)],
        signer: model::Signer::AnvilAccount0,
        value: "0".into(),
        gas_limit: "100000".into(),
    };
    assert!(matches!(
        run(&definition(vec![step("deploy", deploy)]), true, &[item]),
        Err(AdapterError::InvalidArguments)
    ));
}

#[test]
fn constructor_resolves_account_reference_and_exact_signature() {
    let deploy = model::StepKind::DeployArtifact {
        artifact_id: "artifact".into(),
        constructor_signature: "constructor(address)".into(),
        args: vec![json!("${account.2.address}")],
        signer: model::Signer::AnvilAccount2,
        value: "0".into(),
        gas_limit: "100000".into(),
    };
    let mapped = run(
        &definition(vec![step("deploy", deploy)]),
        true,
        &[artifact()],
    )
    .unwrap();
    let runner::Operation::Deploy {
        creation,
        gas,
        signer,
        ..
    } = &mapped.plan.steps[0].operation
    else {
        panic!("deploy expected")
    };
    assert!(matches!(signer, runner::SignerAlias::Handle(2)));
    assert!(matches!(gas, runner::Input::Literal(Value::String(value)) if value == "100000"));
    assert!(
        matches!(creation, runner::Input::Literal(Value::String(value)) if value.starts_with("0x6001") && value.ends_with("3333333333333333333333333333333333333333"))
    );
}

#[test]
fn raw_only_plans_skip_the_build_but_artifact_steps_still_need_it() {
    let raw: model::MacroDefinition = model::decode_macro(br#"{"schemaVersion":1,"id":"raw","name":"Raw","description":"","steps":[
        {"id":"snap","enabled":true,"kind":{"type":"snapshot"}},
        {"id":"read","enabled":true,"kind":{"type":"raw_call","target":"0x5fbdb2315678afecb367f032d93f642f64180aa3","data":"0x","signer":"anvil_account0","value":"0","gas_limit":"100000"}}]}"#).unwrap();
    assert!(!needs_artifacts(&raw));
    assert!(adapt_without_artifacts(&raw, &addresses(), token(), false).is_ok());
    let abi: model::MacroDefinition = model::decode_macro(br#"{"schemaVersion":1,"id":"abi","name":"Abi","description":"","steps":[
        {"id":"call","enabled":true,"kind":{"type":"abi_call","artifact_id":"a","signature":"f()","mode":"read","target":"0x5fbdb2315678afecb367f032d93f642f64180aa3","args":[],"signer":"anvil_account0","value":"0","gas_limit":"100000"}}]}"#).unwrap();
    assert!(needs_artifacts(&abi));
    assert_eq!(adapt_without_artifacts(&abi, &addresses(), token(), false).err(), Some(AdapterError::ArtifactUnavailable));
}
