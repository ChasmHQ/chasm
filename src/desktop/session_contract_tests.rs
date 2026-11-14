use super::*;
use crate::desktop::call_result::{CallResult, ResultContext, TraceState};

fn bank_state() -> DesktopState {
    let raw = json!({"type":"compile_success","contracts":[{"name":"Bank","artifact":{
        "bytecode":{"object":"6000"},
        "abi":[
            {"type":"constructor","inputs":[{"name":"limit","type":"uint256"}],"stateMutability":"nonpayable"},
            {"type":"function","name":"read","inputs":[],"outputs":[],"stateMutability":"view"},
            {"type":"function","name":"write","inputs":[{"name":"amount","type":"uint256"}],"outputs":[],"stateMutability":"nonpayable"}
        ]
    }}]});
    let mut state = DesktopState::new(PathBuf::from("."), 1.0);
    state.contracts = contracts_from_compile(&raw.to_string()).unwrap();
    state
}

fn result(address: &str) -> CallResult {
    CallResult {
        context: ResultContext {
            contract: "Bank".into(),
            address: address.into(),
            signature: "read()".into(),
            request: json!({}),
            workspace_epoch: 1,
            network_epoch: 1,
            source: json!({}),
        },
        raw: json!("0x"),
        abi: Default::default(),
        simple: "result A".into(),
        trace: TraceState::Empty,
    }
}

#[test]
fn deployment_completion_keeps_two_banks_and_owner_drafts_separate() {
    let mut state = bank_state();
    state.select_contract(0);
    let a = state.current_document_id().unwrap();
    assert!(state.edit_constructor_argument(0, "11".into()));
    state.edit_deployment_value("0".into());
    state.current_contract_view_mut().unwrap().arguments = "draft A".into();
    state.current_contract_view_mut().unwrap().slot = "5".into();
    state.current_contract_view_mut().unwrap().storage = "storage A".into();
    assert!(state.select_document_function(&a, 0));
    assert!(state.open_deploy_for_document(&a));
    let deploy_a = state.current_document_id().unwrap();
    assert_eq!(
        state.deployment_origin_document().as_deref(),
        Some(a.as_str())
    );
    let token = state.begin_request(Operation::Deploy, "deploy A", false);
    state.open_wallet_doc();
    let active = state.current_document_id().unwrap();
    let bound_a = state
        .complete_deployment(
            &a,
            "Bank",
            "0x1111111111111111111111111111111111111111",
            &token,
        )
        .unwrap()
        .unwrap();
    assert_eq!(bound_a, a);
    assert_eq!(
        state.current_document_id().as_deref(),
        Some(active.as_str())
    );
    let token = state.begin_request(Operation::Deploy, "deploy B", false);
    let b = state
        .complete_deployment(
            &a,
            "Bank",
            "0x2222222222222222222222222222222222222222",
            &token,
        )
        .unwrap()
        .unwrap();
    let ai = state.documents.iter().position(|doc| doc.id == a).unwrap();
    let bi = state.documents.iter().position(|doc| doc.id == b).unwrap();
    state.documents[ai].contract_view.result =
        Some(result("0x1111111111111111111111111111111111111111"));
    assert_eq!(state.documents[ai].title, "Bank");
    assert_eq!(state.documents[bi].title, "Bank 2");
    assert_eq!(state.documents[bi].contract_name(), Some("Bank"));
    assert_ne!(
        state.documents[ai].contract_view.target,
        state.documents[bi].contract_view.target
    );
    state.select_document(bi as i32);
    assert_eq!(state.selected_contract.as_deref(), Some("Bank"));
    assert!(state.edit_constructor_argument(0, "22".into()));
    state.current_contract_view_mut().unwrap().arguments = "draft B".into();
    state.open_deploy_for_document(&b);
    let deploy_b = state.current_document_id().unwrap();
    assert_ne!(deploy_a, deploy_b);
    assert_eq!(
        state.deployment_draft_for_document(&deploy_a).values,
        vec!["11"]
    );
    assert_eq!(
        state.deployment_draft_for_document(&deploy_b).values,
        vec!["22"]
    );
    state.select_document(ai as i32);
    for section in 0..=2 {
        state.current_contract_view_mut().unwrap().section = section;
        state.select_document(bi as i32);
        assert_eq!(state.current_contract_view().unwrap().arguments, "draft B");
        state.select_document(ai as i32);
        let view = state.current_contract_view().unwrap();
        assert_eq!(view.arguments, "draft A");
        assert_eq!(view.slot, "5");
        assert_eq!(view.storage, "storage A");
        assert_eq!(view.result.as_ref().unwrap().simple, "result A");
        assert_eq!(view.selected_function, 0);
    }
    let a_id = state.deployment_id(0).unwrap();
    let b_id = state.deployment_id(1).unwrap();
    assert!(state.select_deployment_by_id(&b_id));
    assert_eq!(state.current_document_id().as_deref(), Some(b.as_str()));
    assert!(state.select_deployment_by_id(&a_id));
    assert_eq!(state.current_document_id().as_deref(), Some(a.as_str()));
    assert!(state.remove_deployment(0));
    let target_a = state.documents[ai].contract_view.target.as_ref().unwrap();
    assert_eq!(
        target_a.address,
        "0x1111111111111111111111111111111111111111"
    );
    assert!(target_a.deployment_id.is_none());
    assert_eq!(state.deployment_id(0).as_deref(), Some(b_id.as_str()));
    assert!(state.select_deployment_by_id(&b_id));
    assert_eq!(state.current_document_id().as_deref(), Some(b.as_str()));
}

#[test]
fn selection_recompile_tracks_signature_or_clears_it_per_document() {
    let mut state = bank_state();
    state.select_contract(0);
    let a = state.current_document_id().unwrap();
    assert!(state.select_document_function(&a, 1));
    state.current_contract_view_mut().unwrap().arguments = "17".into();
    state.deployments.push(Deployment {
        name: "Bank".into(),
        address: "0x2222222222222222222222222222222222222222".into(),
    });
    state.select_deployment(0);
    let b = state.current_document_id().unwrap();
    assert!(state.select_document_function(&b, 0));
    let mut reordered = state.contracts.clone();
    reordered[0].functions.swap(0, 1);
    state.replace_compiled_contracts(reordered.clone());
    let ai = state.documents.iter().position(|doc| doc.id == a).unwrap();
    let bi = state.documents.iter().position(|doc| doc.id == b).unwrap();
    assert_eq!(state.documents[ai].contract_view.selected_function, 0);
    assert_eq!(state.documents[bi].contract_view.selected_function, 1);
    assert_eq!(state.selected_function, 1);
    assert_eq!(state.documents[ai].contract_view.arguments, "17");
    reordered[0].functions[0].params[0].1 = "address".into();
    state.replace_compiled_contracts(reordered);
    assert_eq!(state.documents[ai].contract_view.selected_function, -1);
    assert_eq!(state.documents[bi].contract_view.selected_function, 1);
    assert!(state.notice.contains("Cleared 1"));
    state.select_document(ai as i32);
    assert_eq!(state.selected_function, -1);
    assert_eq!(state.current_contract_view().unwrap().arguments, "17");
    assert!(!state.select_document_function(&a, 999));
    assert_eq!(state.selected_function, -1);
}

#[test]
fn invalid_abi_is_counted_without_discarding_valid_artifacts() {
    let raw = json!({"type":"compile_success","contracts":[
        {"name":"Good","artifact":{"abi":[]}},
        {"name":"BadJson","artifact":{"abi":null}},
        {"name":"BadFunction","artifact":{"abi":[{"type":"function","name":"f","inputs":[{"name":"x","type":"not_a_type"}],"outputs":[]}]}},
        {"name":"BadShape","artifact":{"abi":[{"type":"function","name":"f","inputs":[]} ]}}
    ]}).to_string();
    let report = contracts_from_compile_report(&raw).unwrap();
    assert_eq!(report.contracts.len(), 1);
    assert_eq!(report.contracts[0].name, "Good");
    assert_eq!(report.skipped, 3);
    assert!(report.warning().unwrap().contains("3 artifact"));
    assert!(contracts_from_compile(&raw).is_err());
}

#[test]
fn tool_activity_documents_keep_stable_owners_and_contract_context() {
    use crate::desktop::layout::{Geometry, PaneId, PaneKind, Placement, Zone};

    let mut state = bank_state();
    state.select_contract(0);
    let bank_id = state.current_document_id().unwrap();
    assert!(state.set_contract_address("0x1111111111111111111111111111111111111111".into()));
    state.current_contract_view_mut().unwrap().arguments = "bank draft".into();
    assert!(state.select_document_function(&bank_id, 1));
    let bank_target = state.current_contract_view().unwrap().target.clone();
    state.open_wallet_doc();
    state.select_document(0);

    for (activity, kind, title) in [
        (1, 4, "Chain Explorer"),
        (2, 5, "Converter"),
        (3, 6, "Macros"),
    ] {
        state.open_activity(activity);
        assert_eq!(state.activity, activity);
        assert_eq!(state.current_doc_kind(), kind);
        assert_eq!(state.selected_contract.as_deref(), Some("Bank"));
        let owner = state.current_document_id().unwrap();
        assert_eq!(state.documents[state.active_document as usize].title, title);
        let pane = PaneId::new(PaneKind::Document, Some(owner.clone()));
        state.layout.ensure(pane.clone(), Zone::Center).unwrap();
        state
            .layout
            .float(
                &pane,
                Geometry {
                    x: 100,
                    y: 100,
                    width: 800,
                    height: 600,
                    maximized: false,
                },
            )
            .unwrap();
        assert!(matches!(
            state.layout.placement(&pane),
            Some(Placement::Floating { .. })
        ));
        state.open_activity(0);
        assert_eq!(
            state.current_document_id().as_deref(),
            Some(bank_id.as_str())
        );
        state.open_activity(activity);
        assert_eq!(state.current_document_id().as_deref(), Some(owner.as_str()));
        assert_eq!(
            state
                .documents
                .iter()
                .filter(|doc| doc.kind == kind)
                .count(),
            1
        );
        assert!(matches!(
            state.layout.placement(&pane),
            Some(Placement::Floating { .. })
        ));
    }

    state.select_document(0);
    assert_eq!(state.activity, 0);
    assert_eq!(state.selected_contract.as_deref(), Some("Bank"));
    assert_eq!(state.current_contract_view().unwrap().target, bank_target);
    assert_eq!(
        state.current_contract_view().unwrap().arguments,
        "bank draft"
    );
    assert_eq!(state.selected_function, 1);

    for (kind, activity) in [(4, 1), (5, 2), (6, 3)] {
        let index = state
            .documents
            .iter()
            .position(|doc| doc.kind == kind)
            .unwrap();
        state.select_document(index as i32);
        assert_eq!(state.activity, activity);
        assert_eq!(state.selected_contract.as_deref(), Some("Bank"));
    }
}

#[test]
fn tool_document_reuse_keeps_drafts_after_close_and_reopen() {
    let mut state = bank_state();
    state.select_contract(0);
    state.open_activity(2);
    let original_converter = state.current_document_id().unwrap();
    state.convert(" 17 ");
    let output = state.converter_output.clone();
    state.open_activity(1);
    state.explorer_input = " 0x1234 ".into();
    state.explorer_result = "retained lookup".into();
    state.open_activity(3);
    let macros = state.current_document_id().unwrap();
    state.new_macro();
    assert_eq!(
        state.current_document_id().as_deref(),
        Some(macros.as_str())
    );
    assert_eq!(state.macros.len(), 1);
    state.open_activity(2);
    assert_eq!(
        state.current_document_id().as_deref(),
        Some(original_converter.as_str())
    );
    assert_eq!(state.converter_input, " 17 ");
    assert_eq!(state.converter_output, output);
    state.close_document(state.active_document);
    state.open_activity(2);
    assert_ne!(
        state.current_document_id().as_deref(),
        Some(original_converter.as_str())
    );
    assert_eq!(state.converter_input, " 17 ");
    assert_eq!(state.converter_output, output);
    state.open_activity(1);
    assert_eq!(state.explorer_input, " 0x1234 ");
    assert_eq!(state.explorer_result, "retained lookup");
}

#[test]
fn stale_network_and_confirmation_do_not_rebind_or_edit_targets() {
    let mut state = bank_state();
    state.select_contract(0);
    let owner = state.current_document_id().unwrap();
    let token = state.begin_request(Operation::Deploy, "deploy", false);
    state.network_epoch += 1;
    assert!(state
        .complete_deployment(
            &owner,
            "Bank",
            "0x1111111111111111111111111111111111111111",
            &token
        )
        .is_err());
    assert!(state.deployments.is_empty());
    state.network_epoch -= 1;
    state
        .complete_deployment(
            &owner,
            "Bank",
            "0x1111111111111111111111111111111111111111",
            &token,
        )
        .unwrap();
    let id = state.deployment_id(0).unwrap();
    state.network_epoch += 1;
    assert_eq!(state.deployment_id(0).as_deref(), Some(id.as_str()));
    assert!(!state.select_deployment_by_id(&id));
    state.security_confirmation = true;
    assert!(state.transition_pending());
    assert!(!state.can_change_workspace());
    assert!(!state.set_contract_address("0x2222222222222222222222222222222222222222".into()));
    state.security_confirmation = false;
    state.folder_picker_busy = true;
    assert!(state.transition_pending());
    assert!(!state.can_change_workspace());
    state.folder_picker_busy = false;
    assert!(state.set_contract_address("0x2222222222222222222222222222222222222222".into()));
    assert_eq!(
        state
            .current_contract_view()
            .unwrap()
            .target
            .as_ref()
            .unwrap()
            .network_epoch,
        token.network_epoch
    );
}

#[test]
fn revert_drops_post_snapshot_deployments_and_rebinds_survivors() {
    const OLD: &str = "0x1111111111111111111111111111111111111111";
    const NEW: &str = "0x2222222222222222222222222222222222222222";
    let mut state = bank_state();
    state.select_contract(0);
    let a = state.current_document_id().unwrap();
    let token = state.begin_request(Operation::Deploy, "deploy old", false);
    state.complete_deployment(&a, "Bank", OLD, &token).unwrap();
    state.record_snapshot("0x5".into());
    let token = state.begin_request(Operation::Deploy, "deploy new", false);
    let b = state.complete_deployment(&a, "Bank", NEW, &token).unwrap().unwrap();
    let before = state.network_epoch;

    state.apply_revert();

    assert_eq!(state.network_epoch, before + 1);
    assert!(state.snapshot_id.is_empty(), "Anvil consumes the snapshot");
    let addresses: Vec<_> = state.deployments.iter().map(|d| d.address.as_str()).collect();
    assert_eq!(addresses, [OLD], "the post-snapshot contract no longer exists");
    let target = |state: &DesktopState, id: &str| {
        let doc = state.documents.iter().find(|doc| doc.id == id).unwrap();
        doc.contract_view.target.clone().unwrap()
    };
    let survivor = target(&state, &a);
    assert_eq!(survivor.network_epoch, state.network_epoch);
    assert_eq!(survivor.deployment_id, state.deployment_id(0));
    assert_ne!(
        target(&state, &b).network_epoch,
        state.network_epoch,
        "a document for a reverted deployment must not reach the chain"
    );
    let id = state.deployment_id(0).unwrap();
    assert!(state.select_deployment_by_id(&id));
    // The reverted address can be deployed again in the new context.
    let token = state.begin_request(Operation::Deploy, "redeploy", false);
    assert!(state.complete_deployment(&a, "Bank", NEW, &token).is_ok());
}
