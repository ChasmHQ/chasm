//! Compiler and libraries view. Installs run off the UI thread; any change
//! to the compiler or remappings triggers a recompile.
use super::session::DesktopState;
use super::toolchain;
use super::{LibraryRow, MainWindow, NetworkProfileRow, PragmaRow, ToolchainData, ToolchainLogic};
use slint::{ComponentHandle, Weak};
use std::sync::{Arc, Mutex};

type State = Arc<Mutex<DesktopState>>;

thread_local! {
    /// The `[compiler]` settings last copied into the form.
    static SETTINGS_SHOWN: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
    /// The network profile the form was last filled from, so typing in the
    /// form never fights a repaint.
    static PROFILE_SHOWN: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

pub fn render(ui: &MainWindow, state: &DesktopState) {
    // Disk scans only while the view is open.
    if state.current_doc_kind() != 8 {
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
        let state = held.lock().unwrap();
        if !state.can_change_workspace() {
            drop(state);
            return status(&weak, "Wait for the current operation to finish, then save again.".into(), false);
        }
        let workspace = state.workspace.clone();
        drop(state);
        match toolchain::save_network(&workspace, url.as_str(), block.as_str()) {
            Ok(settings) => {
                let id = settings.profile_id.clone();
                rt.spawn(preflight_and_reopen(weak.clone(), held.clone(), id, false));
            }
            Err(error) => status(&weak, error, false),
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
            return match toolchain::save_network(&workspace, &url, &block) {
                Ok(settings) => {
                    let id = settings.profile_id.clone();
                    rt.spawn(preflight_and_reopen(weak.clone(), held.clone(), id, false));
                    status(&weak, "Saved. Testing the RPC before the project reopens…".into(), true)
                }
                Err(error) => status(&weak, error, false),
            };
        }
        let (profiles, ..) = toolchain::load_network(&workspace);
        let fork_url_env = profiles.iter().find(|profile| profile.id == id).map(|profile| profile.fork_url_env.clone()).unwrap_or_default();
        let fork_block = if block.is_empty() {
            None
        } else {
            match block.parse::<u64>() {
                Ok(block) => Some(block),
                Err(_) => return status(&weak, "Enter the fork block as a whole number, or leave it empty for the latest block.".into(), false),
            }
        };
        let profile = crate::config::NetworkProfile { id: id.clone(), name: name.trim().to_string(), fork_url: url, fork_url_env, fork_block, chain_id_mode: mode };
        match toolchain::save_profile(&workspace, profile) {
            Ok(profile) => {
                if profile.id == running {
                    rt.spawn(preflight_and_reopen(weak.clone(), held.clone(), profile.id.clone(), false));
                    status(&weak, format!("Saved profile {}. Testing the RPC before the project reopens…", profile.id), true);
                } else {
                    status(&weak, format!("Saved profile {}. Use Switch to run it.", profile.id), false);
                }
                super::paint(&weak, &held);
            }
            Err(error) => status(&weak, error, false),
        }
    });
    let weak = ui.as_weak();
    let held = state.clone();
    let rt = runtime.clone();
    ui.global::<ToolchainLogic>().on_profile_switch(move |id, fresh| {
        let state = held.lock().unwrap();
        if !state.can_change_workspace() {
            drop(state);
            return status(&weak, "Wait for the current operation to finish, then switch again.".into(), false);
        }
        drop(state);
        rt.spawn(preflight_and_reopen(weak.clone(), held.clone(), id.to_string(), fresh));
    });
    let weak = ui.as_weak();
    ui.global::<ToolchainLogic>().on_test_rpc({
        let weak = weak.clone();
        let rt = runtime.clone();
        move |url, block| {
            status(&weak, "Testing the RPC…".into(), true);
            let weak = weak.clone();
            let (url, block) = (url.to_string(), block.to_string());
            rt.spawn(async move {
                let result = toolchain::test_rpc(&url, &block).await;
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

/// Switch to profile `id` (or the local chain on an empty id): preflight the
/// target RPC before anything is torn down, then make it active and reopen
/// the project. When the RPC fails, nothing changes and the current network
/// keeps running. `fresh` also discards the profile's saved state (N3 fills
/// that folder; today nothing is stored under it).
pub(super) async fn preflight_and_reopen(weak: Weak<MainWindow>, held: State, id: String, fresh: bool) {
    let workspace = held.lock().unwrap().workspace.clone();
    let settings = match toolchain::target_network(&workspace, &id) {
        Ok(settings) => settings,
        Err(error) => {
            // UI state is touched only on the event loop thread.
            let _ = slint::invoke_from_event_loop(move || status(&weak, error, false));
            return;
        }
    };
    let message = if settings.is_local() { "Switching to the local chain…".to_string() } else { format!("Testing the RPC of profile {}…", settings.profile_id) };
    let _ = slint::invoke_from_event_loop({
        let weak = weak.clone();
        move || status(&weak, message, true)
    });
    let mut warnings = Vec::new();
    if !settings.is_local() {
        let block = settings.fork_block.map(|block| block.to_string()).unwrap_or_default();
        match toolchain::test_rpc(&settings.fork_url, &block).await {
            Err(error) => {
                let text = format!("Not switched: the RPC of profile {} did not pass its test: {error} The current network keeps running.", settings.profile_id);
                let _ = slint::invoke_from_event_loop(move || {
                    status(&weak, text, false);
                    super::paint(&weak, &held);
                });
                return;
            }
            Ok(lines) => warnings = lines.into_iter().filter(|line| line.starts_with("no ") || line.starts_with("-- ")).collect(),
        }
    }
    let _ = slint::invoke_from_event_loop(move || {
        let mut state = held.lock().unwrap();
        if !state.can_change_workspace() {
            drop(state);
            return status(&weak, "Wait for the current operation to finish, then switch again.".into(), false);
        }
        // The marker lets boot fall back to this network when the target
        // never becomes ready.
        let current = state.network.profile_id.clone();
        match toolchain::set_active_network(&state.workspace, id.trim()) {
            Ok(_) => {
                if fresh {
                    let _ = std::fs::remove_dir_all(state.workspace.join(".chasm").join("networks").join(id.trim()));
                }
                super::network::write_switch(&state.workspace, &current, id.trim());
                state.next_workspace = Some(state.workspace.clone());
                state.wallet_generation = state.wallet_generation.wrapping_add(1);
                state.workspace_busy = true;
                state.status_left = if settings.is_local() {
                    "Reopening with a local chain…".into()
                } else {
                    let warn = if warnings.is_empty() { String::new() } else { format!(" ({})", warnings.join("; ")) };
                    format!("Reopening as profile {} (fork of {}){warn}…", settings.profile_id, super::fork_host(&settings.fork_url))
                };
                state.active = false;
                drop(state);
                let _ = slint::quit_event_loop();
            }
            Err(error) => {
                drop(state);
                status(&weak, error, false);
            }
        }
    });
}
