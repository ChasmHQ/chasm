//! Compiler and libraries view. Installs run off the UI thread; any change
//! to the compiler or remappings triggers a recompile.
use super::session::DesktopState;
use super::toolchain;
use super::{LibraryRow, MainWindow, NetworkProfileRow, PragmaRow, ToolchainData, ToolchainLogic};
use slint::{ComponentHandle, Weak};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type State = Arc<Mutex<DesktopState>>;

thread_local! {
    /// The `[compiler]` settings last copied into the form.
    static SETTINGS_SHOWN: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
    /// The network profile the form was last filled from, so typing in the
    /// form never fights a repaint.
    static PROFILE_SHOWN: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

const CONFIRM_TIMEOUT: Duration = Duration::from_secs(30);

struct FreshConfirmation {
    workspace: PathBuf,
    target: String,
    plan: toolchain::NetworkSwitch,
    expires: Instant,
}

impl FreshConfirmation {
    fn consume(pending: &mut Option<Self>, workspace: &Path, target: &str, now: Instant) -> Option<toolchain::NetworkSwitch> {
        pending.take().filter(|request| request.workspace == workspace && request.target == target && now < request.expires).map(|request| request.plan)
    }
}

pub fn render(ui: &MainWindow, state: &DesktopState) {
    // Disk scans only while the view is open.
    if state.current_doc_kind() != 8 {
        if !ui.global::<ToolchainData>().get_fresh_target().is_empty() {
            ui.global::<ToolchainLogic>().invoke_cancel_fresh();
        }
        return;
    }
    let data = ui.global::<ToolchainData>();
    let workspace = &state.workspace;
    data.set_locked(toolchain::locked_solc(workspace).into());
    let settings = toolchain::compiler_settings(workspace);
    let (chosen, active, origin, effective, error) = toolchain::build_source(workspace);
    data.set_build_chosen(chosen.into());
    data.set_build_active(active.clone().into());
    data.set_build_origin(origin.into());
    data.set_build_error(error.into());
    data.set_imports(super::model(super::foundry_import::saved(workspace).into_iter().map(Into::into).collect()));
    data.set_settings_summary(if active == "foundry" { format!("{effective} (from foundry.toml)") } else { settings.describe() }.into());
    // Fill the form from the saved settings once per change, never while typing.
    let key = format!("{settings:?}");
    let fresh = SETTINGS_SHOWN.with(|shown| shown.replace(key.clone()) != key);
    if fresh {
        data.set_optimizer(match settings.optimizer { Some(true) => 1, Some(false) => 2, None => 0 });
        data.set_optimizer_runs(settings.optimizer_runs.map(|runs| runs.to_string()).unwrap_or_default().into());
        data.set_via_ir(settings.via_ir == Some(true));
        data.set_evm_version(settings.evm_version.clone().into());
        data.set_bytecode_hash(settings.bytecode_hash.clone().into());
    }
    data.set_installed_solc(super::model(toolchain::installed_solc().into_iter().map(Into::into).collect()));
    data.set_pragmas(super::model(toolchain::pragmas(workspace).into_iter().map(|(file, pragma)| PragmaRow { file: file.into(), pragma: pragma.into() }).collect()));
    let missing: Vec<&str> = toolchain::missing_imports(workspace).iter().map(|library| library.id).collect();
    // Missing libraries first: they are what blocks the compile.
    let mut libraries: Vec<&toolchain::Library> = toolchain::LIBRARIES.iter().collect();
    libraries.sort_by_key(|library| !missing.contains(&library.id));
    data.set_libraries(super::model(
        libraries
            .into_iter()
            .map(|library| LibraryRow {
                id: library.id.into(),
                title: library.title.into(),
                prefix: library.prefix.into(),
                installed: toolchain::installed(library.id).join(", ").into(),
                used: toolchain::used_version(workspace, library.id).unwrap_or_default().into(),
                missing: missing.contains(&library.id),
            })
            .collect(),
    ));
    data.set_missing_count(missing.len() as i32);
    // Network profiles: what the node runs, what is saved, and the list.
    let (profiles, active, resolved) = toolchain::load_network(workspace);
    let describe = |settings: &crate::config::NetworkSettings| {
        if settings.is_local() {
            "a fresh local chain".to_string()
        } else {
            // The running fork knows its chain id; a saved one is named by its preset.
            let chain = Some(settings)
                .filter(|settings| *settings == &state.network)
                .and(state.fork_chain_id)
                .map(toolchain::chain_name)
                .or_else(|| toolchain::PUBLIC_RPCS.iter().find(|(_, url, _)| *url == settings.fork_url).map(|(name, _, _)| name.to_string()))
                .unwrap_or_else(|| super::fork_host(&settings.fork_url));
            let mode = match settings.chain_id_mode {
                crate::config::ChainIdMode::Local => "chain id 31337",
                crate::config::ChainIdMode::Source => "the source chain id",
            };
            format!("a fork of {chain} at {} with {mode}", settings.fork_block.map(|block| format!("block {block}")).unwrap_or_else(|| "the latest block".into()))
        }
    };
    data.set_fork_running(describe(&state.network).into());
    let warning = match state.fork_chain_id {
        Some(chain) if toolchain::is_op_stack(chain) && !state.fork_optimism => "OP Stack mode is off for this fork. Its system transactions, precompiles and fees may differ from the local node.".into(),
        Some(chain) => toolchain::chain_warning(chain).unwrap_or_default().into(),
        None => String::new(),
    };
    data.set_chain_warning(warning.into());
    data.set_presets(super::model(toolchain::PUBLIC_RPCS.iter().map(|(name, _, _)| slint::SharedString::from(*name)).collect()));
    data.set_fork_saved(describe(&resolved).into());
    let local_active = active.is_empty() || active == crate::config::NetworkProfile::LOCAL_ID;
    let mut rows = vec![NetworkProfileRow {
        id: crate::config::NetworkProfile::LOCAL_ID.into(),
        title: "Local chain".into(),
        detail: "built in: a fresh local chain, chain id 31337".into(),
        active: local_active,
        builtin: true,
    }];
    rows.extend(profiles.iter().map(|profile| NetworkProfileRow {
        id: profile.id.clone().into(),
        title: profile.title().into(),
        detail: profile_detail(profile).into(),
        active: profile.id == active,
        builtin: false,
    }));
    data.set_profiles(super::model(rows));
    // Fill the form when the selection changes, never while typing. With no
    // selection, the form shows the active profile.
    let wanted = data.get_selected_profile().to_string();
    let wanted = if wanted.is_empty() { if local_active { crate::config::NetworkProfile::LOCAL_ID.to_string() } else { active.clone() } } else { wanted };
    let fresh_form = PROFILE_SHOWN.with(|shown| shown.replace(wanted.clone()) != wanted);
    if fresh_form {
        let profile = profiles.iter().find(|profile| profile.id == wanted);
        data.set_profile_name(profile.map(|profile| profile.name.clone()).unwrap_or_default().into());
        data.set_fork_url(profile.map(|profile| profile.fork_url.clone()).unwrap_or_default().into());
        data.set_fork_block(profile.and_then(|profile| profile.fork_block).map(|block| block.to_string()).unwrap_or_default().into());
        data.set_chain_mode(i32::from(profile.is_some_and(|profile| profile.chain_id_mode == crate::config::ChainIdMode::Source)));
        PROFILE_SHOWN.with(|shown| *shown.borrow_mut() = wanted.clone());
    }
    data.set_lib_folder(crate::config::global_lib_dir().display().to_string().into());
}

/// One line for the profile list: host only, since keys often sit in the URL.
fn profile_detail(profile: &crate::config::NetworkProfile) -> String {
    if profile.is_local() {
        return "a fresh local chain".into();
    }
    let source = if profile.fork_url.is_empty() { format!("env {}", profile.fork_url_env) } else { super::fork_host(&profile.fork_url) };
    let block = profile.fork_block.map(|block| format!(" at block {block}")).unwrap_or_else(|| " at the latest block".into());
    let mode = match profile.chain_id_mode {
        crate::config::ChainIdMode::Local => "chain id 31337",
        crate::config::ChainIdMode::Source => "the source chain id",
    };
    format!("fork of {source}{block}, {mode}")
}

/// A profile id from a display name: lowercase, separators collapsed to one dash.
fn profile_slug(name: &str) -> String {
    let mut slug = String::new();
    for c in name.trim().to_lowercase().chars() {
        let c = if c.is_ascii_lowercase() || c.is_ascii_digit() { c } else { '-' };
        if c == '-' && (slug.is_empty() || slug.ends_with('-')) { continue; }
        slug.push(c);
    }
    let slug: String = slug.trim_end_matches('-').chars().take(40).collect();
    if slug.is_empty() { "network".into() } else { slug }
}

fn status(weak: &Weak<MainWindow>, text: String, busy: bool) {
    if let Some(ui) = weak.upgrade() {
        ui.global::<ToolchainData>().set_status(text.into());
        ui.global::<ToolchainData>().set_busy(busy);
    }
}

fn recompile(weak: &Weak<MainWindow>) {
    if let Some(ui) = weak.upgrade() {
        ui.global::<super::Logic>().invoke_recompile();
    }
}

pub fn wire(ui: &MainWindow, state: &State, runtime: tokio::runtime::Handle) {
    // MainWindow survives reopen, but these callbacks and their pending
    // confirmation belong to this session only (even for the same path).
    let pending = Rc::new(RefCell::new(None::<FreshConfirmation>));
    let expiry = Rc::new(slint::Timer::default());
    ui.global::<ToolchainData>().set_fresh_target("".into());
    ui.global::<ToolchainData>().set_switch_fresh(false);
    ui.global::<ToolchainData>().set_status("".into());
    ui.global::<ToolchainData>().set_busy(false);
    ui.global::<ToolchainLogic>().on_cancel_fresh({
        let pending = pending.clone();
        let expiry = expiry.clone();
        let weak = ui.as_weak();
        move || {
            expiry.stop();
            pending.borrow_mut().take();
            if let Some(ui) = weak.upgrade() { ui.global::<ToolchainData>().set_fresh_target("".into()); }
        }
    });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<ToolchainLogic>().on_build_source({
        let weak = weak.clone();
        let held = held.clone();
        move |source| {
            let workspace = held.lock().unwrap().workspace.clone();
            let Some(source) = crate::build_config::Source::parse(source.as_str()) else { return };
            match crate::build_config::choose(&workspace, source) {
                Ok(()) => {
                    status(&weak, format!("Build source: {}. Recompiling…", source.label()), false);
                    recompile(&weak);
                }
                Err(error) => status(&weak, error, false),
            }
            super::paint(&weak, &held);
        }
    });
    ui.global::<ToolchainLogic>().on_add_import({
        let weak = weak.clone();
        let held = held.clone();
        move |folder| {
            let workspace = held.lock().unwrap().workspace.clone();
            match super::foundry_import::add(&workspace, folder.as_str()) {
                Ok(count) => {
                    status(&weak, format!("Added a Foundry build with {count} contracts. Traces map them from now on."), false);
                    if let Some(ui) = weak.upgrade() {
                        ui.global::<ToolchainData>().set_import_input("".into());
                    }
                }
                Err(error) => status(&weak, error, false),
            }
            super::paint(&weak, &held);
        }
    });
    ui.global::<ToolchainLogic>().on_remove_import({
        let weak = weak.clone();
        let held = held.clone();
        move |folder| {
            let workspace = held.lock().unwrap().workspace.clone();
            match super::foundry_import::remove(&workspace, folder.as_str()) {
                Ok(()) => status(&weak, format!("Removed {folder}."), false),
                Err(error) => status(&weak, error, false),
            }
            super::paint(&weak, &held);
        }
    });
    ui.global::<ToolchainLogic>().on_save_settings({
        let weak = weak.clone();
        let held = held.clone();
        move |optimizer, runs, via_ir, evm_version, bytecode_hash| {
            let workspace = held.lock().unwrap().workspace.clone();
            let optimizer = match optimizer { 1 => Some(true), 2 => Some(false), _ => None };
            match toolchain::save_compiler_settings(&workspace, optimizer, runs.as_str(), via_ir.then_some(true), evm_version.as_str(), bytecode_hash.as_str()) {
                Ok(summary) => {
                    status(&weak, format!("Compiler settings: {summary}. Recompiling…"), false);
                    recompile(&weak);
                }
                Err(error) => status(&weak, error, false),
            }
            super::paint(&weak, &held);
        }
    });
    ui.global::<ToolchainLogic>().on_lock_solc(move |version| {
        let workspace = held.lock().unwrap().workspace.clone();
        match toolchain::lock_solc(&workspace, version.as_str()) {
            Ok(()) => {
                let text = if version.trim().is_empty() { "Compiler set to Auto. Recompiling…".to_string() } else { format!("Locked to solc {}. Recompiling (a missing version is downloaded first)…", version.trim()) };
                status(&weak, text, false);
                recompile(&weak);
            }
            Err(error) => status(&weak, error, false),
        }
        super::paint(&weak, &held);
    });
    let weak = ui.as_weak();
    let held = state.clone();
    let rt = runtime.clone();
    ui.global::<ToolchainLogic>().on_install(move |id, tag| {
        let Some(library) = toolchain::library(id.as_str()) else { return };
        let tag = if tag.trim().is_empty() { library.default_tag.to_string() } else { tag.trim().to_string() };
        status(&weak, format!("Downloading {} {tag}…", library.title), true);
        let weak = weak.clone();
        let held = held.clone();
        let id = id.to_string();
        rt.spawn(async move {
            let result = tokio::task::spawn_blocking({
                let id = id.clone();
                let tag = tag.clone();
                move || toolchain::install(&id, &tag)
            })
            .await
            .unwrap_or_else(|_| Err("The download task failed.".into()));
            let _ = slint::invoke_from_event_loop(move || {
                let text = match result {
                    Ok(path) => format!("Installed {} {tag} in {}. Press Use to map it in this project.", library.title, path.display()),
                    Err(error) => error,
                };
                status(&weak, text, false);
                if let Some(ui) = weak.upgrade() {
                    ui.global::<ToolchainData>().set_tag_input(tag.into());
                }
                super::paint(&weak, &held);
            });
        });
    });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<ToolchainLogic>().on_use_version(move |id, tag| {
        let workspace = held.lock().unwrap().workspace.clone();
        let tag = if tag.trim().is_empty() { toolchain::installed(id.as_str()).into_iter().next().unwrap_or_default() } else { tag.trim().to_string() };
        match toolchain::use_in_project(&workspace, id.as_str(), &tag) {
            Ok(remapping) => {
                status(&weak, format!("Mapped {remapping}. Recompiling…"), false);
                recompile(&weak);
            }
            Err(error) => status(&weak, error, false),
        }
        super::paint(&weak, &held);
    });
    let weak = ui.as_weak();
    let held = state.clone();
    let rt = runtime.clone();
    // Legacy alias behind the API's `toolchain.action network`: save the URL
    // onto the active slot, preflight it, then reopen.
    ui.global::<ToolchainLogic>().on_save_network(move |url, block| {
        let Some(ui) = weak.upgrade() else { return };
        if let Err(error) = start_switch(&ui, &held, &rt, false, |root| toolchain::prepare_network(root, url.as_str(), block.as_str())) {
            status(&weak, error, ui.global::<ToolchainData>().get_busy());
        }
    });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<ToolchainLogic>().on_profile_select(move |id| {
        if let Some(ui) = weak.upgrade() {
            ui.global::<ToolchainData>().set_selected_profile(id.clone());
        }
        super::paint(&weak, &held);
    });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<ToolchainLogic>().on_profile_add(move |name| {
        let workspace = held.lock().unwrap().workspace.clone();
        let name = name.trim().to_string();
        let (profiles, ..) = toolchain::load_network(&workspace);
        let mut id = profile_slug(&name);
        while profiles.iter().any(|profile| profile.id == id) {
            id = format!("{id}-2");
        }
        if let Some(ui) = weak.upgrade() {
            let data = ui.global::<ToolchainData>();
            data.set_selected_profile(id.clone().into());
            data.set_profile_name(if name.is_empty() { id.clone().into() } else { name.into() });
            data.set_fork_url("".into());
            data.set_fork_block("".into());
            data.set_chain_mode(0);
            // The form was filled here; the render pass must not refill it.
            PROFILE_SHOWN.with(|shown| *shown.borrow_mut() = id.clone());
        }
        status(&weak, format!("Profile {id} created. Fill the RPC below and press Save profile."), false);
        super::paint(&weak, &held);
    });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<ToolchainLogic>().on_profile_duplicate(move |id| {
        let workspace = held.lock().unwrap().workspace.clone();
        let (profiles, ..) = toolchain::load_network(&workspace);
        let Some(source) = profiles.iter().find(|profile| profile.id == id.as_str()) else { return status(&weak, format!("No profile named {id}."), false) };
        let mut clone = source.clone();
        clone.id = String::new();
        for attempt in 0.. {
            let candidate = if attempt == 0 { format!("{}-copy", source.id) } else { format!("{}-copy-{}", source.id, attempt + 1) };
            if !profiles.iter().any(|profile| profile.id == candidate) {
                clone.id = candidate;
                break;
            }
        }
        clone.name = format!("Copy of {}", source.title());
        match toolchain::save_profile(&workspace, clone.clone()) {
            Ok(_) => {
                if let Some(ui) = weak.upgrade() {
                    ui.global::<ToolchainData>().set_selected_profile(clone.id.clone().into());
                    PROFILE_SHOWN.with(|shown| *shown.borrow_mut() = clone.id.clone());
                }
                status(&weak, format!("Saved profile {}. Switch runs it.", clone.id), false);
                super::paint(&weak, &held);
            }
            Err(error) => status(&weak, error, false),
        }
    });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<ToolchainLogic>().on_profile_remove(move |id| {
        let workspace = held.lock().unwrap().workspace.clone();
        match toolchain::remove_profile(&workspace, id.as_str()) {
            Ok(()) => {
                status(&weak, format!("Removed profile {id}. The running network is untouched."), false);
                super::paint(&weak, &held);
            }
            Err(error) => status(&weak, error, false),
        }
    });
    let weak = ui.as_weak();
    let held = state.clone();
    let rt = runtime.clone();
    ui.global::<ToolchainLogic>().on_profile_save(move |id, name, url, block, mode| {
        let state = held.lock().unwrap();
        if !state.can_change_workspace() {
            drop(state);
            return status(&weak, "Wait for the current operation to finish, then save again.".into(), false);
        }
        let workspace = state.workspace.clone();
        let running = state.network.profile_id.clone();
        drop(state);
        let id = id.trim().to_string();
        let url = url.trim().to_string();
        let block = block.trim().to_string();
        let mode = crate::config::ChainIdMode::parse(mode.as_str()).unwrap_or(crate::config::ChainIdMode::Local);
        // Editing the built-in local chain with a URL is the legacy fork save:
        // it fills the active slot as fork-1 with chain id Local.
        if id.is_empty() || id == crate::config::NetworkProfile::LOCAL_ID {
            if url.is_empty() {
                return status(&weak, "The local chain is built in. Give a profile a name above and press New profile to fork on top of it.".into(), false);
            }
            let Some(ui) = weak.upgrade() else { return };
            if let Err(error) = start_switch(&ui, &held, &rt, false, |root| toolchain::prepare_network(root, &url, &block)) {
                status(&weak, error, false);
            }
            return;
        }
        let (profiles, ..) = toolchain::load_network(&workspace);
        let saved = profiles.iter().find(|profile| profile.id == id);
        let fork_url_env = saved.map(|profile| profile.fork_url_env.clone()).unwrap_or_default();
        let fork_block = if block.is_empty() {
            None
        } else {
            match block.parse::<u64>() {
                Ok(block) => Some(block),
                Err(_) => return status(&weak, "Enter the fork block as a whole number, or leave it empty for the latest block.".into(), false),
            }
        };
        let profile = crate::config::NetworkProfile {
            id: id.clone(), name: name.trim().to_string(), fork_url: url, fork_url_env, fork_block, chain_id_mode: mode,
            hardfork: saved.map(|profile| profile.hardfork.clone()).unwrap_or_default(),
            optimism: saved.and_then(|profile| profile.optimism),
        };
        let Some(ui) = weak.upgrade() else { return };
        ui.global::<ToolchainLogic>().invoke_cancel_fresh();
        if profile.id == running {
            if let Err(error) = start_switch(&ui, &held, &rt, false, |root| toolchain::NetworkSwitch::prepare(root, &id, Some(profile))) {
                status(&weak, error, false);
            }
        } else {
            match toolchain::save_profile(&workspace, profile) {
                Ok(profile) => status(&weak, format!("Saved profile {}. Use Switch to run it.", profile.id), false),
                Err(error) => status(&weak, error, false),
            }
            super::paint(&weak, &held);
        }
    });
    let weak = ui.as_weak();
    let held = state.clone();
    let rt = runtime.clone();
    ui.global::<ToolchainLogic>().on_profile_switch({
        let pending = pending.clone();
        move |id, fresh| {
            let Some(ui) = weak.upgrade() else { return };
            ui.global::<ToolchainLogic>().invoke_cancel_fresh();
            if !fresh {
                if let Err(error) = start_switch(&ui, &held, &rt, false, |root| toolchain::NetworkSwitch::prepare(root, id.as_str(), None)) {
                    status(&weak, error, ui.global::<ToolchainData>().get_busy());
                }
                return;
            }
            let state = held.lock().unwrap();
            if !state.workspace_ready || !state.can_change_workspace() { return; }
            let plan = match toolchain::NetworkSwitch::prepare(&state.workspace, id.as_str(), None) {
                Ok(plan) => plan,
                Err(error) => return status(&weak, error, false),
            };
            *pending.borrow_mut() = Some(FreshConfirmation { workspace: state.workspace.clone(), target: id.to_string(), plan, expires: Instant::now() + CONFIRM_TIMEOUT });
            ui.global::<ToolchainData>().set_fresh_target(id);
            let weak = weak.clone();
            expiry.start(slint::TimerMode::SingleShot, CONFIRM_TIMEOUT, move || {
                if let Some(ui) = weak.upgrade() { ui.global::<ToolchainLogic>().invoke_cancel_fresh(); }
            });
        }
    });
    let weak = ui.as_weak();
    let held = state.clone();
    let rt = runtime.clone();
    ui.global::<ToolchainLogic>().on_confirm_fresh(move |id| {
        let Some(ui) = weak.upgrade() else { return };
        let state = held.lock().unwrap();
        let confirmed = FreshConfirmation::consume(&mut pending.borrow_mut(), &state.workspace, id.as_str(), Instant::now());
        drop(state);
        ui.global::<ToolchainLogic>().invoke_cancel_fresh();
        let Some(plan) = confirmed else { return };
        if let Err(error) = start_switch(&ui, &held, &rt, true, |root| { plan.checked_config(root)?; Ok(plan) }) {
            status(&weak, error, ui.global::<ToolchainData>().get_busy());
        }
    });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<ToolchainLogic>().on_test_rpc({
        let weak = weak.clone();
        let rt = runtime.clone();
        move |url, block| {
            status(&weak, "Testing the RPC…".into(), true);
            let weak = weak.clone();
            let (url, block) = (url.to_string(), block.to_string());
            let evm = super::toolchain::compiler_settings(&held.lock().unwrap().workspace).evm_version;
            rt.spawn(async move {
                let result = toolchain::test_rpc(&url, &block, (!evm.is_empty()).then_some(evm.as_str())).await;
                let _ = slint::invoke_from_event_loop(move || {
                    let text = match result {
                        Ok(lines) => format!("RPC test:\n{}", lines.join("\n")),
                        Err(error) => error,
                    };
                    status(&weak, text, false);
                });
            });
        }
    });
    ui.global::<ToolchainLogic>().on_preset(move |index| {
        let Some((name, url, _)) = usize::try_from(index).ok().and_then(|index| toolchain::PUBLIC_RPCS.get(index)) else { return };
        if let Some(ui) = weak.upgrade() {
            let data = ui.global::<ToolchainData>();
            data.set_fork_url((*url).into());
            data.set_status(format!("{name} public RPC filled in. Save and reopen to fork it.").into());
        }
    });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<ToolchainLogic>().on_remove(move |id| {
        let workspace = held.lock().unwrap().workspace.clone();
        match toolchain::remove_from_project(&workspace, id.as_str()) {
            Ok(()) => {
                status(&weak, "Removed the remapping. The downloaded copy stays for other projects. Recompiling…".into(), false);
                recompile(&weak);
            }
            Err(error) => status(&weak, error, false),
        }
        super::paint(&weak, &held);
    });
}

/// Reserve the workspace synchronously, before another UI or API request can
/// start a second preflight, change the workspace, or mutate the running node.
pub(super) fn start_switch(ui: &MainWindow, held: &State, runtime: &tokio::runtime::Handle, fresh: bool,
    prepare: impl FnOnce(&Path) -> Result<toolchain::NetworkSwitch, String>,
) -> Result<(), String> {
    ui.global::<ToolchainLogic>().invoke_cancel_fresh();
    let mut state = held.lock().unwrap();
    if !state.workspace_ready || !state.can_change_workspace() || state.pending_workspace.is_some() {
        return Err("Wait for the current operation to finish, then switch again.".into());
    }
    let lease = state.mutation_gate.try_acquire().ok_or_else(|| "Wait for the current chain operation to finish, then switch again.".to_string())?;
    let plan = prepare(&state.workspace)?;
    state.workspace_busy = true;
    let workspace = state.workspace.clone();
    drop(state);
    let message = if plan.settings.is_local() { "Switching to a local chain…".into() } else { format!("Testing the RPC of profile {}…", plan.settings.profile_id) };
    ui.global::<super::AppData>().set_workspace_busy(true);
    status(&ui.as_weak(), message, true);
    let weak = ui.as_weak();
    let held = held.clone();
    runtime.spawn(async move {
        let result = preflight(&held, &plan).await;
        let _ = slint::invoke_from_event_loop(move || {
            let _lease = lease; // Keep the checkpoint stable through config commit or abort.
            let mut state = held.lock().unwrap();
            if !state.active || !state.workspace_busy || state.workspace != workspace { return; }
            let result = result.and_then(|warnings| commit_switch(&workspace, &plan, fresh).map(|()| warnings));
            match result {
                Ok(warnings) => {
                    let settings = &plan.settings;
                    super::network::write_switch(&workspace, &state.network.profile_id, &settings.profile_id);
                    state.profile_state_saved = true; // No second save on close, including a held-state skip.
                    state.next_workspace = Some(workspace);
                    state.wallet_generation = state.wallet_generation.wrapping_add(1);
                    state.status_left = if settings.is_local() { "Reopening with a local chain…".into() } else {
                        let warn = if warnings.is_empty() { String::new() } else { format!(" ({})", warnings.join("; ")) };
                        format!("Reopening as profile {} (fork of {}){warn}…", settings.profile_id, super::fork_host(&settings.fork_url))
                    };
                    state.active = false;
                    drop(state);
                    let _ = slint::quit_event_loop();
                }
                Err(error) => {
                    let text = format!("Not switched: {error} The current network keeps running.");
                    state.workspace_busy = false;
                    state.notice = text.clone();
                    state.status_left = "Network switch cancelled".into();
                    drop(state);
                    status(&weak, text, false);
                    super::paint(&weak, &held);
                }
            }
        });
    });
    Ok(())
}

async fn preflight(held: &State, plan: &toolchain::NetworkSwitch) -> Result<Vec<String>, String> {
    let warnings = plan.test_rpc().await?.into_iter()
        .filter(|line| line.starts_with("no ") || line.starts_with("-- ")).collect();
    let saved = super::network::save_running_profile(held).await;
    require_save(saved, held.lock().unwrap().profile_state_held)?;
    Ok(warnings)
}

fn require_save(result: Result<bool, String>, held: bool) -> Result<(), String> {
    match result {
        Ok(true) => Ok(()),
        Ok(false) if held => Ok(()), // Never overwrite the held files with the current node.
        Ok(false) => Err("The running network's state was not saved.".into()),
        Err(error) => Err(format!("Could not save the running network: {error}.")),
    }
}

fn commit_switch(workspace: &Path, plan: &toolchain::NetworkSwitch, fresh: bool) -> Result<(), String> {
    let id = &plan.settings.profile_id;
    super::network::profile_dir(workspace, id)?;
    let config = plan.checked_config(workspace)?;
    let archive = if fresh {
        super::network::archive_for_fresh(workspace, id).map_err(|error| format!("Could not move the target's saved state aside: {error}."))?
    } else { None };
    if let Err(error) = config.save(workspace) {
        if let Some(archive) = archive {
            if let Err(restore) = super::network::restore_archived_profile(workspace, id, &archive) {
                return Err(format!("Could not save the configuration ({error}) or restore {archive} ({restore}). The archive is under .chasm/networks/."));
            }
        }
        return Err(format!("Could not save the network configuration: {error}."));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_confirmation_is_scoped_expiring_and_single_use() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let now = Instant::now();
        let arm = || Some(FreshConfirmation {
            workspace: a.path().into(), target: "local".into(), expires: now + CONFIRM_TIMEOUT,
            plan: toolchain::NetworkSwitch::prepare(a.path(), "local", None).unwrap(),
        });
        for (root, target, at) in [(b.path(), "local", now), (a.path(), "other", now), (a.path(), "local", now + CONFIRM_TIMEOUT)] {
            let mut pending = arm();
            assert!(FreshConfirmation::consume(&mut pending, root, target, at).is_none());
            assert!(pending.is_none());
        }
        let mut pending = arm();
        assert!(FreshConfirmation::consume(&mut pending, a.path(), "local", now).is_some());
        assert!(FreshConfirmation::consume(&mut pending, a.path(), "local", now).is_none());
    }

    #[test]
    fn repeated_requests_cancel_and_reopen_never_confirm_a_clean_switch() {
        i_slint_backend_testing::init_no_event_loop();
        let root = tempfile::tempdir().unwrap();
        let session = || Arc::new(Mutex::new(DesktopState::new(root.path().into(), 1.0)));
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let ui = MainWindow::new().unwrap();
        let first = session();
        wire(&ui, &first, runtime.handle().clone());
        let logic = ui.global::<ToolchainLogic>();
        let data = ui.global::<ToolchainData>();
        logic.invoke_profile_switch("local".into(), true);
        logic.invoke_profile_switch("local".into(), true);
        assert_eq!(data.get_fresh_target(), "local");
        assert!(!first.lock().unwrap().workspace_busy);
        logic.invoke_cancel_fresh();
        logic.invoke_confirm_fresh("local".into());
        assert!(!first.lock().unwrap().workspace_busy);

        logic.invoke_profile_switch("local".into(), true);
        let reopened = session();
        wire(&ui, &reopened, runtime.handle().clone());
        assert!(data.get_fresh_target().is_empty());
        logic.invoke_confirm_fresh("local".into());
        assert!(!reopened.lock().unwrap().workspace_busy);
        // The runtime is never driven. An explicit confirmation alone must
        // reserve the workspace before a second caller can start any RPC.
        logic.invoke_profile_switch("local".into(), true);
        logic.invoke_confirm_fresh("local".into());
        assert!(reopened.lock().unwrap().workspace_busy);
        assert!(start_switch(&ui, &reopened, runtime.handle(), false, |_| panic!("two preflights")).is_err());
    }

    #[test]
    fn only_a_written_or_held_checkpoint_allows_switching() {
        assert!(require_save(Ok(true), false).is_ok());
        assert!(require_save(Ok(false), true).is_ok());
        assert!(require_save(Ok(false), false).is_err());
        assert!(require_save(Err("save failed".into()), true).is_err());
        assert!(require_save(Err("save failed".into()), false).is_err());
    }

    #[test]
    fn clean_switch_archives_partial_state_but_aborts_on_an_invalid_target_folder() {
        let root = tempfile::tempdir().unwrap();
        let plan = toolchain::prepare_network(root.path(), "https://rpc.invalid", "").unwrap();
        let dir = super::super::network::profile_dir(root.path(), "fork-1").unwrap();
        std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
        std::fs::write(&dir, b"not a directory").unwrap();
        assert!(commit_switch(root.path(), &plan, true).is_err());
        assert!(!root.path().join(".chasm/chasm.toml").exists());
        assert_eq!(std::fs::read(&dir).unwrap(), b"not a directory");

        let partial = tempfile::tempdir().unwrap();
        let plan = toolchain::prepare_network(partial.path(), "https://rpc.invalid", "").unwrap();
        let dir = super::super::network::profile_dir(partial.path(), "fork-1").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("deployments.json"), b"[]").unwrap();
        commit_switch(partial.path(), &plan, true).unwrap();
        assert!(!dir.exists());
        let archive = dir.parent().unwrap().read_dir().unwrap().next().unwrap().unwrap();
        assert!(archive.file_name().to_string_lossy().starts_with("fork-1.discarded-"));
        assert_eq!(std::fs::read(archive.path().join("deployments.json")).unwrap(), b"[]");
    }
}
