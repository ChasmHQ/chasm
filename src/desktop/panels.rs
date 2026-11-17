use super::investigation::Investigation;
use super::layout::{Geometry, Layout, PaneId, PaneKind, Placement, Preset, Zone};
use super::session::{self, DesktopState, Document};
use super::{AppData, ContractLogic, MainWindow, PanelData, PanelLogic, PanelRow};
use slint::{ComponentHandle, Model, Weak};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const AUTOSAVE_POLL: Duration = Duration::from_millis(250);
const AUTOSAVE_DELAY: Duration = Duration::from_millis(750);

type State = Arc<Mutex<DesktopState>>;
type Hosts = Rc<RefCell<Registry>>;

struct Floating {
    ui: MainWindow,
    id: PaneId,
}
struct Registry {
    main: Weak<MainWindow>,
    state: State,
    runtime: tokio::runtime::Handle,
    investigation: Arc<Investigation>,
    windows: HashMap<String, Floating>,
    geometries: HashMap<String, Geometry>,
    workspace_epoch: u64,
    last_focus: Option<(i32, Option<String>)>,
    autosave_timer: slint::Timer,
    autosave: LayoutAutosave,
}

#[derive(Default)]
struct LayoutAutosave {
    observed: Option<serde_json::Value>,
    unchanged_since: Option<Instant>,
    persisted: Option<serde_json::Value>,
    last_failure: Option<Instant>,
    /// Set when a rejected saved layout could not be backed up.
    disabled: bool,
}

impl LayoutAutosave {
    fn observe(&mut self, snapshot: &serde_json::Value, now: Instant) -> bool {
        if self.disabled {
            return false;
        }
        if self.observed.as_ref() != Some(snapshot) {
            self.observed = Some(snapshot.clone());
            self.unchanged_since = Some(now);
            self.last_failure = None;
            return false;
        }
        self.persisted.as_ref() != Some(snapshot)
            && self.unchanged_since.is_some_and(|since| now.saturating_duration_since(since) >= AUTOSAVE_DELAY)
            && self.last_failure.is_none_or(|since| now.saturating_duration_since(since) >= AUTOSAVE_DELAY)
    }

    fn suspend(&mut self) {
        self.observed = None;
        self.unchanged_since = None;
        self.last_failure = None;
    }

    fn saved(&mut self, snapshot: serde_json::Value) {
        self.persisted = Some(snapshot);
        self.last_failure = None;
    }
}
thread_local! { static HOSTS: RefCell<Option<Hosts>> = const { RefCell::new(None) }; }

fn blocked(state: &DesktopState) -> bool {
    state.transition_pending() || state.folder_picker_busy || state.security_confirmation
}
fn unavailable(state: &DesktopState, id: &PaneId) -> bool {
    blocked(state) || (!state.workspace_ready && id.kind != PaneKind::Investigation)
}

pub fn bind(ui: &MainWindow, state: &State, runtime: tokio::runtime::Handle, investigation: Arc<Investigation>) {
    shutdown();
    let epoch = state.lock().unwrap().workspace_epoch;
    let registry = Rc::new(RefCell::new(Registry {
        main: ui.as_weak(),
        state: state.clone(),
        runtime,
        investigation,
        windows: HashMap::new(),
        geometries: HashMap::new(),
        workspace_epoch: epoch,
        last_focus: None,
        autosave_timer: slint::Timer::default(),
        autosave: LayoutAutosave::default(),
    }));
    HOSTS.with(|hosts| *hosts.borrow_mut() = Some(registry.clone()));
    wire_host(ui, &registry, None);

    let workspace = {
        let state = state.lock().unwrap();
        state.workspace_ready.then(|| state.workspace.clone())
    };
    if let Some(workspace) = workspace {
        let saved = std::fs::read_to_string(workspace.join(".chasm/desktop-settings.json"))
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .and_then(|value| value.get("layout").cloned());
        if let Some(saved) = saved {
            registry.borrow_mut().autosave.persisted = Some(saved.clone());
            match Layout::from_json(&saved) {
                Ok(mut layout) => {
                    let mut state = state.lock().unwrap();
                    retain_live_owners(&mut layout, &state);
                    // Layouts saved before the side Response pane existed gain it;
                    // one the user hid is stored as hidden and stays that way.
                    let response = PaneId::new(PaneKind::CallResult, None);
                    if layout.placement(&response).is_none() {
                        let _ = layout.ensure(response, Zone::Right);
                    }
                    state.layout = layout;
                }
                Err(_) => {
                    // Autosave will replace the rejected layout, so keep a copy first.
                    let settings = workspace.join(".chasm/desktop-settings.json");
                    let backup = workspace.join(".chasm/desktop-settings.rejected.json");
                    state.lock().unwrap().notice = match std::fs::copy(&settings, &backup) {
                        Ok(_) => "Saved layout was invalid. A safe default layout is in use; the previous settings were kept in .chasm/desktop-settings.rejected.json.".into(),
                        Err(_) => {
                            // Without a backup, do not overwrite the user's file.
                            registry.borrow_mut().autosave.disabled = true;
                            "Saved layout was invalid and could not be backed up. A safe default layout is in use and will not be saved this session.".into()
                        }
                    };
                }
            }
        }
    }

    // No saved floating placement is committed until the replacement window is shown.
    let floats = {
        let mut state = state.lock().unwrap();
        let floats = state
            .layout
            .panes
            .iter()
            .filter_map(|pane| match pane.placement {
                Placement::Floating { geometry, .. } => Some((pane.id.clone(), geometry)),
                _ => None,
            })
            .collect::<Vec<_>>();
        for (id, _) in &floats {
            let _ = state.layout.close_float(id);
        }
        floats
    };
    for (id, geometry) in floats {
        registry.borrow_mut().geometries.insert(id.stable_key(), geometry);
        float_panel(&registry, &id.stable_key());
    }
    refresh_all();
    let weak = Rc::downgrade(&registry);
    registry.borrow().autosave_timer.start(slint::TimerMode::Repeated, AUTOSAVE_POLL, move || {
        if let Some(registry) = weak.upgrade() {
            autosave_tick(&registry);
        }
    });
}

pub fn shutdown() {
    HOSTS.with(|hosts| {
        let old = hosts.borrow_mut().take();
        if let Some(registry) = old {
            registry.borrow().autosave_timer.stop();
            save_layout(&registry);
            let windows = registry.borrow_mut().windows.drain().map(|(_, window)| window).collect::<Vec<_>>();
            for window in windows {
                dispose(window);
            }
        }
    });
}

/// Updated by the main host when a local security confirmation is displayed or dismissed.
pub fn set_security_confirmation(value: bool) {
    let changed = HOSTS.with(|hosts| {
        let registry = hosts.borrow().clone();
        let Some(registry) = registry else { return false };
        let state = registry.borrow().state.clone();
        let mut state = state.lock().unwrap();
        if state.security_confirmation == value {
            return false;
        }
        state.security_confirmation = value;
        true
    });
    if changed {
        refresh_all();
    }
}

/// Open or restore a named non-document pane in its remembered zone.
pub fn open(key: &str) {
    HOSTS.with(|hosts| {
        if let Some(registry) = hosts.borrow().clone() {
            dock_panel(&registry, key, -1);
        }
    });
}

pub fn hide(key: &str) {
    HOSTS.with(|hosts| {
        if let Some(registry) = hosts.borrow().clone() {
            hide_panel(&registry, key);
        }
    });
}

/// Select a workspace pane only when navigation changes, not on ordinary repaints.
pub fn focus_current() {
    let registry = HOSTS.with(|hosts| hosts.borrow().clone());
    let Some(registry) = registry else { return };
    let state = registry.borrow().state.clone();
    let focus = {
        let state = state.lock().unwrap();
        if blocked(&state) || !state.workspace_ready {
            return;
        }
        (state.activity, state.current_document_id())
    };
    if registry.borrow().last_focus.as_ref() == Some(&focus) {
        return;
    }
    registry.borrow_mut().last_focus = Some(focus.clone());
    {
        let mut state = state.lock().unwrap();
        let kind = if focus.0 == 4 { PaneKind::DebugCode } else { PaneKind::Document };
        let owned = focus.1.as_ref().map(|owner| PaneId::new(PaneKind::Document, Some(owner.clone())));
        let id = if kind == PaneKind::Document {
            owned.filter(|id| state.layout.placement(id).is_some()).unwrap_or_else(|| PaneId::new(kind, None))
        } else {
            PaneId::new(kind, None)
        };
        match state.layout.placement(&id).cloned() {
            Some(Placement::Docked { zone }) => {
                let _ = state.layout.select(zone, &id);
            }
            Some(Placement::Hidden { .. }) if kind == PaneKind::Document => {
                focus_document(&mut state.layout, &id);
            }
            Some(Placement::Floating { .. } | Placement::Hidden { .. }) => {
                if let Some(Placement::Docked { zone }) = state.layout.placement(&PaneId::new(kind, None)).cloned() {
                    if state.layout.active.get(&zone).is_some_and(|key| key == &PaneId::new(kind, None).stable_key()) {
                        state.layout.maximized = None;
                    }
                }
            }
            None => {
                if state.layout.ensure(id.clone(), default_zone(kind)).is_ok() {
                    let _ = state.layout.select(default_zone(kind), &id);
                }
            }
        }
    }
    refresh_all();
}

fn focus_document(layout: &mut Layout, id: &PaneId) {
    if let Some(Placement::Hidden { return_zone }) = layout.placement(id).cloned() {
        if layout.ensure(id.clone(), return_zone).is_ok() {
            let _ = layout.select(return_zone, id);
        }
    }
}

fn host_matches(registry: &Hosts, host: Option<&PaneId>, id: &PaneId) -> bool {
    let key = id.stable_key();
    let registry = registry.borrow();
    match host {
        Some(host) => host == id && registry.windows.contains_key(&key),
        None => !registry.windows.contains_key(&key),
    }
}
fn originating_pane_is_hosted(ui: &MainWindow, owner: &str) -> bool {
    originating_kind_is_hosted(ui, owner, 1)
}
fn originating_kind_is_hosted(ui: &MainWindow, owner: &str, kind: i32) -> bool {
    if owner.is_empty() {
        return false;
    }
    if ui.get_panel_host() {
        return ui.get_host_pane() == kind && ui.get_host_owner().as_str() == owner;
    }
    let data = ui.global::<PanelData>();
    [
        (data.get_left_kind(), data.get_left_owner()),
        (data.get_center_kind(), data.get_center_owner()),
        (data.get_right_kind(), data.get_right_owner()),
        (data.get_bottom_kind(), data.get_bottom_owner()),
    ]
    .iter()
    .any(|(displayed_kind, displayed_owner)| *displayed_kind == kind && displayed_owner.as_str() == owner)
}

fn wire_host(ui: &MainWindow, registry: &Hosts, host: Option<PaneId>) {
    let weak = Rc::downgrade(registry);
    let host_id = host.clone();
    ui.global::<PanelLogic>().on_float(move |key| {
        if let Some(registry) = weak.upgrade() {
            let id = resolve(&registry.borrow().state.lock().unwrap(), key.as_str());
            if id.as_ref().is_some_and(|id| host_matches(&registry, host_id.as_ref(), id)) {
                float_panel(&registry, key.as_str());
            }
        }
    });
    let weak = Rc::downgrade(registry);
    let host_id = host.clone();
    ui.global::<PanelLogic>().on_dock(move |key, zone| {
        if let Some(registry) = weak.upgrade() {
            let id = resolve(&registry.borrow().state.lock().unwrap(), key.as_str());
            if id.as_ref().is_some_and(|id| host_matches(&registry, host_id.as_ref(), id)) {
                dock_panel(&registry, key.as_str(), zone);
            }
        }
    });
    let weak = Rc::downgrade(registry);
    let host_id = host.clone();
    ui.global::<PanelLogic>().on_hide(move |key| {
        if let Some(registry) = weak.upgrade() {
            let id = resolve(&registry.borrow().state.lock().unwrap(), key.as_str());
            if id.as_ref().is_some_and(|id| host_matches(&registry, host_id.as_ref(), id)) {
                hide_panel(&registry, key.as_str());
            }
        }
    });
    let weak = Rc::downgrade(registry);
    let host_id = host.clone();
    ui.global::<PanelLogic>().on_select(move |key, zone| {
        let Some(registry) = weak.upgrade() else { return };
        if host_id.is_some() {
            return;
        }
        let state = registry.borrow().state.clone();
        {
            let mut state = state.lock().unwrap();
            if blocked(&state) {
                return;
            }
            let (Some(id), Some(zone)) = (resolve(&state, key.as_str()), zone_from(zone)) else { return };
            if !host_matches(&registry, None, &id) || !displayed(&state, &id) || state.layout.select(zone, &id).is_err()
            {
                return;
            }
            if id.kind == PaneKind::Document {
                if let Some(index) = state.documents.iter().position(|doc| Some(&doc.id) == id.owner.as_ref()) {
                    state.select_document(index as i32);
                }
            }
        }
        refresh_all();
    });
    let weak = Rc::downgrade(registry);
    let host_id = host.clone();
    ui.global::<PanelLogic>().on_move(move |key, zone, index| {
        let Some(registry) = weak.upgrade() else { return };
        if host_id.is_some() {
            return;
        }
        let state = registry.borrow().state.clone();
        {
            let mut state = state.lock().unwrap();
            if blocked(&state) {
                return;
            }
            let (Some(id), Some(zone), Ok(index)) =
                (resolve(&state, key.as_str()), zone_from(zone), usize::try_from(index))
            else {
                return;
            };
            if !host_matches(&registry, None, &id) || !displayed(&state, &id) {
                return;
            }
            let ordered = state
                .layout
                .panes
                .iter()
                .filter(|pane| {
                    pane.id != id && pane.placement == Placement::Docked { zone } && displayed(&state, &pane.id)
                })
                .map(|pane| pane.id.clone())
                .collect::<Vec<_>>();
            if index > ordered.len() {
                return;
            }
            let actual = if let Some(target) = ordered.get(index) {
                state
                    .layout
                    .panes
                    .iter()
                    .filter(|pane| pane.id != id && pane.placement == Placement::Docked { zone })
                    .position(|pane| &pane.id == target)
                    .unwrap_or(ordered.len())
            } else {
                state
                    .layout
                    .panes
                    .iter()
                    .filter(|pane| pane.id != id && pane.placement == Placement::Docked { zone })
                    .count()
            };
            if state.layout.reorder(&id, zone, actual).is_err() {
                return;
            }
        }
        refresh_all();
    });
    let weak = Rc::downgrade(registry);
    let view = ui.as_weak();
    ui.global::<PanelLogic>().on_drag_drop(move |pane, x, y| {
        let (Some(registry), Some(ui)) = (weak.upgrade(), view.upgrade()) else { return };
        if ui.get_panel_host() || !x.is_finite() || !y.is_finite() {
            return;
        }
        let state = registry.borrow().state.clone();
        {
            let state = state.lock().unwrap();
            if blocked(&state) {
                return;
            }
            let Some(id) = resolve(&state, pane.as_str()) else { return };
            if !displayed(&state, &id) || !matches!(state.layout.placement(&id), Some(Placement::Docked { .. })) {
                return;
            }
        }
        ui.invoke_drop_pane(pane, x, y);
    });
    let view = ui.as_weak();
    ui.global::<PanelLogic>().on_viewport_changed(move || {
        if let Some(ui) = view.upgrade() {
            let zoom = ui.global::<super::Theme>().get_zoom();
            update_viewport(&ui, zoom);
        }
    });
    let weak = Rc::downgrade(registry);
    ui.global::<PanelLogic>().on_resize(move |zone, value| {
        let Some(registry) = weak.upgrade() else { return };
        let state = registry.borrow().state.clone();
        {
            let mut state = state.lock().unwrap();
            if blocked(&state) || !matches!(zone, 0 | 2 | 3) {
                return;
            }
            let layout = &mut state.layout;
            let (left, right, bottom) = (layout.left_width, layout.right_width, layout.bottom_height);
            if layout
                .set_sizes(
                    if zone == 0 { value } else { left },
                    if zone == 2 { value } else { right },
                    if zone == 3 { value } else { bottom },
                )
                .is_err()
            {
                return;
            }
        }
        refresh_all();
    });
    let weak = Rc::downgrade(registry);
    ui.global::<PanelLogic>().on_preset(move |preset| {
        if let Some(registry) = weak.upgrade() {
            reset(
                &registry,
                match preset {
                    1 => Preset::Debugging,
                    2 => Preset::Investigation,
                    _ => Preset::Contracts,
                },
            );
        }
    });
    let weak = Rc::downgrade(registry);
    ui.global::<PanelLogic>().on_reset(move || {
        if let Some(registry) = weak.upgrade() {
            reset(&registry, Preset::Contracts);
        }
    });
    let weak = Rc::downgrade(registry);
    let host_id = host.clone();
    ui.global::<PanelLogic>().on_maximize(move |key| {
        let Some(registry) = weak.upgrade() else { return };
        if host_id.is_some() {
            return;
        }
        let state = registry.borrow().state.clone();
        {
            let mut state = state.lock().unwrap();
            if blocked(&state) {
                return;
            }
            let Some(id) = resolve(&state, key.as_str()) else { return };
            if !displayed(&state, &id) {
                return;
            }
            let Some(Placement::Docked { zone }) = state.layout.placement(&id).cloned() else { return };
            if state.layout.active.get(&zone) != Some(&id.stable_key()) {
                return;
            }
            state.layout.maximized =
                if state.layout.maximized.as_deref() == Some(key.as_str()) { None } else { Some(id.stable_key()) };
        }
        refresh_all();
    });

    let weak = Rc::downgrade(registry);
    let view = ui.as_weak();
    ui.global::<PanelLogic>().on_console_submit(move |input| {
        let Some(registry) = weak.upgrade() else { return };
        let (state, runtime) = {
            let registry = registry.borrow();
            (registry.state.clone(), registry.runtime.clone())
        };
        if blocked(&state.lock().unwrap()) && input.trim() != "cancel" {
            return;
        }
        super::native_debugger::console(view.clone(), state, runtime, input.to_string());
        refresh_all();
    });
    let weak = Rc::downgrade(registry);
    ui.global::<PanelLogic>().on_console_input_edited(move |input| {
        let Some(registry) = weak.upgrade() else { return };
        let state = registry.borrow().state.clone();
        {
            let mut state = state.lock().unwrap();
            if blocked(&state) {
                return;
            }
            state.console.set_draft(input.as_str());
        }
        refresh_all();
    });
    let weak = Rc::downgrade(registry);
    ui.global::<PanelLogic>().on_console_scroll(move |position| {
        if !position.is_finite() || !(-1_000_000.0..=0.0).contains(&position) { return; }
        let Some(registry) = weak.upgrade() else { return };
        let state = registry.borrow().state.clone();
        let Ok(mut state) = state.try_lock() else { return };
        if !blocked(&state) { state.console.scroll_y = position; }
    });
    let weak = Rc::downgrade(registry);
    ui.global::<PanelLogic>().on_console_follow(move |follow| {
        let Some(registry) = weak.upgrade() else { return };
        let state = registry.borrow().state.clone();
        {
            let mut state = state.lock().unwrap();
            if blocked(&state) { return; }
            state.console.follow = follow;
        }
        refresh_all();
    });
    let weak = Rc::downgrade(registry);
    ui.global::<PanelLogic>().on_console_history(move |direction| {
        let Some(registry) = weak.upgrade() else { return };
        let state = registry.borrow().state.clone();
        {
            let mut state = state.lock().unwrap();
            if blocked(&state) {
                return;
            }
            if direction < 0 {
                state.console.previous();
            } else {
                state.console.next();
            }
        }
        refresh_all();
    });
    let weak = Rc::downgrade(registry);
    ui.global::<PanelLogic>().on_console_complete(move || {
        let Some(registry) = weak.upgrade() else { return };
        let state = registry.borrow().state.clone();
        {
            let mut state = state.lock().unwrap();
            if blocked(&state) {
                return;
            }
            let choices = [
                "help",
                "continue",
                "step",
                "stepi",
                "next",
                "finish",
                "print ",
                "ptype ",
                "bt",
                "info locals",
                "info args",
                "info storage",
                "info gas",
                "info logs",
                "info registers",
                "info breakpoints",
                "break ",
                "watch ",
                "delete ",
                "list",
                "disassemble",
                "cancel",
                "history",
                "clear",
            ];
            let matches = choices.iter().filter(|item| item.starts_with(state.console.draft())).collect::<Vec<_>>();
            if matches.len() == 1 {
                state.console.set_draft(matches[0]);
            }
        }
        refresh_all();
    });

    let weak = Rc::downgrade(registry);
    let view = ui.as_weak();
    ui.global::<PanelLogic>().on_constructor_edited(move |owner, index, value| {
        with_deployment(&weak, &view, owner.as_str(), |ui| {
            ui.global::<super::Logic>().invoke_constructor_edited(index, value)
        });
    });
    let weak = Rc::downgrade(registry);
    let view = ui.as_weak();
    ui.global::<PanelLogic>().on_reset_constructor(move |owner| {
        with_deployment(&weak, &view, owner.as_str(), |ui| ui.global::<super::Logic>().invoke_reset_constructor());
    });
    let weak = Rc::downgrade(registry);
    let view = ui.as_weak();
    ui.global::<PanelLogic>().on_deployment_value_edited(move |owner, value| {
        with_deployment(&weak, &view, owner.as_str(), |ui| {
            ui.global::<super::Logic>().invoke_deployment_value_edited(value)
        });
    });
    let weak = Rc::downgrade(registry);
    let view = ui.as_weak();
    ui.global::<PanelLogic>().on_deploy_contract(move |owner| {
        with_deployment(&weak, &view, owner.as_str(), |ui| ui.global::<super::Logic>().invoke_deploy_contract());
    });

    let weak = Rc::downgrade(registry);
    let view = ui.as_weak();
    ui.global::<ContractLogic>().on_detach_result(move |owner| {
        if let (Some(registry), Some(ui)) = (weak.upgrade(), view.upgrade()) {
            if originating_pane_is_hosted(&ui, owner.as_str()) {
                float_contract_panel(&registry, PaneKind::CallResult, owner.as_str());
            }
        }
    });
    let weak = Rc::downgrade(registry);
    let view = ui.as_weak();
    ui.global::<ContractLogic>().on_detach_storage(move |owner| {
        if let (Some(registry), Some(ui)) = (weak.upgrade(), view.upgrade()) {
            if originating_pane_is_hosted(&ui, owner.as_str()) {
                float_contract_panel(&registry, PaneKind::Storage, owner.as_str());
            }
        }
    });
    let weak = Rc::downgrade(registry);
    let view = ui.as_weak();
    ui.global::<ContractLogic>().on_dock_result(move |owner| {
        if let (Some(registry), Some(ui)) = (weak.upgrade(), view.upgrade()) {
            if originating_pane_is_hosted(&ui, owner.as_str()) || originating_kind_is_hosted(&ui, owner.as_str(), 2) {
                return_to_bank(&registry, PaneKind::CallResult, owner.as_str());
            }
        }
    });
    let weak = Rc::downgrade(registry);
    let view = ui.as_weak();
    ui.global::<ContractLogic>().on_dock_storage(move |owner| {
        if let (Some(registry), Some(ui)) = (weak.upgrade(), view.upgrade()) {
            if originating_pane_is_hosted(&ui, owner.as_str()) || originating_kind_is_hosted(&ui, owner.as_str(), 3) {
                return_to_bank(&registry, PaneKind::Storage, owner.as_str());
            }
        }
    });
    if let Some(id) = host {
        let weak = Rc::downgrade(registry);
        ui.window().on_close_requested(move || {
            if let Some(registry) = weak.upgrade() {
                dock_panel(&registry, &id.stable_key(), -1);
            }
            slint::CloseRequestResponse::KeepWindowShown
        });
    }
}

fn with_deployment(
    weak: &std::rc::Weak<RefCell<Registry>>,
    view: &Weak<MainWindow>,
    owner: &str,
    action: impl FnOnce(&MainWindow),
) {
    let Some(registry) = weak.upgrade() else { return };
    let Some(ui) = view.upgrade() else { return };
    if !originating_pane_is_hosted(&ui, owner) {
        return;
    }
    let state = registry.borrow().state.clone();
    {
        let mut state = state.lock().unwrap();
        if blocked(&state) || !state.workspace_ready {
            return;
        }
        let Some(index) = state.documents.iter().position(|doc| doc.id == owner && doc.kind == 1) else { return };
        state.select_document(index as i32);
    }
    action(&ui);
    refresh_all();
}

fn float_contract_panel(registry: &Hosts, kind: PaneKind, owner: &str) {
    if owner.is_empty() {
        return;
    }
    let state = registry.borrow().state.clone();
    let id = {
        let state = state.lock().unwrap();
        if blocked(&state)
            || !state.workspace_ready
            || !state.documents.iter().any(|doc| doc.id == owner && doc.kind == 0)
        {
            return;
        }
        PaneId::new(kind, Some(owner.into()))
    };
    float_panel(registry, &id.stable_key());
}

/// Build and validate the return transition before changing the live model or window.
fn prepare_return_to_bank(state: &DesktopState, kind: PaneKind, owner: &str) -> Result<(Layout, usize), String> {
    if blocked(state) || !state.workspace_ready || owner.is_empty() {
        return Err("The contract panel is unavailable".into());
    }
    if !matches!(kind, PaneKind::CallResult | PaneKind::Storage) {
        return Err("Not a contract subpanel".into());
    }
    let index = state
        .documents
        .iter()
        .position(|doc| doc.id == owner && doc.kind == 0)
        .ok_or_else(|| "Contract document no longer exists".to_owned())?;
    let id = PaneId::new(kind, Some(owner.to_owned()));
    id.validate()?;
    let mut next = state.layout.clone();
    next.remove(&id)?;
    let document = PaneId::new(PaneKind::Document, Some(owner.to_owned()));
    let target = if next.placement(&document).is_some() { document } else { PaneId::new(PaneKind::Document, None) };
    match next.placement(&target).cloned() {
        Some(Placement::Docked { zone }) => next.select(zone, &target)?,
        Some(Placement::Floating { .. }) => {}
        Some(Placement::Hidden { return_zone }) => {
            next.ensure(target.clone(), return_zone)?;
            next.select(return_zone, &target)?;
        }
        None => {
            next.ensure(target.clone(), Zone::Center)?;
            next.select(Zone::Center, &target)?;
        }
    }
    next.validate()?;
    Ok((next, index))
}

fn apply_return_to_bank(state: &mut DesktopState, kind: PaneKind, owner: &str) -> Result<(), String> {
    let (next, index) = prepare_return_to_bank(state, kind, owner)?;
    state.layout = next;
    state.documents[index].contract_view.section = if kind == PaneKind::Storage { 1 } else { 0 };
    state.select_document(index as i32);
    Ok(())
}

fn return_to_bank(registry: &Hosts, kind: PaneKind, owner: &str) {
    let state = registry.borrow().state.clone();
    let id = PaneId::new(kind, Some(owner.to_owned()));
    {
        let mut state = state.lock().unwrap();
        if apply_return_to_bank(&mut state, kind, owner).is_err() {
            return;
        }
    }
    let window = take_window(registry, &id);
    if let Some(window) = window {
        dispose(window);
    }
    registry.borrow_mut().last_focus = Some((0, Some(owner.to_owned())));
    refresh_all();
}

fn live_owner(state: &DesktopState, id: &PaneId) -> bool {
    match (&id.owner, id.kind) {
        (None, PaneKind::Storage) => false,
        (None, _) => true,
        (Some(owner), PaneKind::Document) => state.documents.iter().any(|doc| &doc.id == owner),
        (Some(owner), PaneKind::CallResult | PaneKind::Storage) => {
            state.documents.iter().any(|doc| &doc.id == owner && doc.kind == 0)
        }
        _ => false,
    }
}

fn retain_live_owners(layout: &mut Layout, state: &DesktopState) {
    let live = layout
        .panes
        .iter()
        .filter(|pane| live_owner(state, &pane.id))
        .filter_map(|pane| pane.id.owner.clone())
        .collect::<HashSet<_>>();
    layout.panes.retain(|pane| live_owner(state, &pane.id));
    layout.retain_owners(|owner| live.contains(owner));
}

fn resolve(state: &DesktopState, key: &str) -> Option<PaneId> {
    if let Some(pane) = state.layout.panes.iter().find(|pane| pane.id.stable_key() == key) {
        return live_owner(state, &pane.id).then(|| pane.id.clone());
    }
    let (kind, owner) = if let Some((name, owner)) = key.split_once(':') {
        let kind = match name {
            "document" => PaneKind::Document,
            "call_result" => PaneKind::CallResult,
            "storage" => PaneKind::Storage,
            _ => return None,
        };
        (kind, Some(owner.to_owned()))
    } else {
        let kind = match key {
            "navigation" => PaneKind::Navigation,
            "document" => PaneKind::Document,
            "call_result" => PaneKind::CallResult,
            "storage" => PaneKind::Storage,
            "debug_code" => PaneKind::DebugCode,
            "debug_inspector" => PaneKind::DebugInspector,
            "debug_inspection" => PaneKind::DebugInspection,
            "debug_session" => PaneKind::DebugSession,
            "console" => PaneKind::Console,
            "logs" => PaneKind::Logs,
            "jobs" => PaneKind::Jobs,
            "investigation" => PaneKind::Investigation,
            _ => return None,
        };
        let owner = if matches!(kind, PaneKind::CallResult | PaneKind::Storage) {
            Some(state.current_document_id()?)
        } else {
            None
        };
        (kind, owner)
    };
    let id = PaneId::new(kind, owner);
    (id.validate().is_ok() && live_owner(state, &id)).then_some(id)
}

fn zone_from(zone: i32) -> Option<Zone> {
    match zone {
        0 => Some(Zone::Left),
        1 => Some(Zone::Center),
        2 => Some(Zone::Right),
        3 => Some(Zone::Bottom),
        _ => None,
    }
}
fn default_zone(kind: PaneKind) -> Zone {
    match kind {
        PaneKind::Navigation => Zone::Left,
        PaneKind::Console | PaneKind::Logs | PaneKind::Jobs => Zone::Bottom,
        PaneKind::DebugInspector
        | PaneKind::DebugInspection
        | PaneKind::DebugSession
        | PaneKind::CallResult
        | PaneKind::Storage => Zone::Right,
        _ => Zone::Center,
    }
}
fn default_geometry() -> Geometry {
    Geometry { x: 120, y: 120, width: 900, height: 650, maximized: false }
}

fn prepare_float(layout: &Layout, id: &PaneId, geometry: Geometry) -> Result<Layout, String> {
    let mut next = layout.clone();
    let zone = if id.kind == PaneKind::Document && id.owner.is_some() {
        match layout.placement(&PaneId::new(PaneKind::Document, None)) {
            Some(Placement::Docked { zone }) => *zone,
            Some(Placement::Hidden { return_zone } | Placement::Floating { return_zone, .. }) => *return_zone,
            None => default_zone(id.kind),
        }
    } else {
        default_zone(id.kind)
    };
    next.ensure(id.clone(), zone)?;
    next.float(id, geometry)?;
    next.validate()?;
    Ok(next)
}

fn float_panel(registry: &Hosts, key: &str) {
    let (state, runtime, investigation) = {
        let registry = registry.borrow();
        (registry.state.clone(), registry.runtime.clone(), registry.investigation.clone())
    };
    let (id, previous) = {
        let state = state.lock().unwrap();
        if blocked(&state) {
            return;
        }
        let Some(mut id) = resolve(&state, key) else { return };
        if id.kind == PaneKind::Document && id.owner.is_none() {
            let Some(owner) = state.current_document_id() else { return };
            id.owner = Some(owner);
        }
        if !live_owner(&state, &id) || unavailable(&state, &id) {
            return;
        }
        (id, state.layout.clone())
    };
    if registry.borrow().windows.contains_key(&id.stable_key()) {
        return;
    }
    let geometry = registry
        .borrow()
        .geometries
        .get(&id.stable_key())
        .copied()
        .or_else(|| match previous.placement(&id) {
            Some(Placement::Floating { geometry, .. }) => Some(*geometry),
            _ => None,
        })
        .unwrap_or_else(default_geometry);
    let ui = match MainWindow::new() {
        Ok(ui) => ui,
        Err(_) => {
            state.lock().unwrap().notice = "Could not create a floating native window; panel remains docked.".into();
            return;
        }
    };
    let geometry = super::platform::visible_panel_geometry(geometry, ui.window().scale_factor());
    let mut next = match prepare_float(&previous, &id, geometry) {
        Ok(next) => next,
        Err(_) => {
            state.lock().unwrap().notice = "Could not float this panel; layout is unchanged.".into();
            return;
        }
    };
    if id.kind == PaneKind::Document {
        let generic = PaneId::new(PaneKind::Document, None);
        if next.maximized.as_deref() == Some(generic.stable_key().as_str()) {
            next.maximized = None;
        }
    }
    ui.set_panel_host(true);
    ui.set_host_pane(kind_index(id.kind));
    ui.set_host_owner(id.owner.clone().unwrap_or_default().into());
    ui.window().set_size(slint::LogicalSize::new(geometry.width as f32, geometry.height as f32));
    ui.window().set_position(slint::PhysicalPosition::new(geometry.x, geometry.y));
    super::wire(&ui, &state, runtime.clone());
    super::native_control::wire_shared(&ui, &state);
    super::native_investigation::wire_host(&ui, &state, &investigation, runtime);
    wire_host(&ui, registry, Some(id.clone()));
    let main = registry.borrow().main.clone();
    let folder_state = state.clone();
    ui.global::<super::Logic>().on_open_folder(move || {
        if blocked(&folder_state.lock().unwrap()) {
            return;
        }
        if let Some(main) = main.upgrade() {
            main.global::<super::Logic>().invoke_open_folder();
        }
    });
    {
        let state = state.lock().unwrap();
        super::render(&ui, &state);
        render_host(&ui, &state, Some(&id));
    }
    ui.window().set_maximized(geometry.maximized);
    if ui.show().is_err() {
        super::native_macro::detach_host(&ui);
        super::native_investigation::detach_host(&ui);
        state.lock().unwrap().notice = "Floating window could not be shown; panel remains docked.".into();
        return;
    }
    ui.window().set_size(slint::LogicalSize::new(geometry.width as f32, geometry.height as f32));
    ui.window().set_position(slint::PhysicalPosition::new(geometry.x, geometry.y));
    ui.window().set_maximized(geometry.maximized);
    {
        let state = state.lock().unwrap();
        if blocked(&state) || state.layout != previous || !live_owner(&state, &id) {
            drop(state);
            super::native_macro::detach_host(&ui);
            super::native_investigation::detach_host(&ui);
            let _ = ui.hide();
            return;
        }
    }
    clear_wallet_for_move(registry, &id);
    {
        let mut state = state.lock().unwrap();
        if blocked(&state) || state.layout != previous || !live_owner(&state, &id) {
            drop(state);
            super::native_macro::detach_host(&ui);
            super::native_investigation::detach_host(&ui);
            let _ = ui.hide();
            return;
        }
        state.layout = next;
    }
    registry.borrow_mut().windows.insert(id.stable_key(), Floating { ui, id });
    forget_focus(registry);
    refresh_all();
}

fn capture(window: &Floating) -> Option<Geometry> {
    let ui = &window.ui;
    let position = ui.window().position();
    let size = ui.window().size();
    let scale = ui.window().scale_factor();
    if !scale.is_finite() || scale <= 0.0 {
        return None;
    }
    let geometry = Geometry {
        x: position.x,
        y: position.y,
        width: (size.width as f32 / scale).round().clamp(240.0, 8192.0) as u32,
        height: (size.height as f32 / scale).round().clamp(160.0, 8192.0) as u32,
        maximized: ui.window().is_maximized(),
    };
    geometry.validate().is_ok().then_some(geometry)
}
fn dispose(window: Floating) {
    super::native_macro::detach_host(&window.ui);
    super::native_investigation::detach_host(&window.ui);
    super::clear_session_inputs(&window.ui);
    let _ = window.ui.hide();
}
fn take_window(registry: &Hosts, id: &PaneId) -> Option<Floating> {
    let key = id.stable_key();
    let window = registry.borrow_mut().windows.remove(&key);
    if let Some(geometry) = window.as_ref().and_then(capture) {
        registry.borrow_mut().geometries.insert(key, geometry);
    }
    window
}
fn wallet_document(state: &DesktopState, id: &PaneId) -> bool {
    if id.kind != PaneKind::Document {
        return false;
    }
    document_for(state, id).is_some_and(|doc| doc.kind == 2)
}
fn clear_wallet_for_move(registry: &Hosts, id: &PaneId) {
    let state = registry.borrow().state.clone();
    if !wallet_document(&state.lock().unwrap(), id) {
        return;
    }
    let (main, windows) = {
        let registry = registry.borrow();
        (registry.main.upgrade(), registry.windows.values().map(|window| window.ui.clone_strong()).collect::<Vec<_>>())
    };
    for ui in main.into_iter().chain(windows) {
        let data = ui.global::<AppData>();
        data.set_wallet_password("".into());
        data.set_keystore_password("".into());
        data.set_keystore_confirm("".into());
        data.set_keystore_import("".into());
    }
    state.lock().unwrap().notice =
        "Wallet password and private key drafts were cleared when the pane moved. Enter them again in the destination."
            .into();
}

fn merge_current_document(layout: &mut Layout, id: &PaneId) -> Result<(), String> {
    if id.kind != PaneKind::Document || id.owner.is_none() {
        return Ok(());
    }
    let zone = match layout.placement(id) {
        Some(Placement::Docked { zone }) => *zone,
        _ => return Err("Document is not docked".into()),
    };
    layout.remove(id)?;
    let generic = PaneId::new(PaneKind::Document, None);
    layout.ensure(generic.clone(), zone)?;
    layout.dock(&generic, zone)
}

fn dock_panel(registry: &Hosts, key: &str, zone: i32) {
    let state = registry.borrow().state.clone();
    let (id, next) = {
        let state = state.lock().unwrap();
        if blocked(&state) {
            return;
        }
        let Some(id) = resolve(&state, key) else { return };
        if unavailable(&state, &id) {
            return;
        }
        let mut next = state.layout.clone();
        let old = next.placement(&id).cloned();
        if next.ensure(id.clone(), default_zone(id.kind)).is_err() {
            return;
        }
        let result = match (zone, old) {
            (-1, Some(Placement::Floating { .. })) => next.close_float(&id),
            (-1, Some(Placement::Hidden { return_zone })) => next.dock(&id, return_zone),
            (-1, Some(Placement::Docked { zone })) => next.dock(&id, zone),
            (-1, _) => next.dock(&id, default_zone(id.kind)),
            (_, _) => match zone_from(zone) {
                Some(zone) => next.dock(&id, zone),
                None => return,
            },
        };
        if result.is_err() {
            return;
        }
        if state.current_document_id().as_ref() == id.owner.as_ref() && merge_current_document(&mut next, &id).is_err()
        {
            return;
        }
        if next.validate().is_err() {
            return;
        }
        (id, next)
    };
    let changes_host = registry.borrow().windows.contains_key(&id.stable_key())
        || state.lock().unwrap().layout.placement(&id) != next.placement(&id);
    if changes_host {
        clear_wallet_for_move(registry, &id);
    }
    let window = take_window(registry, &id);
    state.lock().unwrap().layout = next;
    if let Some(window) = window {
        dispose(window);
    }
    refresh_all();
}
fn hide_panel(registry: &Hosts, key: &str) {
    let state = registry.borrow().state.clone();
    let (id, next) = {
        let state = state.lock().unwrap();
        if blocked(&state) {
            return;
        }
        let Some(id) = resolve(&state, key) else { return };
        if unavailable(&state, &id) {
            return;
        }
        let mut next = state.layout.clone();
        if next.hide(&id).is_err() || next.validate().is_err() {
            return;
        }
        (id, next)
    };
    clear_wallet_for_move(registry, &id);
    let window = take_window(registry, &id);
    state.lock().unwrap().layout = next;
    if let Some(window) = window {
        dispose(window);
    }
    forget_focus(registry);
    refresh_all();
}
/// Placement changed under the cached focus, so the next navigation to the
/// same document must restore its pane instead of being treated as a no-op.
fn forget_focus(registry: &Hosts) {
    registry.borrow_mut().last_focus = None;
}
fn reset(registry: &Hosts, preset: Preset) {
    let state = registry.borrow().state.clone();
    let ids = registry.borrow().windows.values().map(|window| window.id.clone()).collect::<Vec<_>>();
    let next = {
        let state = state.lock().unwrap();
        if blocked(&state) {
            return;
        }
        let mut next = Layout::preset(preset);
        if preset == Preset::Debugging {
            let _ = next.hide(&PaneId::new(PaneKind::Navigation, None));
            next.bottom_height = 170.0;
        }
        if next.validate().is_err() {
            return;
        }
        next
    };
    for id in &ids {
        clear_wallet_for_move(registry, id);
    }
    let windows = ids.iter().filter_map(|id| take_window(registry, id)).collect::<Vec<_>>();
    state.lock().unwrap().layout = next;
    for window in windows {
        dispose(window);
    }
    forget_focus(registry);
    refresh_all();
}
fn layout_snapshot(registry: &Hosts, final_save: bool) -> Option<(std::path::PathBuf, serde_json::Value)> {
    let (state, epoch, captures) = {
        let registry = registry.try_borrow().ok()?;
        // A minimized window reports a parked off-screen position and a tiny
        // size; it keeps the geometry already stored in the layout instead.
        let captures = registry
            .windows
            .values()
            .filter(|window| !window.ui.window().is_minimized())
            .map(|window| capture(window).map(|geometry| (window.id.clone(), geometry)))
            .collect::<Option<Vec<_>>>()?;
        (registry.state.clone(), registry.workspace_epoch, captures)
    };
    let state = if final_save { state.lock().ok()? } else { state.try_lock().ok()? };
    if !state.workspace_ready || state.workspace_epoch != epoch || (!final_save && blocked(&state)) {
        return None;
    }
    let mut layout = state.layout.clone();
    retain_live_owners(&mut layout, &state);
    for (id, geometry) in captures {
        if live_owner(&state, &id) && matches!(layout.placement(&id), Some(Placement::Floating { .. })) {
            layout.float(&id, geometry).ok()?;
        }
    }
    Some((state.workspace.clone(), layout.to_json().ok()?))
}

fn autosave_tick(registry: &Hosts) {
    let Some((workspace, snapshot)) = layout_snapshot(registry, false) else {
        if let Ok(mut registry) = registry.try_borrow_mut() {
            registry.autosave.suspend();
        }
        return;
    };
    let now = Instant::now();
    let should_save = match registry.try_borrow_mut() {
        Ok(mut registry) => registry.autosave.observe(&snapshot, now),
        Err(_) => false,
    };
    if !should_save {
        return;
    }
    let result = session::save_setting_fields(&workspace, &[("layout", snapshot.clone())]);
    if let Ok(mut registry) = registry.try_borrow_mut() {
        if result.is_ok() {
            registry.autosave.saved(snapshot);
        } else {
            registry.autosave.last_failure = Some(now);
        }
    }
}

fn save_layout(registry: &Hosts) {
    if registry.borrow().autosave.disabled {
        return;
    }
    if let Some((workspace, snapshot)) = layout_snapshot(registry, true) {
        if session::save_setting_fields(&workspace, &[("layout", snapshot.clone())]).is_ok() {
            registry.borrow_mut().autosave.saved(snapshot);
        }
    }
}

/// Called from the GUI thread after every state mutation.
pub fn refresh_all() {
    let registry = HOSTS.with(|hosts| hosts.borrow().clone());
    let Some(registry) = registry else { return };
    let (state, main, windows, epoch) = {
        let Ok(registry) = registry.try_borrow() else { return };
        (
            registry.state.clone(),
            registry.main.upgrade(),
            registry.windows.values().map(|window| (window.id.clone(), window.ui.clone_strong())).collect::<Vec<_>>(),
            registry.workspace_epoch,
        )
    };
    let (stale, active) = {
        let mut state = state.lock().unwrap();
        let active = state.active && state.workspace_epoch == epoch;
        if active {
            let mut layout = state.layout.clone();
            retain_live_owners(&mut layout, &state);
            state.layout = layout;
        }
        (
            windows
                .iter()
                .filter(|(id, _)| {
                    !active
                        || !live_owner(&state, id)
                        || !matches!(state.layout.placement(id), Some(Placement::Floating { .. }))
                })
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>(),
            active,
        )
    };
    for id in &stale {
        if let Some(window) = take_window(&registry, id) {
            dispose(window);
        }
        registry.borrow_mut().geometries.remove(&id.stable_key());
    }
    if active {
        let state = state.lock().unwrap();
        if let Some(main) = main {
            super::render(&main, &state);
            render_host(&main, &state, None);
        }
        for (id, ui) in windows {
            if !stale.contains(&id) {
                super::render(&ui, &state);
                render_host(&ui, &state, Some(&id));
            }
        }
    }
    super::native_investigation::refresh_all();
}
fn kind_index(kind: PaneKind) -> i32 {
    match kind {
        PaneKind::Navigation => 0,
        PaneKind::Document => 1,
        PaneKind::CallResult => 2,
        PaneKind::Storage => 3,
        PaneKind::DebugCode => 4,
        PaneKind::DebugInspector => 5,
        PaneKind::DebugInspection => 6,
        PaneKind::DebugSession => 7,
        PaneKind::Console => 8,
        PaneKind::Logs => 9,
        PaneKind::Jobs => 10,
        PaneKind::Investigation => 11,
    }
}
fn document_for<'a>(state: &'a DesktopState, id: &PaneId) -> Option<&'a Document> {
    match &id.owner {
        Some(owner) => state.documents.iter().find(|doc| &doc.id == owner),
        None if matches!(id.kind, PaneKind::Document | PaneKind::CallResult) => {
            usize::try_from(state.active_document).ok().and_then(|index| state.documents.get(index))
        }
        None => None,
    }
}
/// The generic Response pane shows the active contract instance's result,
/// unless that tab's result already has its own pane.
fn side_response(state: &DesktopState, doc: &Document) -> bool {
    state.current_document_id().as_deref() == Some(doc.id.as_str())
        && doc.kind == 0
        && doc.contract_view.target.is_some()
        && matches!(
            state.layout.placement(&PaneId::new(PaneKind::CallResult, None)),
            Some(Placement::Docked { .. } | Placement::Floating { .. })
        )
        && state.layout.placement(&PaneId::new(PaneKind::CallResult, Some(doc.id.clone()))).is_none()
}

fn displayed(state: &DesktopState, id: &PaneId) -> bool {
    // Debugger panes belong to the Debugger activity; elsewhere they would only
    // add a second tab strip above the document.
    if matches!(id.kind, PaneKind::DebugCode | PaneKind::DebugInspector | PaneKind::DebugInspection | PaneKind::DebugSession)
        && state.activity != 4
    {
        return false;
    }
    // The macro builder has its own library list, so it takes the full width.
    if id.kind == PaneKind::Navigation && state.activity == 3 && state.current_doc_kind() == 6 {
        return false;
    }
    // Investigation lives in the Explorer tab once a project is open.
    if id.kind == PaneKind::Investigation && state.workspace_ready {
        return false;
    }
    if id.kind == PaneKind::CallResult && id.owner.is_none() {
        if state.activity == 4 {
            return false;
        }
        return usize::try_from(state.active_document)
            .ok()
            .and_then(|index| state.documents.get(index))
            .is_some_and(|doc| side_response(state, doc));
    }
    if id.kind != PaneKind::Document || id.owner.is_some() {
        return true;
    }
    let Some(current) = state.current_document_id() else { return true };
    let owned = PaneId::new(PaneKind::Document, Some(current));
    state.layout.placement(&owned).is_none()
}
struct ZoneProjection {
    rows: Vec<PanelRow>,
    active: String,
    index: i32,
    kind: i32,
    title: String,
    owner: String,
    document_kind: i32,
    document_title: String,
    document_body: String,
    contract: super::ContractViewData,
    deployment: super::DeploymentView,
}
fn projection(
    state: &DesktopState,
    id: Option<&PaneId>,
) -> (i32, String, String, i32, String, String, super::ContractViewData, super::DeploymentView) {
    let Some(id) = id else {
        return (
            -1,
            String::new(),
            String::new(),
            -1,
            String::new(),
            String::new(),
            Default::default(),
            Default::default(),
        );
    };
    let document = document_for(state, id);
    let owner = document.map(|doc| doc.id.clone()).unwrap_or_default();
    let title = document
        .filter(|_| id.kind == PaneKind::Document)
        .map(|doc| doc.title.clone())
        .unwrap_or_else(|| id.title().into());
    let contract = document
        .filter(|doc| doc.kind == 0)
        .map(|doc| {
            let mut view = super::native_contract::presentation(state, doc);
            view.result_side = side_response(state, doc);
            view.result_detached = view.result_side
                || state.layout.placement(&PaneId::new(PaneKind::CallResult, Some(doc.id.clone()))).is_some();
            view.storage_detached =
                state.layout.placement(&PaneId::new(PaneKind::Storage, Some(doc.id.clone()))).is_some();
            view
        })
        .unwrap_or_default();
    let deployment =
        document.filter(|doc| doc.kind == 1).map(|doc| super::deployment_presentation(state, doc)).unwrap_or_default();
    (
        kind_index(id.kind),
        title,
        owner,
        document.map(|doc| doc.kind).unwrap_or(-1),
        document.map(|doc| doc.title.clone()).unwrap_or_default(),
        document.map(|doc| doc.body.clone()).unwrap_or_default(),
        contract,
        deployment,
    )
}
fn document_role(kind: i32) -> &'static str {
    match kind {
        0 => "Request",
        1 => "Constructor",
        2 => "Keystores",
        3 => "Trace",
        4 => "Lookup",
        5 => "Conversion",
        6 => "Macro library",
        7 => "Build",
        8 => "Toolchain",
        _ => "Document",
    }
}

fn zone_projection(state: &DesktopState, zone: Zone) -> ZoneProjection {
    let panes = state
        .layout
        .panes
        .iter()
        .filter(|pane| pane.placement == Placement::Docked { zone } && displayed(state, &pane.id))
        .collect::<Vec<_>>();
    let active = state
        .layout
        .active
        .get(&zone)
        .and_then(|key| panes.iter().position(|pane| &pane.id.stable_key() == key))
        .or_else(|| (!panes.is_empty()).then_some(0));
    let selected = active.map(|index| &panes[index].id);
    let rows = panes
        .iter()
        .enumerate()
        .map(|(index, pane)| {
            let projected = projection(state, Some(&pane.id));
            // The document strip already names the document; stacked pane tabs name the role.
            let title = if pane.id.kind == PaneKind::Document { document_role(projected.3).to_string() } else { projected.1 };
            PanelRow { id: pane.id.stable_key().into(), title: title.into(), active: Some(index) == active }
        })
        .collect();
    let (kind, mut title, owner, document_kind, document_title, document_body, contract, deployment) =
        projection(state, selected);
    if selected.is_some_and(|id| id.kind == PaneKind::Document) {
        title = document_role(document_kind).into();
    }
    ZoneProjection {
        rows,
        active: selected.map(PaneId::stable_key).unwrap_or_default(),
        index: active.map(|index| index as i32).unwrap_or(-1),
        kind,
        title,
        owner,
        document_kind,
        document_title,
        document_body,
        contract,
        deployment,
    }
}
fn update_viewport(ui: &MainWindow, zoom: f32) {
    let scale = ui.window().scale_factor().max(0.1);
    let height = ui.window().size().height as f32 / scale / zoom.max(0.1);
    ui.global::<PanelData>().set_viewport_width(ui.window().size().width as f32 / scale);
    let fraction = if ui.global::<AppData>().get_activity() == 4 { 0.45 } else { 0.26 };
    let limit = (height * fraction).clamp(96.0, 280.0);
    ui.global::<PanelData>().set_bottom_limit(limit);
}

fn render_host(ui: &MainWindow, state: &DesktopState, pane: Option<&PaneId>) {
    let data = ui.global::<PanelData>();
    update_viewport(ui, state.zoom);
    data.set_blocked(blocked(state));
    data.set_console_text(state.console.transcript_text().into());
    data.set_console_scroll_y(state.console.scroll_y);
    data.set_console_follow(state.console.follow);
    if data.get_console_input().as_str() != state.console.draft() {
        data.set_console_input(state.console.draft().into());
    }
    data.set_left_width(state.layout.left_width);
    data.set_right_width(state.layout.right_width);
    data.set_bottom_height(state.layout.bottom_height);
    let maximized = state
        .layout
        .maximized
        .as_ref()
        .filter(|key| {
            state
                .layout
                .panes
                .iter()
                .find(|pane| pane.id.stable_key() == key.as_str())
                .is_some_and(|pane| displayed(state, &pane.id))
        })
        .cloned()
        .unwrap_or_default();
    data.set_maximized(maximized.into());
    if let Some(id) = pane {
        if id.kind == PaneKind::Document {
            if let Some(doc) = document_for(state, id) {
                let app = ui.global::<AppData>();
                app.set_document_body(doc.body.clone().into());
                app.set_doc_kind(doc.kind);
            }
        }
        let (_, title, _, document_kind, document_title, document_body, contract, deployment) =
            projection(state, Some(id));
        data.set_pane_id(id.stable_key().into());
        data.set_pane_title(title.into());
        data.set_document_kind(document_kind);
        data.set_document_title(document_title.into());
        data.set_document_body(document_body.into());
        data.set_contract_view(keep_request_rows(data.get_contract_view(), contract));
        data.set_deployment_view(keep_field_rows(data.get_deployment_view(), deployment));
        if let Some(doc) = document_for(state, id).filter(|doc| doc.kind == 0) {
            super::native_contract::render_document(ui, state, doc);
        }
    }
    let left = zone_projection(state, Zone::Left);
    data.set_left_visible(!left.rows.is_empty());
    data.set_left_tabs(super::model(left.rows));
    data.set_left_active(left.active.into());
    data.set_left_active_index(left.index);
    data.set_left_kind(left.kind);
    data.set_left_title(left.title.into());
    data.set_left_owner(left.owner.into());
    data.set_left_document_kind(left.document_kind);
    data.set_left_document_title(left.document_title.into());
    data.set_left_document_body(left.document_body.into());
    data.set_left_contract(keep_request_rows(data.get_left_contract(), left.contract));
    data.set_left_deployment(keep_field_rows(data.get_left_deployment(), left.deployment));
    let center = zone_projection(state, Zone::Center);
    data.set_center_visible(!center.rows.is_empty());
    data.set_center_tabs(super::model(center.rows));
    data.set_center_active(center.active.into());
    data.set_center_active_index(center.index);
    data.set_center_kind(center.kind);
    data.set_center_title(center.title.into());
    data.set_center_owner(center.owner.into());
    data.set_center_document_kind(center.document_kind);
    data.set_center_document_title(center.document_title.into());
    data.set_center_document_body(center.document_body.into());
    data.set_center_contract(keep_request_rows(data.get_center_contract(), center.contract));
    data.set_center_deployment(keep_field_rows(data.get_center_deployment(), center.deployment));
    let right = zone_projection(state, Zone::Right);
    data.set_right_visible(!right.rows.is_empty());
    data.set_right_tabs(super::model(right.rows));
    data.set_right_active(right.active.into());
    data.set_right_active_index(right.index);
    data.set_right_kind(right.kind);
    data.set_right_title(right.title.into());
    data.set_right_owner(right.owner.into());
    data.set_right_document_kind(right.document_kind);
    data.set_right_document_title(right.document_title.into());
    data.set_right_document_body(right.document_body.into());
    data.set_right_contract(keep_request_rows(data.get_right_contract(), right.contract));
    data.set_right_deployment(keep_field_rows(data.get_right_deployment(), right.deployment));
    let bottom = zone_projection(state, Zone::Bottom);
    data.set_bottom_visible(!bottom.rows.is_empty());
    data.set_bottom_tabs(super::model(bottom.rows));
    data.set_bottom_active(bottom.active.into());
    data.set_bottom_active_index(bottom.index);
    data.set_bottom_kind(bottom.kind);
    data.set_bottom_title(bottom.title.into());
    data.set_bottom_owner(bottom.owner.into());
    data.set_bottom_document_kind(bottom.document_kind);
    data.set_bottom_document_title(bottom.document_title.into());
    data.set_bottom_document_body(bottom.document_body.into());
    data.set_bottom_contract(keep_request_rows(data.get_bottom_contract(), bottom.contract));
    data.set_bottom_deployment(keep_field_rows(data.get_bottom_deployment(), bottom.deployment));
}

/// Request argument fields keep their model while the same function of the
/// same tab is shown, so typing does not lose focus. `details` names the tab.
fn keep_request_rows(shown: super::ContractViewData, mut next: super::ContractViewData) -> super::ContractViewData {
    if shown.details == next.details && shown.selected_name == next.selected_name {
        next.fields = super::keep_rows(shown.fields, next.fields, |old, new| old.name == new.name && old.kind == new.kind);
    }
    next
}

/// A fresh `fields` model makes Slint rebuild every constructor LineEdit, which
/// drops keyboard focus after each keystroke. Keep the shown model when the
/// argument schema is unchanged and update only the rows that differ.
fn keep_field_rows(shown: super::DeploymentView, mut next: super::DeploymentView) -> super::DeploymentView {
    let same_schema = shown.fields.row_count() == next.fields.row_count()
        && (0..next.fields.row_count()).all(|index| {
            match (shown.fields.row_data(index), next.fields.row_data(index)) {
                (Some(old), Some(new)) => old.name == new.name && old.kind == new.kind,
                _ => false,
            }
        });
    if same_schema && next.fields.row_count() > 0 {
        for index in 0..next.fields.row_count() {
            let row = next.fields.row_data(index).unwrap();
            if shown.fields.row_data(index).as_ref() != Some(&row) {
                shown.fields.set_row_data(index, row);
            }
        }
        next.fields = shown.fields;
    }
    next
}

#[cfg(test)]
#[path = "panel_tests.rs"]
mod tests;
