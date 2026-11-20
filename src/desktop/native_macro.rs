use super::macro_model::{
    self, AbiCallMode, MacroDefinition, MacroId, PreflightSummary, RunId, Signer, StepKind,
    ValidationError, MACRO_SCHEMA_VERSION, MAX_MACRO_JSON_BYTES,
};
use super::macro_runner::{self, ExecutionToken, RunReport, StepStatus};
use super::{
    DesktopState, Logic, MacroData, MacroLogic, MacroResult, MacroRow, MacroStep, MainWindow,
};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use slint::ComponentHandle;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio_util::sync::CancellationToken;

type State = Arc<Mutex<DesktopState>>;

const LIBRARY_VERSION: u32 = 1;
const LIBRARY_FILE: &str = "desktop-macro-library.json";
const LEGACY_FILE: &str = "desktop-macros.json";
const MAX_LIBRARY_BYTES: usize = 8 * 1024 * 1024;
const MAX_LIBRARY_ITEMS: usize = 128;
const MAX_RUNS: usize = 20;
const MAX_RUN_BYTES: usize = 256 * 1024;
const MAX_STEP_OUTPUT_BYTES: usize = 4096;
const MAX_UI_ERRORS: usize = 24;
const MAX_WINDOW_LOG_BYTES: usize = 16 * 1024;

/// Platform implementations supply bytes selected by the user, never a path for
/// this controller to open. Messages from platform errors are intentionally ignored.
pub trait MacroPlatform: Send + Sync {
    fn import_definition(&self) -> Result<Option<Vec<u8>>, ()>;
    fn export_definition(&self, json: &str) -> Result<bool, ()>;
    fn save_evidence(&self, json: &str) -> Result<bool, ()>;
}

/// An in-process adapter must validate ownership, epochs and signer authority at
/// the side-effect boundary. A dry run must follow the runner's no-mutation rules.
/// The ordered IDs map enabled model steps to the runner's consecutive step IDs.
pub trait MacroExecutor: Send + Sync {
    fn execute(
        &self,
        request: ExecutionRequest,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<ExecutionOutput, ExecutionFailure>>;
}

pub struct ExecutionRequest {
    pub definition: MacroDefinition,
    pub dry_run: bool,
    pub token: ExecutionToken,
    pub owned_node: Arc<super::owned_node::OwnedNode>,
}

pub struct ExecutionOutput {
    pub report: RunReport,
    pub step_ids: Vec<macro_model::StepId>,
}

#[derive(Clone, Copy, Debug)]
pub enum ExecutionFailure {
    Unavailable,
    InvalidPlan,
    StaleContext,
    Failed,
}

#[derive(Clone)]
struct Record {
    definition: MacroDefinition,
    draft: String,
}

#[derive(Clone)]
struct ValidationSnapshot {
    id: MacroId,
    json: String,
    revision: u64,
    definition: Option<MacroDefinition>,
    result: Result<PreflightSummary, Vec<ValidationError>>,
}

struct ActiveRun {
    id: RunId,
    macro_id: MacroId,
    json: String,
    revision: u64,
    workspace_epoch: u64,
    network_epoch: u64,
    token: ExecutionToken,
    owned_node: Arc<super::owned_node::OwnedNode>,
    definition: MacroDefinition,
    dry_run: bool,
    cancel: CancellationToken,
    started: Instant,
}

#[derive(Clone)]
struct StoredRun {
    id: RunId,
    macro_id: MacroId,
    json: String,
    report: RunReport,
    step_ids: Vec<macro_model::StepId>,
    elapsed_ms: u128,
    evidence_saved: bool,
}

struct Library {
    records: Vec<Record>,
    selected: Option<MacroId>,
    revision: u64,
    validation: Option<ValidationSnapshot>,
    runs: VecDeque<StoredRun>,
    selected_run: Option<RunId>,
    active_run: Option<ActiveRun>,
    writable: bool,
    failed: bool,
    error: String,
    status: String,
}

impl Default for Library {
    fn default() -> Self {
        Self {
            records: Vec::new(),
            selected: None,
            revision: 0,
            validation: None,
            runs: VecDeque::new(),
            selected_run: None,
            active_run: None,
            writable: true,
            failed: false,
            error: String::new(),
            status: String::new(),
        }
    }
}

impl Library {
    fn selected_record(&self) -> Option<&Record> {
        let id = self.selected.as_ref()?;
        self.records
            .iter()
            .find(|record| &record.definition.id == id)
    }

    fn selected_index(&self) -> Option<usize> {
        let id = self.selected.as_ref()?;
        self.records
            .iter()
            .position(|record| &record.definition.id == id)
    }

    fn invalidate(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.validation = None;
    }

    fn set_error(&mut self, text: &str) {
        self.failed = true;
        self.error = text.into();
        self.status.clear();
    }

    fn set_status(&mut self, text: &str) {
        self.failed = false;
        self.error.clear();
        self.status = text.into();
    }

    fn select(&mut self, index: i32) {
        let Some(id) = usize::try_from(index)
            .ok()
            .and_then(|index| self.records.get(index))
            .map(|record| record.definition.id.clone())
        else {
            return;
        };
        self.selected = Some(id);
        self.selected_run = None;
        self.invalidate();
        self.set_status("Selection changed. Validate the current JSON before running.");
    }

    fn edit(&mut self, text: String) {
        self.invalidate();
        let Some(index) = self.selected_index() else {
            return;
        };
        if text.len() > MAX_MACRO_JSON_BYTES {
            self.set_error("Draft exceeds the 1 MiB limit. The previous draft is unchanged.");
            return;
        }
        self.records[index].draft = text;
        self.set_status("Draft changed. Validate to save it to the library.");
    }

    fn validation_current(&self) -> Option<&ValidationSnapshot> {
        let snapshot = self.validation.as_ref()?;
        let record = self.selected_record()?;
        (snapshot.id == record.definition.id
            && snapshot.json == record.draft
            && snapshot.revision == self.revision)
            .then_some(snapshot)
    }

    fn selected_result(&self) -> Option<&StoredRun> {
        let selected = self.selected.as_ref()?;
        let id = self.selected_run.as_ref()?;
        self.runs
            .iter()
            .find(|run| &run.id == id && &run.macro_id == selected)
    }

    fn validate_draft(&mut self) -> Option<(MacroId, String, u64)> {
        self.invalidate();
        let record = self.selected_record()?;
        Some((
            record.definition.id.clone(),
            record.draft.clone(),
            self.revision,
        ))
    }

    fn install_validation(&mut self, snapshot: ValidationSnapshot) -> bool {
        let Some(record) = self.selected_record() else {
            return false;
        };
        if self.revision != snapshot.revision
            || record.definition.id != snapshot.id
            || record.draft != snapshot.json
        {
            return false;
        }
        self.validation = Some(snapshot);
        true
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DiskLibrary {
    version: u32,
    macros: Vec<MacroDefinition>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StorageError {
    Unavailable,
    Invalid,
    UnsafeLegacy,
    TooLarge,
}

fn is_link(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn workspace_dir(root: &Path, create: bool) -> Result<PathBuf, StorageError> {
    if root.as_os_str().is_empty() {
        return Err(StorageError::Unavailable);
    }
    let root_meta = fs::metadata(root).map_err(|_| StorageError::Unavailable)?;
    if !root_meta.is_dir() {
        return Err(StorageError::Unavailable);
    }
    let dir = root.join(".chasm");
    match fs::symlink_metadata(&dir) {
        Ok(metadata) if !is_link(&metadata) && metadata.is_dir() => Ok(dir),
        Ok(_) => Err(StorageError::Unavailable),
        Err(error) if error.kind() == io::ErrorKind::NotFound && create => {
            match fs::create_dir(&dir) {
                Ok(()) => Ok(dir),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    let metadata =
                        fs::symlink_metadata(&dir).map_err(|_| StorageError::Unavailable)?;
                    if !is_link(&metadata) && metadata.is_dir() {
                        Ok(dir)
                    } else {
                        Err(StorageError::Unavailable)
                    }
                }
                Err(_) => Err(StorageError::Unavailable),
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(dir),
        Err(_) => Err(StorageError::Unavailable),
    }
}

fn read_file(path: &Path, limit: usize) -> Result<Option<Vec<u8>>, StorageError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(StorageError::Unavailable),
    };
    if is_link(&metadata) || !metadata.is_file() {
        return Err(StorageError::Unavailable);
    }
    if metadata.len() > limit as u64 {
        return Err(StorageError::TooLarge);
    }
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(limit as u64 + 1).read_to_end(&mut bytes))
        .map_err(|_| StorageError::Unavailable)?;
    if bytes.len() > limit {
        return Err(StorageError::TooLarge);
    }
    Ok(Some(bytes))
}

fn validate_definitions(definitions: &[MacroDefinition]) -> Result<(), StorageError> {
    if definitions.len() > MAX_LIBRARY_ITEMS {
        return Err(StorageError::TooLarge);
    }
    let mut ids = HashSet::new();
    for definition in definitions {
        definition
            .validate_storage()
            .map_err(|_| StorageError::Invalid)?;
        if !ids.insert(definition.id.as_str()) {
            return Err(StorageError::Invalid);
        }
    }
    Ok(())
}

fn decode_library(bytes: &[u8]) -> Result<Vec<MacroDefinition>, StorageError> {
    if bytes.len() > MAX_LIBRARY_BYTES {
        return Err(StorageError::TooLarge);
    }
    let envelope: DiskLibrary = serde_json::from_slice(bytes).map_err(|_| StorageError::Invalid)?;
    if envelope.version != LIBRARY_VERSION {
        return Err(StorageError::Invalid);
    }
    validate_definitions(&envelope.macros)?;
    Ok(envelope.macros)
}

fn save_library(root: &Path, definitions: &[MacroDefinition]) -> Result<(), StorageError> {
    validate_definitions(definitions)?;
    let encoded = serde_json::to_vec_pretty(&DiskLibrary {
        version: LIBRARY_VERSION,
        macros: definitions.to_vec(),
    })
    .map_err(|_| StorageError::Invalid)?;
    if encoded.len() > MAX_LIBRARY_BYTES {
        return Err(StorageError::TooLarge);
    }
    let dir = workspace_dir(root, true)?;
    let destination = dir.join(LIBRARY_FILE);
    if let Some(metadata) = fs::symlink_metadata(&destination).ok() {
        if is_link(&metadata) || !metadata.is_file() {
            return Err(StorageError::Unavailable);
        }
    }
    let mut temporary =
        tempfile::NamedTempFile::new_in(&dir).map_err(|_| StorageError::Unavailable)?;
    temporary
        .write_all(&encoded)
        .and_then(|_| temporary.flush())
        .and_then(|_| temporary.as_file().sync_all())
        .map_err(|_| StorageError::Unavailable)?;
    temporary
        .persist(&destination)
        .map_err(|_| StorageError::Unavailable)?;
    #[cfg(unix)]
    {
        // Once renamed, a directory sync failure cannot be reported as an
        // unchanged save; the file is visible but durability is best effort.
        if let Ok(directory) = File::open(&dir) {
            let _ = directory.sync_all();
        }
    }
    Ok(())
}

fn load_library(root: &Path) -> Result<Vec<MacroDefinition>, StorageError> {
    let dir = workspace_dir(root, false)?;
    if let Some(bytes) = read_file(&dir.join(LIBRARY_FILE), MAX_LIBRARY_BYTES)? {
        return decode_library(&bytes);
    }
    let Some(legacy) = read_file(&dir.join(LEGACY_FILE), MAX_MACRO_JSON_BYTES)? else {
        return Ok(Vec::new());
    };
    let definitions =
        macro_model::migrate_legacy(&legacy).map_err(|_| StorageError::UnsafeLegacy)?;
    validate_definitions(&definitions)?;
    save_library(root, &definitions)?;
    Ok(definitions)
}

fn storage_message(error: StorageError) -> &'static str {
    match error {
        StorageError::UnsafeLegacy => {
            "Legacy notes could not be migrated safely. The original file was not changed."
        }
        StorageError::Invalid => {
            "The macro library is invalid or uses an unsupported version. It was not changed."
        }
        StorageError::TooLarge => "The macro library exceeds a storage limit. It was not changed.",
        StorageError::Unavailable => {
            "The macro library could not be read or saved. Existing files were not changed."
        }
    }
}

fn encode_definition(definition: &MacroDefinition) -> Result<String, StorageError> {
    serde_json::to_string_pretty(definition).map_err(|_| StorageError::Invalid)
}

fn records_from_definitions(
    definitions: Vec<MacroDefinition>,
) -> Result<Vec<Record>, StorageError> {
    definitions
        .into_iter()
        .map(|definition| {
            let draft = encode_definition(&definition)?;
            Ok(Record { definition, draft })
        })
        .collect()
}

struct Controller {
    state: State,
    state_identity: usize,
    runtime: tokio::runtime::Handle,
    root: Option<PathBuf>,
    workspace_epoch: u64,
    library: Mutex<Library>,
    executor: Mutex<Option<Arc<dyn MacroExecutor>>>,
    platform: Mutex<Option<Arc<dyn MacroPlatform>>>,
}

impl Controller {
    fn new(state: State, runtime: tokio::runtime::Handle) -> Self {
        let (root, workspace_epoch, state_identity) = {
            let state = state.lock().unwrap();
            (
                state.workspace_ready.then(|| state.workspace.clone()),
                state.workspace_epoch,
                (&*state as *const DesktopState) as usize,
            )
        };
        let mut library = Library::default();
        match root.as_deref().map(load_library) {
            Some(Ok(definitions)) => match records_from_definitions(definitions) {
                Ok(records) => {
                    library.selected = records.first().map(|record| record.definition.id.clone());
                    library.records = records;
                }
                Err(error) => {
                    library.writable = false;
                    library.set_error(storage_message(error));
                }
            },
            Some(Err(error)) => {
                library.writable = false;
                library.set_error(storage_message(error));
            }
            None => {
                library.writable = false;
                library.set_status("Open a workspace to use the local macro library.");
            }
        }
        Self {
            state,
            state_identity,
            runtime,
            root,
            workspace_epoch,
            library: Mutex::new(library),
            executor: Mutex::new(None),
            platform: Mutex::new(None),
        }
    }

    fn node_available(&self, state: &DesktopState) -> bool {
        state.owned_node.as_ref().is_some_and(|node| node.valid())
    }

    fn node_is_current(&self, state: &DesktopState, run: &ActiveRun) -> bool {
        state
            .owned_node
            .as_ref()
            .is_some_and(|node| node.valid() && Arc::ptr_eq(&run.owned_node, node))
    }

    fn available(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.workspace_ready
            && state.workspace_epoch == self.workspace_epoch
            && !state.transition_pending()
    }

    fn persist(&self, records: &[Record]) -> Result<(), StorageError> {
        let root = self.root.as_deref().ok_or(StorageError::Unavailable)?;
        let definitions: Vec<_> = records
            .iter()
            .map(|record| record.definition.clone())
            .collect();
        save_library(root, &definitions)
    }
}

struct Host {
    key: usize,
    generation: u64,
    weak: slint::Weak<MainWindow>,
}

struct Registry {
    controller: Arc<Controller>,
    hosts: Vec<Host>,
    next_generation: u64,
}

thread_local! {
    static REGISTRIES: RefCell<Vec<Registry>> = const { RefCell::new(Vec::new()) };
    /// Macro id and step index whose form fields are on screen.
    static FORM_STEP: RefCell<(String, i32)> = const { RefCell::new((String::new(), -1)) };
}

fn host_key(ui: &MainWindow) -> usize {
    ui.window() as *const slint::Window as usize
}

fn controller_for(state: &State) -> Option<Arc<Controller>> {
    REGISTRIES.with(|registries| {
        registries
            .borrow()
            .iter()
            .find(|registry| Arc::ptr_eq(&registry.controller.state, state))
            .map(|registry| registry.controller.clone())
    })
}

fn host_current(controller: &Arc<Controller>, key: usize, generation: u64) -> bool {
    REGISTRIES.with(|registries| {
        registries.borrow().iter().any(|registry| {
            Arc::ptr_eq(&registry.controller, controller)
                && registry.hosts.iter().any(|host| {
                    host.key == key
                        && host.generation == generation
                        && host.weak.upgrade().is_some()
                })
        })
    })
}

fn refresh_all(controller: &Arc<Controller>) {
    let hosts = REGISTRIES.with(|registries| {
        let mut registries = registries.borrow_mut();
        let Some(registry) = registries
            .iter_mut()
            .find(|registry| Arc::ptr_eq(&registry.controller, controller))
        else {
            return Vec::new();
        };
        registry.hosts.retain(|host| host.weak.upgrade().is_some());
        registry
            .hosts
            .iter()
            .map(|host| host.weak.clone())
            .collect::<Vec<_>>()
    });
    let (workspace_current, node_available) = {
        let state = controller.state.lock().unwrap();
        (
            state.workspace_ready
                && !state.transition_pending()
                && state.workspace_epoch == controller.workspace_epoch,
            controller.node_available(&state),
        )
    };
    let backend = controller.executor.lock().unwrap().is_some();
    let platform = controller.platform.lock().unwrap().is_some();
    let library = controller.library.lock().unwrap();
    for weak in hosts {
        if let Some(ui) = weak.upgrade() {
            project(
                &ui,
                &library,
                workspace_current,
                node_available,
                backend,
                platform,
            );
        }
    }
}

/// Attach a window to the controller for this desktop session. Repeated calls
/// and floating hosts share one library and one in-flight execution slot.
pub fn wire(ui: &MainWindow, state: &State, runtime: tokio::runtime::Handle) {
    let key = host_key(ui);
    let existing = controller_for(state);
    let controller = existing.unwrap_or_else(|| Arc::new(Controller::new(state.clone(), runtime)));
    let generation = REGISTRIES.with(|registries| {
        let mut registries = registries.borrow_mut();
        for registry in registries.iter_mut() {
            registry
                .hosts
                .retain(|host| host.key != key && host.weak.upgrade().is_some());
        }
        registries.retain(|registry| {
            let active = registry.controller.state.lock().unwrap().active;
            if !active {
                if let Some(run) = registry
                    .controller
                    .library
                    .lock()
                    .unwrap()
                    .active_run
                    .as_ref()
                {
                    run.cancel.cancel();
                }
            }
            active
        });
        let index = registries
            .iter()
            .position(|registry| Arc::ptr_eq(&registry.controller, &controller))
            .unwrap_or_else(|| {
                registries.push(Registry {
                    controller: controller.clone(),
                    hosts: Vec::new(),
                    next_generation: 0,
                });
                registries.len() - 1
            });
        let registry = &mut registries[index];
        registry.next_generation = registry.next_generation.wrapping_add(1);
        let generation = registry.next_generation;
        registry.hosts.push(Host {
            key,
            generation,
            weak: ui.as_weak(),
        });
        generation
    });
    wire_callbacks(ui, key, generation, controller.clone());
    refresh_all(&controller);
}

/// Remove a floating host without cancelling work owned by its session.
pub fn detach_host(ui: &MainWindow) {
    let key = host_key(ui);
    REGISTRIES.with(|registries| {
        for registry in registries.borrow_mut().iter_mut() {
            registry
                .hosts
                .retain(|host| host.key != key && host.weak.upgrade().is_some());
        }
    });
}

/// Cancel in-flight work and release this session's controller during shutdown.
pub fn shutdown(state: &State) {
    REGISTRIES.with(|registries| {
        registries.borrow_mut().retain(|registry| {
            if !Arc::ptr_eq(&registry.controller.state, state) {
                return true;
            }
            if let Some(run) = registry
                .controller
                .library
                .lock()
                .unwrap()
                .active_run
                .as_ref()
            {
                run.cancel.cancel();
            }
            false
        });
    });
}

/// Parent-supplied hooks are optional. Without an executor both run controls stay disabled.
pub fn set_executor(state: &State, executor: Option<Arc<dyn MacroExecutor>>) {
    if let Some(controller) = controller_for(state) {
        if let Some(run) = controller.library.lock().unwrap().active_run.as_ref() {
            run.cancel.cancel();
        }
        *controller.executor.lock().unwrap() = executor;
        refresh_all(&controller);
    }
}

/// Install the executor only when this session has none. Floating hosts re-run
/// wiring; replacing the executor there would cancel the shared in-flight run.
pub fn ensure_executor(state: &State, executor: impl FnOnce() -> Arc<dyn MacroExecutor>) {
    let Some(controller) = controller_for(state) else { return };
    if controller.executor.lock().unwrap().is_some() {
        return;
    }
    set_executor(state, Some(executor()));
}

pub fn set_platform(state: &State, platform: Option<Arc<dyn MacroPlatform>>) {
    if let Some(controller) = controller_for(state) {
        *controller.platform.lock().unwrap() = platform;
        refresh_all(&controller);
    }
}

/// The parent calls this from its ordinary render path while holding DesktopState.
pub fn render(ui: &MainWindow, state: &DesktopState) {
    let key = host_key(ui);
    let controller = REGISTRIES.with(|registries| {
        registries
            .borrow()
            .iter()
            .find(|registry| {
                registry.controller.state_identity == (state as *const DesktopState) as usize
                    && registry
                        .hosts
                        .iter()
                        .any(|host| host.key == key && host.weak.upgrade().is_some())
            })
            .map(|registry| registry.controller.clone())
    });
    if let Some(controller) = controller {
        {
            let library = controller.library.lock().unwrap();
            if let Some(run) = library.active_run.as_ref() {
                if !state.workspace_ready
                    || state.transition_pending()
                    || state.workspace_epoch != run.workspace_epoch
                    || state.network_epoch != run.network_epoch
                    || !controller.node_is_current(state, run)
                {
                    run.cancel.cancel();
                }
            }
        }
        let backend = controller.executor.lock().unwrap().is_some();
        let platform = controller.platform.lock().unwrap().is_some();
        let library = controller.library.lock().unwrap();
        project(
            ui,
            &library,
            state.workspace_ready
                && !state.transition_pending()
                && state.workspace_epoch == controller.workspace_epoch,
            controller.node_available(state),
            backend,
            platform,
        );
    }
}

fn commit_records(controller: &Controller, library: &mut Library, records: Vec<Record>) -> bool {
    if !library.writable {
        library.set_error("The library is read-only until its storage problem is resolved.");
        return false;
    }
    match controller.persist(&records) {
        Ok(()) => {
            library.records = records;
            true
        }
        Err(error) => {
            library.set_error(storage_message(error));
            false
        }
    }
}

fn begin_new(controller: &Arc<Controller>) -> bool {
    if !controller.available() {
        return false;
    }
    let mut library = controller.library.lock().unwrap();
    if library.active_run.is_some() {
        return false;
    }
    library.invalidate();
    if library.records.len() >= MAX_LIBRARY_ITEMS {
        library.set_error("The macro library has reached its item limit.");
        return false;
    }
    let definition = MacroDefinition {
        schema_version: MACRO_SCHEMA_VERSION,
        id: MacroId::new(),
        name: format!("Macro {}", library.records.len() + 1),
        description: String::new(),
        variables: BTreeMap::new(),
        snapshot_before_run: false,
        revert_on_failure: false,
        rollback_probes: Vec::new(),
        bundle: false,
        steps: Vec::new(),
        legacy_notes: Vec::new(),
    };
    let Ok(draft) = encode_definition(&definition) else {
        library.set_error("The new macro could not be encoded.");
        return false;
    };
    let id = definition.id.clone();
    let mut records = library.records.clone();
    records.push(Record { definition, draft });
    if !commit_records(controller, &mut library, records) {
        return false;
    }
    library.selected = Some(id);
    library.selected_run = None;
    library.set_status("Created a local macro. Add steps and validate its JSON.");
    true
}

/// The legacy sidebar action and the Macro workspace New action share this path.
/// The parent should replace its old `DesktopState::new_macro()` callback with it.
pub fn new_macro(ui: &MainWindow, state: &State) {
    let Some(controller) = controller_for(state) else {
        return;
    };
    if begin_new(&controller) {
        {
            let mut state = state.lock().unwrap();
            if !state.transition_pending() {
                state.open_activity(3);
            }
        }
        super::panels::focus_current();
        super::paint(&ui.as_weak(), state);
    }
    refresh_all(&controller);
}

fn duplicate(controller: &Arc<Controller>) {
    if !controller.available() {
        return;
    }
    let mut library = controller.library.lock().unwrap();
    if library.active_run.is_some() {
        return;
    }
    library.invalidate();
    if library.records.len() >= MAX_LIBRARY_ITEMS {
        library.set_error("The macro library has reached its item limit.");
        return;
    }
    let Some(record) = library.selected_record() else {
        return;
    };
    let mut definition = match macro_model::decode_macro(record.draft.as_bytes()) {
        Ok(definition) if definition.id == record.definition.id => definition,
        _ => {
            library.set_error("Fix the selected draft before duplicating it.");
            return;
        }
    };
    definition.id = MacroId::new();
    if definition.name.len() + " copy".len() <= macro_model::MAX_NAME_BYTES {
        definition.name.push_str(" copy");
    }
    let Ok(draft) = encode_definition(&definition) else {
        library.set_error("The copy could not be encoded.");
        return;
    };
    let id = definition.id.clone();
    let mut records = library.records.clone();
    records.push(Record { definition, draft });
    if commit_records(controller, &mut library, records) {
        library.selected = Some(id);
        library.selected_run = None;
        library.set_status("Created a separate copy of the macro.");
    }
}

fn delete(controller: &Arc<Controller>, id: &str) {
    if !controller.available() {
        return;
    }
    let mut library = controller.library.lock().unwrap();
    if library.active_run.is_some() || library.selected.as_ref().map(MacroId::as_str) != Some(id) {
        return;
    }
    library.invalidate();
    let Some(index) = library.selected_index() else {
        return;
    };
    let mut records = library.records.clone();
    records.remove(index);
    if commit_records(controller, &mut library, records) {
        library.selected = library
            .records
            .get(index.min(library.records.len().saturating_sub(1)))
            .map(|record| record.definition.id.clone());
        library.runs.retain(|run| run.macro_id.as_str() != id);
        library.selected_run = None;
        library.set_status("Deleted the selected macro and its session results.");
    }
}

fn id_error() -> ValidationError {
    ValidationError {
        path: "$.id".into(),
        code: "selected_id".into(),
        message: "The macro ID cannot be changed in the editor.".into(),
    }
}

fn storage_error() -> ValidationError {
    ValidationError {
        path: "$".into(),
        code: "storage".into(),
        message: "The validated definition could not be saved. Existing files were not changed."
            .into(),
    }
}

fn validate(controller: &Arc<Controller>) {
    if !controller.available() {
        return;
    }
    let (id, json, revision) = {
        let mut library = controller.library.lock().unwrap();
        if library.active_run.is_some() {
            return;
        }
        let Some(snapshot) = library.validate_draft() else {
            return;
        };
        snapshot
    };
    let parsed = macro_model::decode_macro(json.as_bytes()).and_then(|definition| {
        if definition.id != id {
            Err(vec![id_error()])
        } else {
            Ok(definition)
        }
    });
    let mut library = controller.library.lock().unwrap();
    let unchanged = library.selected_record().is_some_and(|record| {
        record.definition.id == id && record.draft == json && library.revision == revision
    });
    if !unchanged {
        return;
    }
    let (definition, result) = match parsed {
        Ok(definition) => {
            let validation = definition.validate();
            let mut records = library.records.clone();
            if let Some(index) = library.selected_index() {
                records[index].definition = definition.clone();
            }
            if commit_records(controller, &mut library, records) {
                (Some(definition), validation)
            } else {
                (None, Err(vec![storage_error()]))
            }
        }
        Err(errors) => (None, Err(errors)),
    };
    let success = result.is_ok();
    let snapshot = ValidationSnapshot {
        id,
        json,
        revision,
        definition,
        result,
    };
    if library.install_validation(snapshot) {
        if success {
            library.set_status("Definition saved and validated. Review preflight before running.");
        } else if !library.failed {
            library.set_status("Validation found issues. Review the details below.");
        }
    }
}

fn import(controller: &Arc<Controller>) {
    if !controller.available() {
        return;
    }
    {
        let mut library = controller.library.lock().unwrap();
        if library.active_run.is_some() {
            return;
        }
        library.invalidate();
    }
    let platform = controller.platform.lock().unwrap().clone();
    let Some(platform) = platform else {
        controller
            .library
            .lock()
            .unwrap()
            .set_error("Import is unavailable until the desktop supplies a file picker.");
        return;
    };
    let revision = controller.library.lock().unwrap().revision;
    let bytes = match platform.import_definition() {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            controller
                .library
                .lock()
                .unwrap()
                .set_status("Import cancelled.");
            return;
        }
        Err(()) => {
            controller
                .library
                .lock()
                .unwrap()
                .set_error("Import could not be completed.");
            return;
        }
    };
    if !controller.available() {
        return;
    }
    let definition = match macro_model::decode_macro(&bytes) {
        Ok(definition) => definition,
        Err(_) => {
            controller
                .library
                .lock()
                .unwrap()
                .set_error("Import is not a safe typed macro definition.");
            return;
        }
    };
    let mut library = controller.library.lock().unwrap();
    if library.active_run.is_some() || library.revision != revision {
        return;
    }
    library.invalidate();
    if library.records.len() >= MAX_LIBRARY_ITEMS {
        library.set_error("The macro library has reached its item limit.");
        return;
    }
    if library
        .records
        .iter()
        .any(|record| record.definition.id == definition.id)
    {
        library
            .set_error("A macro with this ID already exists. Duplicate it in the library instead.");
        return;
    }
    let Ok(draft) = String::from_utf8(bytes) else {
        library.set_error("The imported definition is not UTF-8 JSON.");
        return;
    };
    let id = definition.id.clone();
    let mut records = library.records.clone();
    records.push(Record { definition, draft });
    if commit_records(controller, &mut library, records) {
        library.selected = Some(id);
        library.selected_run = None;
        library.invalidate();
        library.set_status("Imported a local macro. Validate it before running.");
    }
}

fn export(controller: &Arc<Controller>) {
    if !controller.available() {
        return;
    }
    let text = {
        let library = controller.library.lock().unwrap();
        if library.active_run.is_some() {
            return;
        }
        let Some(record) = library.selected_record() else {
            return;
        };
        match macro_model::decode_macro(record.draft.as_bytes()) {
            Ok(definition) if definition.id == record.definition.id => record.draft.clone(),
            _ => {
                drop(library);
                controller
                    .library
                    .lock()
                    .unwrap()
                    .set_error("Fix the selected draft before exporting it.");
                return;
            }
        }
    };
    let platform = controller.platform.lock().unwrap().clone();
    let outcome = platform.map(|platform| platform.export_definition(&text));
    let mut library = controller.library.lock().unwrap();
    match outcome {
        Some(Ok(true)) => library.set_status("Macro exported."),
        Some(Ok(false)) => library.set_status("Export cancelled."),
        Some(Err(())) => library.set_error("Export could not be completed."),
        None => {
            library.set_error("Export is unavailable until the desktop supplies a file picker.")
        }
    }
}

fn save_evidence(controller: &Arc<Controller>) {
    if !controller.available() {
        return;
    }
    let (id, evidence) = {
        let library = controller.library.lock().unwrap();
        if library.active_run.is_some() {
            return;
        }
        let Some(run) = library.selected_result() else {
            return;
        };
        let evidence = macro_runner::export_evidence(&run.report);
        let Ok(evidence) = serde_json::to_string_pretty(&evidence) else {
            return;
        };
        (run.id.clone(), evidence)
    };
    let platform = controller.platform.lock().unwrap().clone();
    let outcome = platform.map(|platform| platform.save_evidence(&evidence));
    let mut library = controller.library.lock().unwrap();
    match outcome {
        Some(Ok(true)) => {
            if let Some(run) = library.runs.iter_mut().find(|run| run.id == id) {
                run.evidence_saved = true;
            }
            library.set_status("Evidence saved.");
        }
        Some(Ok(false)) => library.set_status("Evidence save cancelled."),
        Some(Err(())) => library.set_error("Evidence could not be saved."),
        None => library
            .set_error("Evidence saving is unavailable until the desktop supplies a file picker."),
    }
}

fn select_result(controller: &Arc<Controller>, index: i32) {
    let mut library = controller.library.lock().unwrap();
    let Some(macro_id) = library.selected.clone() else {
        return;
    };
    let Some(index) = usize::try_from(index).ok() else {
        return;
    };
    library.selected_run = library
        .runs
        .iter()
        .filter(|run| run.macro_id == macro_id)
        .nth(index)
        .map(|run| run.id.clone());
}

fn wire_callbacks(ui: &MainWindow, key: usize, generation: u64, controller: Arc<Controller>) {
    let held = controller.clone();
    ui.global::<MacroLogic>().on_select(move |index| {
        if !host_current(&held, key, generation) || !held.available() {
            return;
        }
        {
            let mut library = held.library.lock().unwrap();
            if library.active_run.is_some() {
                return;
            }
            library.select(index);
        }
        refresh_all(&held);
    });
    let held = controller.clone();
    let weak = ui.as_weak();
    ui.global::<MacroLogic>().on_new_macro(move || {
        if !host_current(&held, key, generation) {
            return;
        }
        if let Some(ui) = weak.upgrade() {
            new_macro(&ui, &held.state);
        }
    });
    let held = controller.clone();
    ui.global::<MacroLogic>().on_duplicate(move || {
        if !host_current(&held, key, generation) {
            return;
        }
        duplicate(&held);
        refresh_all(&held);
    });
    let held = controller.clone();
    ui.global::<MacroLogic>().on_delete_macro(move |id| {
        if !host_current(&held, key, generation) {
            return;
        }
        delete(&held, id.as_str());
        refresh_all(&held);
    });
    let held = controller.clone();
    ui.global::<MacroLogic>().on_edited(move |text| {
        if !host_current(&held, key, generation) || !held.available() {
            return;
        }
        {
            let mut library = held.library.lock().unwrap();
            if library.active_run.is_some() {
                return;
            }
            library.edit(text.to_string());
        }
        refresh_all(&held);
    });
    let held = controller.clone();
    ui.global::<MacroLogic>().on_validate(move || {
        if !host_current(&held, key, generation) {
            return;
        }
        validate(&held);
        refresh_all(&held);
    });
    let held = controller.clone();
    ui.global::<MacroLogic>().on_import_macro(move || {
        if !host_current(&held, key, generation) {
            return;
        }
        import(&held);
        refresh_all(&held);
    });
    let held = controller.clone();
    ui.global::<MacroLogic>().on_export_macro(move || {
        if !host_current(&held, key, generation) {
            return;
        }
        export(&held);
        refresh_all(&held);
    });
    let held = controller.clone();
    ui.global::<MacroLogic>().on_select_result(move |index| {
        if !host_current(&held, key, generation) {
            return;
        }
        select_result(&held, index);
        refresh_all(&held);
    });
    let held = controller.clone();
    ui.global::<MacroLogic>().on_save_evidence(move || {
        if !host_current(&held, key, generation) {
            return;
        }
        save_evidence(&held);
        refresh_all(&held);
    });
    let held = controller.clone();
    ui.global::<MacroLogic>().on_dry_run(move || {
        if host_current(&held, key, generation) {
            start_run(&held, true);
        }
    });
    let held = controller.clone();
    ui.global::<MacroLogic>().on_run(move || {
        if host_current(&held, key, generation) {
            start_run(&held, false);
        }
    });
    let held = controller.clone();
    ui.global::<MacroLogic>().on_cancel(move || {
        if !host_current(&held, key, generation) {
            return;
        }
        {
            let mut library = held.library.lock().unwrap();
            if let Some(run) = library.active_run.as_ref() {
                run.cancel.cancel();
                library.set_status(
                    "Cancellation requested. Waiting for the current operation to finish.",
                );
            }
        }
        refresh_all(&held);
    });
    let held = controller.clone();
    let weak = ui.as_weak();
    ui.global::<MacroLogic>().on_add_step(move |kind| {
        if !host_current(&held, key, generation) {
            return;
        }
        let after = weak.upgrade().and_then(|ui| usize::try_from(ui.global::<MacroData>().get_selected_step()).ok());
        let selected = form_edit(&held, |draft| super::macro_form::add_step(draft, kind.as_str(), after));
        if let (Some(index), Some(ui)) = (selected, weak.upgrade()) {
            ui.global::<MacroData>().set_selected_step(index as i32);
            ui.global::<MacroData>().set_json_mode(false);
        }
        refresh_all(&held);
    });
    let held = controller.clone();
    let weak = ui.as_weak();
    ui.global::<MacroLogic>().on_select_step(move |index| {
        if let Some(ui) = weak.upgrade() {
            ui.global::<MacroData>().set_selected_step(index);
            ui.global::<MacroData>().set_call_contract(-1);
            ui.global::<MacroData>().set_call_function(-1);
            ui.global::<MacroData>().set_call_error("".into());
        }
        refresh_all(&held);
    });
    let held = controller.clone();
    ui.global::<MacroLogic>().on_remove_step(move |index| {
        if host_current(&held, key, generation) {
            form_edit(&held, |draft| super::macro_form::remove_step(draft, index.max(0) as usize).map(|draft| (draft, 0)));
            refresh_all(&held);
        }
    });
    let held = controller.clone();
    let weak = ui.as_weak();
    ui.global::<MacroLogic>().on_move_step(move |index, delta| {
        if !host_current(&held, key, generation) {
            return;
        }
        if let Some(moved) = form_edit(&held, |draft| super::macro_form::move_step(draft, index.max(0) as usize, delta)) {
            if let Some(ui) = weak.upgrade() {
                ui.global::<MacroData>().set_selected_step(moved as i32);
            }
        }
        refresh_all(&held);
    });
    let held = controller.clone();
    ui.global::<MacroLogic>().on_toggle_step(move |index| {
        if host_current(&held, key, generation) {
            form_edit(&held, |draft| super::macro_form::toggle_step(draft, index.max(0) as usize).map(|draft| (draft, 0)));
            refresh_all(&held);
        }
    });
    let held = controller.clone();
    ui.global::<MacroLogic>().on_set_bundle(move |enabled| {
        if host_current(&held, key, generation) {
            form_edit(&held, |draft| super::macro_form::set_bundle(draft, enabled).map(|draft| (draft, 0)));
            refresh_all(&held);
        }
    });
    let held = controller.clone();
    ui.global::<MacroLogic>().on_set_field(move |index, field, value| {
        if host_current(&held, key, generation) && value.len() <= 64 * 1024 {
            form_edit(&held, |draft| super::macro_form::set_field(draft, index.max(0) as usize, field.as_str(), value.as_str()).map(|draft| (draft, 0)));
            refresh_all(&held);
        }
    });
    let held = controller.clone();
    let weak = ui.as_weak();
    ui.global::<MacroLogic>().on_choose_call(move |contract, function| {
        let Some(ui) = weak.upgrade() else { return };
        let data = ui.global::<MacroData>();
        data.set_call_contract(contract);
        data.set_call_function(function);
        data.set_call_error("".into());
        data.set_call_arguments("".into());
        project_call(&ui, &held.state.lock().unwrap());
    });
    let held = controller.clone();
    let weak = ui.as_weak();
    ui.global::<MacroLogic>().on_encode_call(move |index, arguments, send| {
        let Some(ui) = weak.upgrade() else { return };
        if !host_current(&held, key, generation) {
            return;
        }
        let data = ui.global::<MacroData>();
        let function = call_function(&held.state.lock().unwrap(), data.get_call_contract(), data.get_call_function());
        let Some(function) = function else { return };
        let mut failure = None;
        form_edit(&held, |draft| match super::macro_form::set_call(draft, index.max(0) as usize, &function, arguments.as_str(), send) {
            Ok(draft) => Ok((draft, 0)),
            Err(error) => {
                failure = Some(error.clone());
                Err(error)
            }
        });
        data.set_call_error(failure.unwrap_or_default().into());
        refresh_all(&held);
    });
    let held = controller.clone();
    let weak = ui.as_weak();
    ui.global::<Logic>().on_new_macro(move || {
        if !host_current(&held, key, generation) {
            return;
        }
        if let Some(ui) = weak.upgrade() {
            new_macro(&ui, &held.state);
        }
    });
}

fn start_run(controller: &Arc<Controller>, dry_run: bool) {
    let Some(executor) = controller.executor.lock().unwrap().clone() else {
        controller.library.lock().unwrap().set_error(
            "Macro execution is unavailable until the local execution adapter is installed.",
        );
        refresh_all(controller);
        return;
    };
    let (workspace_epoch, network_epoch, owned_node) = {
        let state = controller.state.lock().unwrap();
        let Some(owned_node) = state
            .owned_node
            .as_ref()
            .filter(|node| node.valid())
            .cloned()
        else {
            drop(state);
            controller
                .library
                .lock()
                .unwrap()
                .set_error("An active workspace-owned Anvil node is required.");
            refresh_all(controller);
            return;
        };
        if !state.workspace_ready
            || state.transition_pending()
            || state.workspace_epoch != controller.workspace_epoch
        {
            return;
        }
        (state.workspace_epoch, state.network_epoch, owned_node)
    };
    let (request, cancel, run_id) = {
        let mut library = controller.library.lock().unwrap();
        if library.active_run.is_some() {
            return;
        }
        let Some(snapshot) = library.validation_current().cloned() else {
            library.set_error("Validate the exact selected draft before running.");
            drop(library);
            refresh_all(controller);
            return;
        };
        let Some(definition) = snapshot.definition.filter(|_| snapshot.result.is_ok()) else {
            library.set_error("Resolve validation issues before running.");
            drop(library);
            refresh_all(controller);
            return;
        };
        if !definition.steps.iter().any(|step| step.enabled) {
            library.set_error("Add at least one enabled step before running.");
            drop(library);
            refresh_all(controller);
            return;
        }
        let rpc_token = rand::random::<u64>();
        let token = ExecutionToken {
            workspace_epoch,
            network_epoch,
            rpc_token,
        };
        let cancel = CancellationToken::new();
        let run_id = RunId::new();
        library.active_run = Some(ActiveRun {
            id: run_id.clone(),
            macro_id: snapshot.id,
            json: snapshot.json,
            revision: snapshot.revision,
            workspace_epoch,
            network_epoch,
            token: token.clone(),
            owned_node: owned_node.clone(),
            definition: definition.clone(),
            dry_run,
            cancel: cancel.clone(),
            started: Instant::now(),
        });
        library.selected_run = None;
        library.set_status(if dry_run {
            "Dry run in progress."
        } else {
            "Macro run in progress."
        });
        (
            ExecutionRequest {
                definition,
                dry_run,
                token,
                owned_node,
            },
            cancel,
            run_id,
        )
    };
    refresh_all(controller);
    let held = controller.clone();
    controller.runtime.spawn(async move {
        let task = tokio::spawn(async move { executor.execute(request, cancel).await });
        let result = match task.await {
            Ok(result) => result,
            Err(_) => Err(ExecutionFailure::Failed),
        };
        let _ = slint::invoke_from_event_loop(move || finish_run(&held, &run_id, result));
    });
}

fn terminal_step(status: StepStatus) -> bool {
    !matches!(status, StepStatus::Pending | StepStatus::Running)
}

fn valid_output(active: &ActiveRun, output: &ExecutionOutput) -> bool {
    let enabled: Vec<_> = active
        .definition
        .steps
        .iter()
        .filter(|step| step.enabled)
        .collect();
    if output.report.token != active.token
        || output.report.dry_run != active.dry_run
        || output.report.steps.len() != enabled.len()
        || output.step_ids.len() != enabled.len()
    {
        return false;
    }
    for (index, ((report, mapped), definition)) in output
        .report
        .steps
        .iter()
        .zip(&output.step_ids)
        .zip(enabled)
        .enumerate()
    {
        let (_, expected_kind, _) = step_labels(&definition.kind);
        if report.id.0 != index as u64 + 1
            || mapped != &definition.id
            || runner_kind(report.kind) != expected_kind
            || !terminal_step(report.status)
        {
            return false;
        }
    }
    output.report.steps.iter().all(|step| {
        let summary = &step.output;
        let hash = |value: &str| {
            value.strip_prefix("0x").is_some_and(|digits| {
                digits.len() == 64 && digits.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        };
        summary.digest.as_deref().is_none_or(hash)
            && summary.transaction_hash.as_deref().is_none_or(hash)
            && summary.source.as_ref().is_none_or(|source| {
                macro_model::parse_quantity(&source.chain_id).is_ok()
                    && hash(&source.genesis_hash)
                    && hash(&source.block_hash)
            })
    }) && serde_json::to_vec(&output.report).is_ok_and(|bytes| bytes.len() <= MAX_RUN_BYTES)
}

fn runner_kind(kind: macro_runner::StepKind) -> &'static str {
    match kind {
        macro_runner::StepKind::Deploy => "Deploy artifact",
        macro_runner::StepKind::AbiCall => "ABI call",
        macro_runner::StepKind::RawCall => "Raw call",
        macro_runner::StepKind::RawSend => "Raw send",
        macro_runner::StepKind::Storage => "Storage read",
        macro_runner::StepKind::Snapshot => "Snapshot",
        macro_runner::StepKind::Revert => "Revert",
        macro_runner::StepKind::Assert => "Assert",
        macro_runner::StepKind::Delay => "Delay",
        macro_runner::StepKind::GasSweep => "Gas sweep",
        macro_runner::StepKind::ProbeGasSweep => "Probe gas sweep",
    }
}

fn completion_is_current(
    library: &Library,
    run: &ActiveRun,
    workspace_epoch: u64,
    network_epoch: u64,
    active: bool,
) -> bool {
    active
        && run.workspace_epoch == workspace_epoch
        && run.network_epoch == network_epoch
        && library.revision == run.revision
        && library
            .selected_record()
            .is_some_and(|record| record.definition.id == run.macro_id && record.draft == run.json)
}

fn finish_run(
    controller: &Arc<Controller>,
    run_id: &RunId,
    result: Result<ExecutionOutput, ExecutionFailure>,
) {
    let (workspace_epoch, network_epoch, active, owned_node) = {
        let state = controller.state.lock().unwrap();
        (
            state.workspace_epoch,
            state.network_epoch,
            state.active && state.workspace_ready,
            state.owned_node.clone(),
        )
    };
    {
        let mut library = controller.library.lock().unwrap();
        if library.active_run.as_ref().map(|run| &run.id) != Some(run_id) {
            return;
        }
        let run = library.active_run.take().unwrap();
        let node_current = owned_node
            .as_ref()
            .is_some_and(|node| node.valid() && Arc::ptr_eq(node, &run.owned_node));
        let current = completion_is_current(
            &library,
            &run,
            workspace_epoch,
            network_epoch,
            active && node_current,
        );
        if !current {
            library.set_status(
                "Run finished after the workspace or selection changed. Its result was discarded.",
            );
        } else {
            match result {
                Ok(output) if valid_output(&run, &output) => {
                    library.runs.push_front(StoredRun {
                        id: run.id.clone(),
                        macro_id: run.macro_id,
                        json: run.json,
                        report: output.report,
                        step_ids: output.step_ids,
                        elapsed_ms: run.started.elapsed().as_millis(),
                        evidence_saved: false,
                    });
                    library.runs.truncate(MAX_RUNS);
                    library.selected_run = Some(run.id);
                    library.set_status("Run finished. Review its status and evidence.");
                }
                Ok(_) => library
                    .set_error("The execution adapter returned an invalid or oversized report."),
                Err(ExecutionFailure::Unavailable) => {
                    library.set_error("Local macro execution is unavailable.")
                }
                Err(ExecutionFailure::InvalidPlan) => {
                    library.set_error("The execution adapter rejected the plan.")
                }
                Err(ExecutionFailure::StaleContext) => {
                    library.set_error("The workspace or node changed before execution completed.")
                }
                Err(ExecutionFailure::Failed) => {
                    library.set_error("Macro execution failed before a report was available.")
                }
            }
        }
    }
    refresh_all(controller);
}

fn step_labels(kind: &StepKind) -> (&'static str, &'static str, &'static str) {
    match kind {
        StepKind::DeployArtifact { .. } => (
            "Deploy contract",
            "Deploy artifact",
            "Broadcasts a deployment on the workspace node.",
        ),
        StepKind::AbiCall {
            mode: AbiCallMode::Read,
            ..
        } => ("Read contract", "ABI call", "Reads the workspace node."),
        StepKind::AbiCall {
            mode: AbiCallMode::Send,
            ..
        } => (
            "Send contract call",
            "ABI call",
            "Broadcasts a contract call on the workspace node.",
        ),
        StepKind::RawCall { .. } => (
            "Read with calldata",
            "Raw call",
            "Reads the workspace node.",
        ),
        StepKind::RawSend { .. } => (
            "Send calldata",
            "Raw send",
            "Broadcasts a transaction on the workspace node.",
        ),
        StepKind::StorageRead { .. } => ("Read storage", "Storage read", "Reads one storage slot."),
        StepKind::Snapshot { .. } => (
            "Take snapshot",
            "Snapshot",
            "Creates a private workspace-node snapshot.",
        ),
        StepKind::Revert { .. } => (
            "Revert snapshot",
            "Revert",
            "Reverts an earlier snapshot; probes determine verification.",
        ),
        StepKind::GasSweep { .. } => (
            "Sweep gas",
            "Gas sweep",
            "Performs bounded reads to test gas limits.",
        ),
        StepKind::ProbeGasSweep { .. } => (
            "Calibrate routed gas",
            "Probe gas sweep",
            "Tests a reviewed GasProbe at bounded budgets and pins the final gas limit.",
        ),
        StepKind::Assert { .. } => ("Check result", "Assert", "Checks an earlier public result."),
        StepKind::Delay { .. } => ("Wait", "Delay", "Waits before the next step."),
    }
}

fn step_status(status: StepStatus) -> &'static str {
    match status {
        StepStatus::Pending => "Pending",
        StepStatus::Running => "Running",
        StepStatus::Succeeded => "Succeeded",
        StepStatus::RevertAcknowledgedUnverified => "Revert acknowledged, not verified",
        StepStatus::Failed => "Failed",
        StepStatus::Unknown => "Unknown",
        StepStatus::Cancelled => "Cancelled",
        StepStatus::Skipped => "Skipped",
        StepStatus::RollbackFailed => "Rollback failed",
    }
}

fn run_status(status: macro_runner::RunStatus) -> &'static str {
    match status {
        macro_runner::RunStatus::Completed => "Completed",
        macro_runner::RunStatus::PreflightComplete => "Dry run complete",
        macro_runner::RunStatus::InvalidPlan => "Invalid plan",
        macro_runner::RunStatus::Failed => "Failed",
        macro_runner::RunStatus::Unknown => "Unknown",
        macro_runner::RunStatus::Cancelled => "Cancelled",
        macro_runner::RunStatus::RevertAcknowledgedUnverified => {
            "Revert acknowledged, not verified"
        }
        macro_runner::RunStatus::RollbackFailed => "Rollback failed",
    }
}

fn signer_name(signer: &Signer) -> &'static str {
    match signer {
        Signer::AnvilAccount0 => "Anvil account 0",
        Signer::AnvilAccount1 => "Anvil account 1",
        Signer::AnvilAccount2 => "Anvil account 2",
        Signer::UnlockedWalletRef => "Unlocked wallet reference",
    }
}

fn short_errors(errors: &[ValidationError]) -> String {
    let mut lines = Vec::new();
    for error in errors.iter().take(MAX_UI_ERRORS) {
        lines.push(format!("{}: {}", error.path, error.message));
    }
    if errors.len() > MAX_UI_ERRORS {
        lines.push("Additional issues were omitted.".into());
    }
    lines.join("\n")
}

/// Exported once per paint: building it per step made a 128-step result
/// quadratic and froze the window for seconds.
fn runner_evidence(run: &StoredRun) -> serde_json::Value {
    macro_runner::export_evidence(&run.report)
}

fn runner_step_evidence(evidence: &serde_json::Value, index: usize) -> String {
    evidence
        .get("evidence")
        .and_then(|value| value.get("steps"))
        .and_then(|value| value.get(index))
        .and_then(|value| serde_json::to_string(value).ok())
        .filter(|text| text.len() <= MAX_STEP_OUTPUT_BYTES)
        .unwrap_or_else(|| "Public step summary unavailable.".into())
}

fn project(
    ui: &MainWindow,
    library: &Library,
    workspace_current: bool,
    node_available: bool,
    backend: bool,
    platform: bool,
) {
    let data = ui.global::<MacroData>();
    let selected = library.selected_index();
    let record = library.selected_record();
    let validation = library.validation_current();
    let validated = validation.and_then(|value| value.result.as_ref().ok());
    let definition = validation.and_then(|value| value.definition.as_ref());
    let selected_result = library.selected_result();
    let busy = library.active_run.is_some();
    let can_run = workspace_current
        && node_available
        && backend
        && !busy
        && validated.is_some()
        && definition.is_some_and(|definition| definition.steps.iter().any(|step| step.enabled));

    data.set_macros(super::model(
        library
            .records
            .iter()
            .map(|record| MacroRow {
                id: record.definition.id.as_str().into(),
                name: record.definition.name.clone().into(),
                detail: format!("{} step(s)", record.definition.steps.len()).into(),
            })
            .collect(),
    ));
    data.set_selected(selected.map(|index| index as i32).unwrap_or(-1));
    data.set_selected_id(
        record
            .map(|record| record.definition.id.as_str())
            .unwrap_or("")
            .into(),
    );
    data.set_selected_name(
        record
            .map(|record| record.definition.name.as_str())
            .unwrap_or("")
            .into(),
    );
    data.set_bundle(record.and_then(|record| serde_json::from_str::<serde_json::Value>(&record.draft).ok()).is_some_and(|value| value["bundle"] == true));
    data.set_summary(
        record
            .map(|record| {
                if record.definition.description.is_empty() {
                    format!(
                        "{} step(s). Definitions are stored locally.",
                        record.definition.steps.len()
                    )
                } else {
                    record.definition.description.clone()
                }
            })
            .unwrap_or_default()
            .into(),
    );
    let draft = record.map(|record| record.draft.as_str()).unwrap_or("");
    if data.get_definition_json().as_str() != draft {
        data.set_definition_json(draft.into());
    }
    data.set_validation_ready(validation.is_some());
    data.set_validated_id(
        validation
            .map(|value| value.id.as_str())
            .unwrap_or("")
            .into(),
    );
    data.set_validated_json(
        validation
            .map(|value| value.json.as_str())
            .unwrap_or("")
            .into(),
    );
    data.set_validation_failed(validation.is_some_and(|value| value.result.is_err()));
    data.set_validation(
        validation
            .map(|value| match &value.result {
                Ok(summary) => format!(
                    "Valid: {} enabled and {} disabled step(s).",
                    summary.enabled_steps, summary.disabled_steps
                ),
                Err(errors) => short_errors(errors),
            })
            .unwrap_or_default()
            .into(),
    );
    data.set_preflight_status(validated.map(|summary| {
        if backend {
            format!("Validated for local-only execution. Review {} step(s); the adapter must authorize the current owned node.", summary.enabled_steps)
        } else {
            "Validated locally. Execution adapter unavailable; no run can start.".into()
        }
    }).unwrap_or_default().into());
    data.set_preflight_broadcast(
        validated
            .map(|summary| format!("{} broadcast step(s)", summary.broadcast_count))
            .unwrap_or_default()
            .into(),
    );
    data.set_preflight_read(
        validated
            .map(|summary| {
                format!(
                    "{} read step(s), at most {} read request(s)",
                    summary.read_count, summary.max_read_requests
                )
            })
            .unwrap_or_default()
            .into(),
    );
    data.set_preflight_sweep(
        validated
            .map(|summary| {
                format!(
                    "{} total attempt(s), at most {} in one sweep",
                    summary.total_sweep_attempts, summary.max_sweep_attempts
                )
            })
            .unwrap_or_default()
            .into(),
    );
    data.set_preflight_signers(
        validated
            .map(|summary| {
                if summary.signers.is_empty() {
                    "None".into()
                } else {
                    summary
                        .signers
                        .iter()
                        .map(signer_name)
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            })
            .unwrap_or_default()
            .into(),
    );
    data.set_preflight_snapshot(
        validated
            .map(|summary| {
                format!(
                    "Before run: {}; explicit snapshots: {}; reverts: {}; rollback probes: {}. {}",
                    if summary.snapshot_before_run {
                        "yes"
                    } else {
                        "no"
                    },
                    summary.snapshot_count,
                    summary.revert_count,
                    summary.rollback_probe_count,
                    summary.revert_policy
                )
            })
            .unwrap_or_default()
            .into(),
    );
    data.set_status(library.status.clone().into());
    data.set_loading(false);
    data.set_failed(library.failed);
    data.set_error(library.error.clone().into());
    data.set_run_busy(busy);
    data.set_can_run(can_run);
    data.set_can_dry_run(can_run);

    let current_result =
        selected_result.filter(|run| record.is_some_and(|record| record.draft == run.json));
    let evidence = current_result.map(runner_evidence);
    let draft_definition = record.and_then(|record| macro_model::decode_macro(record.draft.as_bytes()).ok());
    let steps = draft_definition.as_ref().or(definition)
        .map(|definition| {
            definition
                .steps
                .iter()
                .map(|step| {
                    let (name, kind, action) = step_labels(&step.kind);
                    let matching = current_result.and_then(|run| {
                        run.step_ids
                            .iter()
                            .position(|id| id == &step.id)
                            .map(|index| (run, index))
                    });
                    let status = if !step.enabled {
                        "Disabled"
                    } else if busy {
                        "Waiting for report"
                    } else {
                        matching
                            .map(|(run, index)| step_status(run.report.steps[index].status))
                            .unwrap_or("Not run")
                    };
                    MacroStep {
                        name: name.into(),
                        kind: kind.into(),
                        enabled: step.enabled,
                        status: status.into(),
                        duration: "Not reported".into(),
                        public_result: matching
                            .and_then(|(_, index)| evidence.as_ref().map(|evidence| runner_step_evidence(evidence, index)))
                            .unwrap_or_default()
                            .into(),
                        action_hint: action.into(),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    data.set_steps(super::model(steps));
    let draft = record.map(|record| record.draft.as_str()).unwrap_or("");
    let draft_value: serde_json::Value = serde_json::from_str(draft).unwrap_or_default();
    let draft_steps = draft_value["steps"].as_array().cloned().unwrap_or_default();
    data.set_step_summaries(super::model(draft_steps.iter().map(|step| super::macro_form::summary(step).into()).collect()));
    let mut selected_step = data.get_selected_step();
    if selected_step >= draft_steps.len() as i32 {
        selected_step = draft_steps.len() as i32 - 1;
        data.set_selected_step(selected_step);
    }
    let index = usize::try_from(selected_step).ok();
    let form_fields = super::model(
        index
            .map(|index| super::macro_form::fields(draft, index))
            .unwrap_or_default()
            .into_iter()
            .map(|field| super::MacroFormField { key: field.key.into(), label: field.label.into(), value: field.value.into(), hint: field.hint.into(), kind: field.kind.into() })
            .collect(),
    );
    // Typing in a step field must not rebuild its LineEdit. The model is kept
    // only for the same step of the same macro.
    let shown_step = (data.get_selected_id().to_string(), selected_step);
    let same_step = FORM_STEP.with(|last| last.replace(shown_step.clone()) == shown_step);
    data.set_form_fields(if same_step {
        super::keep_rows(data.get_form_fields(), form_fields, |old, new| old.key == new.key && old.kind == new.kind)
    } else {
        form_fields
    });
    data.set_references(super::model(index.map(|index| super::macro_form::references(draft, index)).unwrap_or_default().into_iter().map(Into::into).collect()));
    data.set_step_kind(
        index
            .and_then(|index| draft_definition.as_ref().and_then(|definition| definition.steps.get(index)))
            .map(|step| step_labels(&step.kind).0)
            .or_else(|| index.and_then(|index| draft_steps.get(index)).map(|_| "Step (fix its fields)"))
            .unwrap_or("")
            .into(),
    );
    data.set_step_result(
        index
            .and_then(|index| {
                let id = draft_steps.get(index)?["id"].as_str()?.to_string();
                let run = current_result?;
                let position = run.step_ids.iter().position(|step| step.as_str() == id)?;
                let evidence = runner_step_evidence(evidence.as_ref()?, position);
                let pretty = serde_json::from_str::<serde_json::Value>(&evidence).ok().and_then(|value| serde_json::to_string_pretty(&value).ok()).unwrap_or(evidence);
                Some(format!("Status  {}\n\n{pretty}", step_status(run.report.steps[position].status)))
            })
            .unwrap_or_default()
            .into(),
    );
    let selected_id = library.selected.as_ref();
    let runs: Vec<_> = library
        .runs
        .iter()
        .filter(|run| Some(&run.macro_id) == selected_id)
        .collect();
    data.set_results(super::model(
        runs.iter()
            .map(|run| MacroResult {
                id: run.id.as_str().into(),
                title: if run.report.dry_run {
                    "Dry run".into()
                } else {
                    "Run".into()
                },
                status: run_status(run.report.status).into(),
                duration: format!("{} ms", run.elapsed_ms).into(),
            })
            .collect(),
    ));
    data.set_selected_result(
        selected_result
            .and_then(|selected| runs.iter().position(|run| run.id == selected.id))
            .map(|index| index as i32)
            .unwrap_or(-1),
    );
    let raw = selected_result
        .and_then(|run| {
            serde_json::to_string_pretty(&macro_runner::export_evidence(&run.report)).ok()
        })
        .unwrap_or_default();
    data.set_result_raw(raw.into());
    data.set_result_simple(selected_result.map(|run| format!("{}\n{}\n{} step(s) reported.\nDuration: {} ms.\nEvidence contains public summaries and digests, not raw inputs or outputs.", run_status(run.report.status), macro_runner::chain_effect(&run.report), run.report.steps.len(), run.elapsed_ms)).unwrap_or_default().into());
    let log = selected_result
        .map(|run| {
            run.report
                .steps
                .iter()
                .enumerate()
                .map(|(index, step)| {
                    format!(
                        "{}. {}: {}",
                        index + 1,
                        runner_kind(step.kind),
                        step_status(step.status)
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    data.set_run_log(
        if log.len() <= MAX_WINDOW_LOG_BYTES {
            log
        } else {
            "Run log exceeds the display limit.".into()
        }
        .into(),
    );
    data.set_evidence_status(
        selected_result
            .map(|run| {
                if run.evidence_saved {
                    "Saved"
                } else {
                    "Available in this session"
                }
            })
            .unwrap_or("")
            .into(),
    );
    data.set_can_save_evidence(platform && selected_result.is_some() && !busy);
}

#[cfg(test)]
#[path = "native_macro_tests.rs"]
mod tests;

/// Apply a form edit to the selected draft. The edit returns the new draft
/// and an index (the new or moved step); errors show in the form.
fn form_edit(controller: &Arc<Controller>, edit: impl FnOnce(&str) -> Result<(String, usize), String>) -> Option<usize> {
    if !controller.available() {
        return None;
    }
    let mut library = controller.library.lock().unwrap();
    if library.active_run.is_some() {
        return None;
    }
    let draft = library.selected_record()?.draft.clone();
    match edit(&draft) {
        Ok((next, index)) => {
            library.edit(next);
            library.set_status("Step changed. Validate before running.");
            Some(index)
        }
        Err(error) => {
            library.set_error(&error);
            None
        }
    }
}

fn workspace_contracts(state: &super::session::DesktopState) -> Vec<&super::session::ContractItem> {
    state.contracts.iter().filter(|contract| contract.kind == "src" && !contract.functions.is_empty()).collect()
}

fn call_function(state: &super::session::DesktopState, contract: i32, function: i32) -> Option<ethers::abi::Function> {
    let contract = *workspace_contracts(state).get(usize::try_from(contract).ok()?)?;
    let info = contract.functions.get(usize::try_from(function).ok()?)?;
    let signature = format!("{}({})", info.name, info.params.iter().map(|(_, kind)| kind.as_str()).collect::<Vec<_>>().join(","));
    use ethers::abi::FunctionExt;
    contract.abi.functions().find(|item| item.abi_signature() == signature).cloned()
}

/// Add a history entry as a raw_send / raw_call step of the selected macro,
/// creating a macro when none is selected. The target becomes the step
/// that deployed it when the macro has one; otherwise the literal address.
pub fn add_history_step(ui: &MainWindow, state: &State, entry: &super::history::Entry) -> Result<String, String> {
    if entry.data.is_empty() {
        return Err("This entry has no calldata to replay.".into());
    }
    let controller = controller_for(state).ok_or("Macros are unavailable for this window.")?;
    if controller.library.lock().unwrap().selected_record().is_none() && !begin_new(&controller) {
        return Err("Could not create a macro for this step.".into());
    }
    let kind = if entry.kind == "send" { "raw_send" } else { "raw_call" };
    let added = form_edit(&controller, |draft| {
        let (draft, index) = super::macro_form::add_step(draft, kind, None)?;
        let mut draft = draft;
        for (key, value) in [("target", entry.address.as_str()), ("data", entry.data.as_str()), ("value", if entry.value.is_empty() { "0" } else { entry.value.as_str() })] {
            draft = super::macro_form::set_field(&draft, index, key, value)?;
        }
        let signer = (0..3)
            .find(|index| super::session::anvil_dev_account(*index).is_some_and(|(address, _)| address.eq_ignore_ascii_case(&entry.from)))
            .map(|index| format!("anvil_account{index}"))
            .unwrap_or_else(|| "unlocked_wallet_ref".into());
        draft = super::macro_form::set_field(&draft, index, "signer", &signer)?;
        Ok((draft, index))
    })
    .ok_or_else(|| controller.library.lock().unwrap().error.clone())?;
    ui.global::<MacroData>().set_selected_step(added as i32);
    ui.global::<MacroData>().set_json_mode(false);
    refresh_all(&controller);
    Ok(format!("Added {}.{} as step {} of the selected macro. Open Macros to edit or run it.", entry.contract, entry.function, added + 1))
}

/// Contract and function pickers for the Encode call section.
pub fn project_call(ui: &MainWindow, state: &super::session::DesktopState) {
    let data = ui.global::<MacroData>();
    let contracts = workspace_contracts(state);
    data.set_contracts(super::model(contracts.iter().map(|contract| super::MacroChoice { label: contract.name.clone().into(), detail: format!("{} functions", contract.functions.len()).into() }).collect()));
    let chosen = usize::try_from(data.get_call_contract()).ok().and_then(|index| contracts.get(index));
    data.set_functions(super::model(chosen.map(|contract| contract.functions.iter().map(|function| super::MacroChoice { label: format!("{}({})", function.name, function.params.iter().map(|(_, kind)| kind.as_str()).collect::<Vec<_>>().join(",")).into(), detail: function.mutability.clone().into() }).collect()).unwrap_or_default()));
    let params = chosen
        .and_then(|contract| contract.functions.get(usize::try_from(data.get_call_function()).ok()?))
        .map(|function| function.params.iter().map(|(name, kind)| super::MacroChoice { label: if name.is_empty() { "arg".into() } else { name.clone().into() }, detail: kind.clone().into() }).collect())
        .unwrap_or_default();
    data.set_call_params(super::model(params));
}

/// Transaction hashes of the selected macro run, in step order, for a
/// scenario trace.
pub fn selected_run_hashes(state: &State) -> Vec<String> {
    let Some(controller) = controller_for(state) else { return Vec::new() };
    let library = controller.library.lock().unwrap();
    library
        .selected_result()
        .map(|run| run.report.steps.iter().filter_map(|step| step.output.transaction_hash.clone()).collect())
        .unwrap_or_default()
}
