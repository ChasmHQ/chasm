use super::*;
use slint::Model;

fn definition(name: &str) -> MacroDefinition {
    MacroDefinition {
        schema_version: MACRO_SCHEMA_VERSION,
        id: MacroId::new(),
        name: name.into(),
        description: String::new(),
        variables: BTreeMap::new(),
        snapshot_before_run: false,
        revert_on_failure: false,
        rollback_probes: Vec::new(),
        steps: Vec::new(),
        legacy_notes: Vec::new(),
    }
}

fn record(definition: MacroDefinition) -> Record {
    Record {
        draft: encode_definition(&definition).unwrap(),
        definition,
    }
}

fn snapshot(library: &mut Library) -> ValidationSnapshot {
    let (id, json, revision) = library.validate_draft().unwrap();
    let definition = macro_model::decode_macro(json.as_bytes()).unwrap();
    ValidationSnapshot {
        id,
        json,
        revision,
        result: definition.validate(),
        definition: Some(definition),
    }
}

#[test]
fn validation_is_exact_and_stale_responses_do_not_reauthorize() {
    let first = record(definition("First"));
    let second = record(definition("Second"));
    let first_id = first.definition.id.clone();
    let first_json = first.draft.clone();
    let mut library = Library::default();
    library.records = vec![first, second];
    library.selected = Some(first_id.clone());

    let old = snapshot(&mut library);
    library.edit(format!("{} ", first_json));
    assert!(library.validation_current().is_none());
    assert!(!library.install_validation(old));

    let old = snapshot(&mut library);
    library.select(1);
    library.select(0);
    assert_eq!(library.selected, Some(first_id));
    assert!(!library.install_validation(old));
    let current = snapshot(&mut library);
    assert!(library.install_validation(current));
    assert!(library.validation_current().is_some());
    library.edit(first_json);
    assert!(library.validation_current().is_none());
}

#[test]
fn versioned_library_round_trips_and_refuses_invalid_or_duplicate_ids() {
    let dir = tempfile::tempdir().unwrap();
    let definition = definition("Example");
    save_library(dir.path(), &[definition.clone()]).unwrap();
    assert_eq!(load_library(dir.path()).unwrap(), vec![definition.clone()]);
    let before = fs::read(dir.path().join(".chasm").join(LIBRARY_FILE)).unwrap();
    assert_eq!(
        save_library(dir.path(), &[definition.clone(), definition]),
        Err(StorageError::Invalid)
    );
    assert_eq!(
        fs::read(dir.path().join(".chasm").join(LIBRARY_FILE)).unwrap(),
        before
    );
    assert!(matches!(
        decode_library(br#"{"version":99,"macros":[]}"#),
        Err(StorageError::Invalid)
    ));
}

#[test]
fn legacy_migration_is_inert_and_never_changes_the_source() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join(".chasm");
    fs::create_dir(&folder).unwrap();
    let source = folder.join(LEGACY_FILE);
    let original = br#"[{"name":"old note","body":"ordinary historical text"}]"#;
    fs::write(&source, original).unwrap();
    let migrated = load_library(dir.path()).unwrap();
    assert_eq!(fs::read(&source).unwrap(), original);
    assert_eq!(migrated.len(), 1);
    assert!(migrated[0].steps.is_empty());
    assert_eq!(migrated[0].legacy_notes[0].body, "ordinary historical text");
    assert!(migrated[0].validate().is_err());
    assert_eq!(load_library(dir.path()).unwrap(), migrated);
}

#[test]
fn unsafe_legacy_or_corrupt_library_is_not_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join(".chasm");
    fs::create_dir(&folder).unwrap();
    let legacy = folder.join(LEGACY_FILE);
    let unsafe_bytes = br#"[{"name":"old note","body":"password: disposable fixture"}]"#;
    fs::write(&legacy, unsafe_bytes).unwrap();
    assert!(matches!(
        load_library(dir.path()),
        Err(StorageError::UnsafeLegacy)
    ));
    assert_eq!(fs::read(&legacy).unwrap(), unsafe_bytes);
    assert!(!folder.join(LIBRARY_FILE).exists());

    let damaged = br#"{"version":1,"macros":["#;
    fs::write(folder.join(LIBRARY_FILE), damaged).unwrap();
    assert!(matches!(
        load_library(dir.path()),
        Err(StorageError::Invalid)
    ));
    assert_eq!(fs::read(folder.join(LIBRARY_FILE)).unwrap(), damaged);
    assert_eq!(fs::read(&legacy).unwrap(), unsafe_bytes);
}

#[test]
fn failed_save_leaves_existing_destination_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join(".chasm");
    fs::create_dir(&folder).unwrap();
    fs::create_dir(folder.join(LIBRARY_FILE)).unwrap();
    assert_eq!(
        save_library(dir.path(), &[definition("Example")]),
        Err(StorageError::Unavailable)
    );
    assert!(folder.join(LIBRARY_FILE).is_dir());
}

fn test_controller(root: &Path) -> Arc<Controller> {
    let mut state = DesktopState::new(root.to_path_buf(), 1.0);
    state.workspace_epoch = 11;
    state.network_epoch = 21;
    let state = Arc::new(Mutex::new(state));
    let runtime = tokio::runtime::Handle::current();
    Arc::new(Controller::new(state, runtime))
}

fn run_definition() -> MacroDefinition {
    let mut definition = definition("Ordered");
    definition.steps = vec![
        macro_model::MacroStep {
            id: macro_model::StepId("first".into()),
            enabled: true,
            kind: StepKind::Delay { millis: 1 },
        },
        macro_model::MacroStep {
            id: macro_model::StepId("disabled".into()),
            enabled: false,
            kind: StepKind::Delay { millis: 2 },
        },
        macro_model::MacroStep {
            id: macro_model::StepId("last".into()),
            enabled: true,
            kind: StepKind::Assert {
                left: "${var.check}".into(),
                comparator: macro_model::AssertComparator::True,
                right: None,
            },
        },
    ];
    definition
        .variables
        .insert("check".into(), serde_json::json!(true));
    definition
}

fn active_run(definition: MacroDefinition, token: ExecutionToken) -> ActiveRun {
    ActiveRun {
        id: RunId::new(),
        macro_id: definition.id.clone(),
        json: encode_definition(&definition).unwrap(),
        revision: 4,
        workspace_epoch: token.workspace_epoch,
        network_epoch: token.network_epoch,
        token,
        owned_node: super::super::owned_node::OwnedNode::from_child_handle(0, 0),
        definition,
        dry_run: false,
        cancel: CancellationToken::new(),
        started: Instant::now(),
    }
}

fn output_for(active: &ActiveRun) -> ExecutionOutput {
    let steps = active
        .definition
        .steps
        .iter()
        .filter(|step| step.enabled)
        .enumerate()
        .map(|(index, step)| {
            let kind = match &step.kind {
                StepKind::Delay { .. } => macro_runner::StepKind::Delay,
                StepKind::Assert { .. } => macro_runner::StepKind::Assert,
                _ => unreachable!(),
            };
            macro_runner::StepReport {
                id: macro_runner::StepId(index as u64 + 1),
                kind,
                status: StepStatus::Succeeded,
                failure: None,
                unknown: None,
                skipped: None,
                output: macro_runner::OutputSummary::default(),
            }
        })
        .collect();
    ExecutionOutput {
        report: RunReport {
            id: macro_runner::RunId(1),
            token: active.token.clone(),
            dry_run: active.dry_run,
            status: macro_runner::RunStatus::Completed,
            failure: None,
            steps,
        },
        step_ids: active
            .definition
            .steps
            .iter()
            .filter(|step| step.enabled)
            .map(|step| step.id.clone())
            .collect(),
    }
}

#[test]
fn runner_results_require_exact_order_and_context() {
    let token = ExecutionToken {
        workspace_epoch: 11,
        network_epoch: 21,
        rpc_token: 7,
    };
    let run = active_run(run_definition(), token);
    let mut output = output_for(&run);
    assert!(valid_output(&run, &output));
    output.step_ids.swap(0, 1);
    assert!(!valid_output(&run, &output));
    output.step_ids.swap(0, 1);
    output.report.token.network_epoch += 1;
    assert!(!valid_output(&run, &output));
    output.report.token.network_epoch -= 1;
    output.report.steps[1].status = StepStatus::Running;
    assert!(!valid_output(&run, &output));
}

#[test]
fn unsafe_adapter_output_does_not_enter_run_history() {
    let token = ExecutionToken {
        workspace_epoch: 11,
        network_epoch: 21,
        rpc_token: 7,
    };
    let run = active_run(run_definition(), token);
    let mut output = output_for(&run);
    output.report.steps[0].output.transaction_hash = Some("password: disposable fixture".into());
    assert!(!valid_output(&run, &output));
}

#[tokio::test]
async fn stale_completion_does_not_clear_the_active_run_or_publish_old_results() {
    let dir = tempfile::tempdir().unwrap();
    let controller = test_controller(dir.path());
    let token = ExecutionToken {
        workspace_epoch: 11,
        network_epoch: 21,
        rpc_token: 7,
    };
    let run = active_run(run_definition(), token);
    let id = run.id.clone();
    let output = output_for(&run);
    {
        let mut library = controller.library.lock().unwrap();
        library.records = vec![Record {
            definition: run.definition.clone(),
            draft: run.json.clone(),
        }];
        library.selected = Some(run.macro_id.clone());
        library.revision = run.revision;
        library.active_run = Some(run);
    }
    {
        let library = controller.library.lock().unwrap();
        let run = library.active_run.as_ref().unwrap();
        assert!(completion_is_current(&library, run, 11, 21, true));
        assert!(!completion_is_current(&library, run, 11, 22, true));
        assert!(!completion_is_current(&library, run, 11, 21, false));
    }
    finish_run(&controller, &RunId::new(), Err(ExecutionFailure::Failed));
    assert!(controller.library.lock().unwrap().active_run.is_some());
    controller.state.lock().unwrap().network_epoch += 1;
    finish_run(&controller, &id, Ok(output));
    let library = controller.library.lock().unwrap();
    assert!(library.active_run.is_none());
    assert!(library.runs.is_empty());
}

#[tokio::test]
async fn failed_save_keeps_the_committed_library_in_memory() {
    let dir = tempfile::tempdir().unwrap();
    let controller = test_controller(dir.path());
    let original = record(definition("Original"));
    let mut library = controller.library.lock().unwrap();
    library.records.push(original.clone());
    let folder = dir.path().join(".chasm");
    fs::create_dir(&folder).unwrap();
    fs::create_dir(folder.join(LIBRARY_FILE)).unwrap();
    let candidate = vec![record(definition("Replacement"))];
    assert!(!commit_records(&controller, &mut library, candidate));
    assert_eq!(library.records[0].definition, original.definition);
}

#[test]
fn multiple_hosts_share_drafts_validation_and_legacy_new_action() {
    i_slint_backend_testing::init_no_event_loop();
    let dir = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let state = Arc::new(Mutex::new(DesktopState::new(dir.path().to_path_buf(), 1.0)));
    let main = MainWindow::new().unwrap();
    let floating = MainWindow::new().unwrap();
    wire(&main, &state, runtime.handle().clone());
    let controller = controller_for(&state).unwrap();
    wire(&floating, &state, runtime.handle().clone());
    assert!(Arc::ptr_eq(&controller, &controller_for(&state).unwrap()));

    main.global::<MacroLogic>().invoke_new_macro();
    assert_eq!(main.global::<MacroData>().get_macros().row_count(), 1);
    assert_eq!(floating.global::<MacroData>().get_macros().row_count(), 1);
    assert_eq!(
        main.global::<MacroData>().get_selected_id(),
        floating.global::<MacroData>().get_selected_id()
    );
    floating.global::<MacroLogic>().invoke_validate();
    assert!(main.global::<MacroData>().get_validation_ready());
    assert!(floating.global::<MacroData>().get_validation_ready());
    assert!(!main.global::<MacroData>().get_can_run());
    assert!(!floating.global::<MacroData>().get_can_dry_run());

    let edited = format!("{} ", main.global::<MacroData>().get_definition_json());
    main.global::<MacroLogic>()
        .invoke_edited(edited.clone().into());
    assert!(!main.global::<MacroData>().get_validation_ready());
    assert!(!floating.global::<MacroData>().get_validation_ready());
    assert_eq!(
        floating
            .global::<MacroData>()
            .get_definition_json()
            .as_str(),
        edited
    );
    floating.global::<MacroLogic>().invoke_validate();
    assert!(main.global::<MacroData>().get_validation_ready());
    floating.global::<Logic>().invoke_new_macro();
    assert_eq!(main.global::<MacroData>().get_macros().row_count(), 2);
    assert_eq!(floating.global::<MacroData>().get_macros().row_count(), 2);
    assert!(!main.global::<MacroData>().get_validation_ready());

    detach_host(&floating);
    floating.global::<MacroLogic>().invoke_new_macro();
    assert_eq!(main.global::<MacroData>().get_macros().row_count(), 2);
    shutdown(&state);
}
