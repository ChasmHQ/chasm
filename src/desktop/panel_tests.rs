use super::*;

fn fixture() -> DesktopState {
    let mut state = DesktopState::welcome();
    state.workspace_ready = true;
    state.documents.push(Document::new("Bank A".into(), String::new(), 0));
    state.documents.push(Document::new("Bank B".into(), String::new(), 0));
    state.active_document = 1;
    state
}

fn geometry() -> Geometry {
    Geometry { x: -1700, y: -150, width: 900, height: 600, maximized: false }
}

#[test]
fn exact_owner_and_orphan_saved_panes_never_fall_back() {
    let mut state = fixture();
    let first = state.documents[0].id.clone();
    let id = PaneId::new(PaneKind::CallResult, Some(first.clone()));
    state.layout.ensure(id.clone(), Zone::Right).unwrap();
    assert_eq!(resolve(&state, &id.stable_key()), Some(id.clone()));
    assert!(resolve(&state, "call_result:document-missing").is_none());
    state.documents.remove(0);
    assert!(resolve(&state, &id.stable_key()).is_none());
    let mut saved = state.layout.clone();
    retain_live_owners(&mut saved, &state);
    assert!(saved.placement(&id).is_none());
    assert!(saved.placement(&PaneId::new(PaneKind::Console, None)).is_some());
    saved.validate().unwrap();
}

#[test]
fn opening_foundry_restores_hidden_document_pane() {
    let mut state = fixture();
    let generic = PaneId::new(PaneKind::Document, None);
    state.layout.hide(&generic).unwrap();
    state.open_foundry_build();
    focus_document(&mut state.layout, &generic);
    assert_eq!(state.documents[state.active_document as usize].kind, 7);
    assert_eq!(state.layout.placement(&generic), Some(&Placement::Docked { zone: Zone::Center }));
    assert_eq!(state.layout.active.get(&Zone::Center), Some(&generic.stable_key()));
    assert_eq!(zone_projection(&state, Zone::Center).document_kind, 7);
}

#[test]
fn generic_document_placeholder_does_not_duplicate_floating_owner() {
    let mut state = fixture();
    state.activity = 0;
    let generic = PaneId::new(PaneKind::Document, None);
    let first = PaneId::new(PaneKind::Document, Some(state.documents[0].id.clone()));
    let second = PaneId::new(PaneKind::Document, Some(state.documents[1].id.clone()));
    state.layout = prepare_float(&state.layout, &second, geometry()).unwrap();
    assert!(!displayed(&state, &generic));
    state.layout.hide(&second).unwrap();
    assert!(!displayed(&state, &generic));
    state.active_document = 0;
    assert!(displayed(&state, &generic));
    assert_ne!(first, second);
    state.documents.push(Document::new("Converter".into(), String::new(), 5));
    state.activity = 2;
    state.active_document = 2;
    assert!(displayed(&state, &generic));
    assert_eq!(projection(&state, Some(&generic)).3, 5);
}

#[test]
fn per_owner_presentation_uses_the_requested_document() {
    let mut state = fixture();
    state.documents[0].contract_view.storage = "first storage".into();
    state.documents[1].contract_view.storage = "second storage".into();
    let first = PaneId::new(PaneKind::Storage, Some(state.documents[0].id.clone()));
    let projected = projection(&state, Some(&first));
    assert_eq!(projected.2, state.documents[0].id);
    assert_eq!(projected.6.storage_output.as_str(), "first storage");
    assert_ne!(projected.6.storage_output.as_str(), "second storage");
    let missing = PaneId::new(PaneKind::Storage, Some("document-missing".into()));
    let projected = projection(&state, Some(&missing));
    assert!(projected.2.is_empty());
    assert_ne!(projected.6.storage_output.as_str(), "second storage");
}

#[test]
fn subpanel_returns_to_exact_bank_from_any_placement() {
    for kind in [PaneKind::CallResult, PaneKind::Storage] {
        for mode in 0..3 {
            let mut state = fixture();
            let owner = state.documents[0].id.clone();
            let other = state.documents[1].id.clone();
            let id = PaneId::new(kind, Some(owner.clone()));
            let unrelated = PaneId::new(kind, Some(other.clone()));
            state.layout.ensure(id.clone(), Zone::Right).unwrap();
            state.layout.ensure(unrelated.clone(), Zone::Bottom).unwrap();
            if mode == 1 {
                state.layout.float(&id, geometry()).unwrap();
            }
            if mode == 2 {
                state.layout.hide(&id).unwrap();
            }
            assert!(state.layout.placement(&id).is_some());
            let other_before = state.layout.placement(&unrelated).cloned();
            let other_section = state.documents[1].contract_view.section;
            apply_return_to_bank(&mut state, kind, &owner).unwrap();
            assert_eq!(state.current_document_id().as_deref(), Some(owner.as_str()));
            assert_eq!(state.documents[0].contract_view.section, if kind == PaneKind::Storage { 1 } else { 0 });
            assert_eq!(state.documents[1].contract_view.section, other_section);
            assert!(state.layout.placement(&id).is_none());
            assert_eq!(state.layout.placement(&unrelated).cloned(), other_before);
            let view = projection(&state, Some(&PaneId::new(PaneKind::Document, None))).6;
            if kind == PaneKind::Storage {
                assert!(!view.storage_detached);
            } else {
                assert!(!view.result_detached);
            }
            assert_eq!(state.layout.active.get(&Zone::Center), Some(&"document".to_owned()));
            state.layout.validate().unwrap();
            state.layout = prepare_float(&state.layout, &id, geometry()).unwrap();
            let redetached = projection(&state, Some(&PaneId::new(PaneKind::Document, None))).6;
            if kind == PaneKind::Storage {
                assert!(redetached.storage_detached);
            } else {
                assert!(redetached.result_detached);
            }
            apply_return_to_bank(&mut state, kind, &owner).unwrap();
            assert!(state.layout.placement(&id).is_none());
        }
    }
}

#[test]
fn subpanel_return_preserves_floating_bank_and_rejects_orphan() {
    let mut state = fixture();
    let owner = state.documents[0].id.clone();
    let bank = PaneId::new(PaneKind::Document, Some(owner.clone()));
    let storage = PaneId::new(PaneKind::Storage, Some(owner.clone()));
    state.layout = prepare_float(&state.layout, &bank, geometry()).unwrap();
    state.layout.ensure(storage.clone(), Zone::Right).unwrap();
    state.layout.float(&storage, geometry()).unwrap();
    let bank_before = state.layout.placement(&bank).cloned();
    let before = state.layout.clone();
    assert!(apply_return_to_bank(&mut state, PaneKind::Storage, "document-missing").is_err());
    assert_eq!(state.layout, before);
    assert_eq!(state.active_document, 1);
    state.security_confirmation = true;
    assert!(apply_return_to_bank(&mut state, PaneKind::Storage, &owner).is_err());
    assert_eq!(state.layout, before);
    assert_eq!(state.active_document, 1);
    state.security_confirmation = false;
    apply_return_to_bank(&mut state, PaneKind::Storage, &owner).unwrap();
    assert_eq!(state.layout.placement(&bank).cloned(), bank_before);
    assert!(state.layout.placement(&storage).is_none());
    assert_eq!(state.documents[0].contract_view.section, 1);
    assert_eq!(state.current_document_id().as_deref(), Some(owner.as_str()));
    assert!(!projection(&state, Some(&bank)).6.storage_detached);
    assert!(!displayed(&state, &PaneId::new(PaneKind::Document, None)));
    state.layout.validate().unwrap();
}

#[test]
fn prospective_layout_rejects_invalid_geometry_without_mutating_source() {
    let state = fixture();
    let before = state.layout.clone();
    let id = PaneId::new(PaneKind::Document, Some(state.documents[0].id.clone()));
    assert!(prepare_float(&state.layout, &id, Geometry { width: 1, ..geometry() }).is_err());
    assert_eq!(state.layout, before);
    let candidate = prepare_float(&state.layout, &id, geometry()).unwrap();
    assert_eq!(state.layout, before);
    assert!(matches!(candidate.placement(&id), Some(Placement::Floating { return_zone: Zone::Center, .. })));
    let generic = PaneId::new(PaneKind::Document, None);
    let mut moved = before.clone();
    moved.dock(&generic, Zone::Right).unwrap();
    let candidate = prepare_float(&moved, &id, geometry()).unwrap();
    assert!(matches!(candidate.placement(&id), Some(Placement::Floating { return_zone: Zone::Right, .. })));
    let mut redocked = candidate;
    redocked.close_float(&id).unwrap();
    merge_current_document(&mut redocked, &id).unwrap();
    assert!(redocked.placement(&id).is_none());
    assert_eq!(redocked.placement(&generic), Some(&Placement::Docked { zone: Zone::Right }));
    assert_eq!(redocked.active.get(&Zone::Right), Some(&generic.stable_key()));
    redocked.validate().unwrap();
}

#[test]
fn tabs_reorder_and_maximization_follow_exact_active_pane() {
    let mut layout = Layout::preset(Preset::Debugging);
    let code = PaneId::new(PaneKind::DebugCode, None);
    let doc = PaneId::new(PaneKind::Document, None);
    layout.maximized = Some(code.stable_key());
    layout.select(Zone::Center, &doc).unwrap();
    assert_eq!(layout.maximized, None);
    layout.reorder(&doc, Zone::Center, 0).unwrap();
    let center = layout
        .panes
        .iter()
        .filter(|pane| pane.placement == Placement::Docked { zone: Zone::Center })
        .map(|pane| pane.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(center, vec![doc.clone(), code]);
    let before = layout.clone();
    assert!(layout.reorder(&doc, Zone::Center, 5).is_err());
    assert_eq!(layout, before);
    layout.maximized = Some(doc.stable_key());
    layout.validate().unwrap();
}

#[test]
fn autosave_waits_for_stable_changes_and_deduplicates_success() {
    let mut autosave = LayoutAutosave::default();
    let start = Instant::now();
    let first = serde_json::json!({"left_width": 240});
    let changed = serde_json::json!({"left_width": 260});
    assert!(!autosave.observe(&first, start));
    assert!(!autosave.observe(&first, start + Duration::from_millis(500)));
    assert!(!autosave.observe(&changed, start + Duration::from_millis(500)));
    assert!(!autosave.observe(&changed, start + Duration::from_millis(1249)));
    assert!(autosave.observe(&changed, start + Duration::from_millis(1250)));
    autosave.saved(changed.clone());
    assert!(!autosave.observe(&changed, start + Duration::from_secs(3)));
    assert!(!autosave.observe(&first, start + Duration::from_secs(4)));
    autosave.suspend();
    assert!(!autosave.observe(&first, start + Duration::from_secs(20)));
    assert!(autosave.observe(&first, start + Duration::from_millis(20_750)));
    autosave.last_failure = Some(start + Duration::from_millis(20_750));
    assert!(!autosave.observe(&first, start + Duration::from_secs(21)));
    assert!(autosave.observe(&first, start + Duration::from_millis(21_500)));
}

#[test]
fn monitor_clamp_handles_negative_offsets_and_high_dpi() {
    let work = super::super::platform::WorkArea { left: -1920, top: -200, right: 0, bottom: 880 };
    let input = Geometry { x: -2500, y: -500, width: 2000, height: 1300, maximized: false };
    let result = super::super::platform::clamp_panel_to_work_area(input, 1.5, work).unwrap();
    assert_eq!((result.x, result.y, result.width, result.height), (-1920, -200, 1280, 720));
    let small = super::super::platform::WorkArea { left: 0, top: 0, right: 200, bottom: 100 };
    assert!(super::super::platform::clamp_panel_to_work_area(input, 1.0, small).is_none());
    assert!(super::super::platform::clamp_panel_to_work_area(input, f32::NAN, work).is_none());
}
