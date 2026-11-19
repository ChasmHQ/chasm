//! Compiler and libraries view. Installs run off the UI thread; any change
//! to the compiler or remappings triggers a recompile.
use super::session::DesktopState;
use super::toolchain;
use super::{LibraryRow, MainWindow, PragmaRow, ToolchainData, ToolchainLogic};
use slint::{ComponentHandle, Weak};
use std::sync::{Arc, Mutex};

type State = Arc<Mutex<DesktopState>>;

thread_local! {
    /// The `[compiler]` settings last copied into the form.
    static SETTINGS_SHOWN: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
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
    let describe = |network: &crate::config::NetworkConfig| {
        if network.is_local() {
            "a fresh local chain".to_string()
        } else {
            // The running fork knows its chain id; a saved one is named by its preset.
            let chain = Some(network)
                .filter(|network| *network == &state.network)
                .and(state.fork_chain_id)
                .map(toolchain::chain_name)
                .or_else(|| toolchain::PUBLIC_RPCS.iter().find(|(_, url, _)| *url == network.fork_url).map(|(name, _, _)| name.to_string()))
                .unwrap_or_else(|| super::fork_host(&network.fork_url));
            format!("a fork of {chain} at {}", network.fork_block.map(|block| format!("block {block}")).unwrap_or_else(|| "the latest block".into()))
        }
    };
    let saved = crate::config::ChasmConfig::load(workspace).map(|config| config.network).unwrap_or_default();
    data.set_fork_running(describe(&state.network).into());
    data.set_presets(super::model(toolchain::PUBLIC_RPCS.iter().map(|(name, _, _)| slint::SharedString::from(*name)).collect()));
    data.set_fork_saved(describe(&saved).into());
    // Fill the form once from the saved setting; afterwards it is the user's text.
    if data.get_fork_url().is_empty() && data.get_fork_block().is_empty() && !saved.is_local() {
        data.set_fork_url(saved.fork_url.clone().into());
        data.set_fork_block(saved.fork_block.map(|block| block.to_string()).unwrap_or_default().into());
    }
    data.set_lib_folder(crate::config::global_lib_dir().display().to_string().into());
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
    ui.global::<ToolchainLogic>().on_save_network(move |url, block| {
        let mut state = held.lock().unwrap();
        if !state.can_change_workspace() {
            drop(state);
            return status(&weak, "Wait for the current operation to finish, then save again.".into(), false);
        }
        match toolchain::save_network(&state.workspace, url.as_str(), block.as_str()) {
            Ok(network) => {
                // Reopen the same project: boot reads [network] and starts the node.
                let path = state.workspace.clone();
                state.next_workspace = Some(path);
                state.workspace_busy = true;
                state.wallet_generation = state.wallet_generation.wrapping_add(1);
                state.status_left = if network.is_local() { "Reopening with a local chain…".into() } else { format!("Reopening as a fork of {}…", super::fork_host(&network.fork_url)) };
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
    let weak = ui.as_weak();
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
