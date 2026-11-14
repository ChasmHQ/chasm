use super::deployment::{validate_draft, ConstructorSpec, DeploymentDraft};
use super::jobs::{self, JobEntry, JobId, JobTracker, Operation, RequestToken};
use crate::compiler::Compiler;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub const ZOOM_LEVELS: [f32; 4] = [1.0, 1.1, 1.25, 1.5];
static NEXT_WORKSPACE_EPOCH: AtomicU64 = AtomicU64::new(1);

const ANVIL_DEV_ACCOUNTS: [(&str, &str); 3] = [
    (
        "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266",
        "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
    ),
    (
        "0x70997970C51812dc3A010C7d01b50e0d17dc79C8",
        "0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d",
    ),
    (
        "0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC",
        "0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a",
    ),
];

pub fn anvil_dev_account(index: i32) -> Option<(&'static str, &'static str)> {
    usize::try_from(index)
        .ok()
        .and_then(|index| ANVIL_DEV_ACCOUNTS.get(index).copied())
}

#[derive(Clone, Debug)]
pub struct FunctionInfo {
    pub name: String,
    pub inputs: String,
    pub mutability: String,
    pub params: Vec<(String, String)>,
}

#[derive(Clone, Debug)]
pub struct ContractItem {
    pub abi: ethers::abi::Abi,
    pub name: String,
    pub kind: String,
    pub functions: Vec<FunctionInfo>,
    pub bytecode: String,
    pub constructor_sig: String,
    pub constructor: Option<ConstructorSpec>,
    pub constructor_error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Document {
    pub id: String,
    pub contract_view: super::contract_state::ContractView,
    pub title: String,
    pub body: String,
    pub kind: i32,
    contract_name: Option<String>,
    deployment_origin_document: Option<String>,
}

impl Document {
    pub fn new(title: String, body: String, kind: i32) -> Self {
        let contract_name = match kind {
            0 => Some(title.clone()),
            1 => title.strip_prefix("Deploy · ").map(str::to_string),
            _ => None,
        };
        Self {
            id: super::contract_state::new_document_id(),
            contract_view: super::contract_state::ContractView::new(None),
            title,
            body,
            kind,
            contract_name,
            deployment_origin_document: None,
        }
    }

    pub fn contract_name(&self) -> Option<&str> {
        self.contract_name.as_deref()
    }
}

#[derive(Clone, Debug)]
pub struct MacroNote {
    pub name: String,
    pub body: String,
}

#[derive(Clone, Debug)]
pub struct Deployment {
    pub name: String,
    pub address: String,
}

fn deployment_key(deployment: &Deployment) -> (String, String) {
    (deployment.name.clone(), deployment.address.to_lowercase())
}

fn unique_contract<'a>(contracts: &'a [ContractItem], name: &str) -> Option<&'a ContractItem> {
    let mut matches = contracts.iter().filter(|item| item.name == name);
    let contract = matches.next()?;
    matches.next().is_none().then_some(contract)
}

fn matching_function_index(old: &FunctionInfo, functions: &[FunctionInfo]) -> Option<usize> {
    let mut matches = functions.iter().enumerate().filter(|(_, candidate)| {
        candidate.name == old.name
            && candidate
                .params
                .iter()
                .map(|(_, kind)| kind)
                .eq(old.params.iter().map(|(_, kind)| kind))
    });
    let (index, _) = matches.next()?;
    matches.next().is_none().then_some(index)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainRow {
    pub title: String,
    pub detail: String,
    pub query: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChainQuery {
    Address(String),
    Hash(String),
    BlockNumber(String),
    Invalid,
}

pub struct DesktopState {
    pub active: bool,
    pub startup_screen: bool,
    pub project_opening: bool,
    pub workspace_ready: bool,
    pub workspace_busy: bool,
    pub folder_picker_busy: bool,
    pub security_confirmation: bool,
    pub pending_workspace: Option<PathBuf>,
    pub next_workspace: Option<PathBuf>,
    pub wallet_busy: bool,
    pub wallet_generation: u64,
    pub wallet_source: Option<String>,
    pub pending_keystore_delete: Option<super::keystore::PendingDelete>,
    pub keystore_error: String,
    pub send_busy: bool,
    pub rpc_busy: bool,
    pub mutation_gate: super::mutation_gate::MutationGate,
    pub activity: i32,
    pub submenu: i32,
    pub sidebar_width: f32,
    pub sidebar_collapsed: bool,
    pub sidebar_drag: Option<(f32, f32)>,
    pub log_filter: String,
    pub zoom: f32,
    pub zoom_index: i32,
    pub contracts: Vec<ContractItem>,
    pub documents: Vec<Document>,
    pub active_document: i32,
    last_contracts_document: Option<String>,
    pub selected_contract: Option<String>,
    pub deployment_drafts: HashMap<String, DeploymentDraft>,
    pub selected_function: i32,
    pub deployments: Vec<Deployment>,
    deployment_epochs: HashMap<(String, String), u64>,
    deployment_default_epoch: u64,
    pub macros: Vec<MacroNote>,
    pub logs: Vec<String>,
    pub jobs: Vec<JobEntry>,
    pub job_tracker: JobTracker,
    pub workspace_epoch: u64,
    pub network_epoch: u64,
    pub view_revision: u64,
    pub selected_job: Option<JobId>,
    pub notice: String,
    pub status_left: String,
    pub compile_label: String,
    pub debugger_label: String,
    pub debugger_body: String,
    pub debugger_live: bool,
    pub debug: super::debug_session::DebugSession,
    pub debug_drafts: HashMap<String, String>,
    pub debug_launch_mode: i32,
    pub debug_foundry_target: String,
    pub debug_foundry_match: String,
    pub debug_foundry_trusted: bool,
    pub debug_capture_id: String,
    pub debug_build_info_id: String,
    pub debug_launch_summary: String,
    pub debug_prepared_launch: Option<super::debug_transport::PreparedLaunch>,
    pub console: super::console::ConsoleState,
    pub layout: super::layout::Layout,
    pub explorer_input: String,
    pub explorer_result: String,
    pub recent_blocks: Vec<ChainRow>,
    pub recent_transactions: Vec<ChainRow>,
    pub converter_output: String,
    pub converter_input: String,
    pub converter_tool: i32,
    pub converter_mode: i32,
    pub converter_error: String,
    pub block_number: u64,
    pub keystores: Vec<String>,
    pub selected_keystore: String,
    pub wallet_key: Option<String>,
    pub wallet_address: String,
    pub address_input: String,
    pub deploy_output: String,
    pub deploy_busy: bool,
    pub call_output: String,
    pub storage_output: String,
    pub trace_output: String,
    pub last_tx: String,
    pub snapshot_id: String,
    pub menu_open: i32,
    pub deployment_menu: i32,
    pub dock_tab: i32,
    pub show_logs: bool,
    pub rpc: String,
    pub owned_node: Option<std::sync::Arc<super::owned_node::OwnedNode>>,
    pub(crate) csrf: String,
    pub(crate) client: reqwest::Client,
    pub(crate) session_id: Option<String>,
    pub(crate) generation: u64,
    pub(crate) revision: u64,
    pub(crate) workspace: PathBuf,
    pub(crate) keystore_root: Option<PathBuf>,
}

impl DesktopState {
    pub fn welcome() -> Self {
        let mut state = Self::new(PathBuf::new(), 1.0);
        state.startup_screen = true;
        state.workspace_ready = false;
        state.status_left = "Open a project folder to begin.".into();
        state.compile_label.clear();
        state.debugger_label = "Debugger not started".into();
        state.logs.clear();
        state
    }

    pub fn opening(workspace: PathBuf) -> Self {
        let mut state = Self::welcome();
        state.workspace = workspace;
        state.startup_screen = false;
        state.project_opening = true;
        state.status_left = "Opening project…".into();
        state
    }

    pub fn accept_project_folder(&mut self, path: PathBuf) -> bool {
        if !self.can_change_workspace() {
            return false;
        }
        if self.workspace_ready {
            if path != self.workspace {
                self.pending_workspace = Some(path);
            }
            false
        } else {
            self.next_workspace = Some(path);
            self.project_opening = true;
            self.status_left = "Opening project…".into();
            true
        }
    }

    pub fn new(workspace: PathBuf, zoom: f32) -> Self {
        Self {
            active: true,
            startup_screen: false,
            project_opening: false,
            workspace_ready: true,
            workspace_busy: false,
            folder_picker_busy: false,
            security_confirmation: false,
            pending_workspace: None,
            next_workspace: None,
            wallet_busy: false,
            wallet_generation: 0,
            wallet_source: None,
            pending_keystore_delete: None,
            keystore_error: String::new(),
            send_busy: false,
            rpc_busy: false,
            mutation_gate: super::mutation_gate::MutationGate::default(),
            activity: 0,
            submenu: 0,
            sidebar_width: 250.0,
            sidebar_collapsed: false,
            sidebar_drag: None,
            log_filter: String::new(),
            zoom,
            zoom_index: zoom_index_for(zoom),
            contracts: Vec::new(),
            documents: Vec::new(),
            active_document: -1,
            last_contracts_document: None,
            selected_contract: None,
            deployment_drafts: HashMap::new(),
            selected_function: -1,
            deployments: Vec::new(),
            deployment_epochs: HashMap::new(),
            deployment_default_epoch: 1,
            macros: Vec::new(),
            logs: vec!["Desktop session ready. Activity changes keep this workspace.".into()],
            jobs: Vec::new(),
            job_tracker: JobTracker::default(),
            workspace_epoch: NEXT_WORKSPACE_EPOCH.fetch_add(1, Ordering::Relaxed),
            network_epoch: 1,
            view_revision: 0,
            selected_job: None,
            notice: String::new(),
            status_left: "Ready".into(),
            compile_label: "Compile".into(),
            debugger_label: "Debugger idle".into(),
            debugger_body: "No debugger session. Choose Synthetic, Trusted Foundry, or Offline Capture, then review the launch source before Start.".into(),
            debugger_live: false,
            debug: super::debug_session::DebugSession::default(),
            debug_drafts: HashMap::new(),
            debug_launch_mode: 0,
            debug_foundry_target: String::new(),
            debug_foundry_match: String::new(),
            debug_foundry_trusted: false,
            debug_capture_id: String::new(),
            debug_build_info_id: String::new(),
            debug_launch_summary: "Synthetic fixture selected. No project or chain transaction is implied.".into(),
            debug_prepared_launch: None,
            console: super::console::ConsoleState::default(),
            layout: super::layout::Layout::preset(super::layout::Preset::Contracts),
            explorer_input: String::new(),
            explorer_result: "No lookup yet. Search an address, transaction hash, or block, or open one from the lists.".into(),
            recent_blocks: Vec::new(),
            recent_transactions: Vec::new(),
            converter_output: "Enter a value. Choose a tool in the sidebar.".into(),
            converter_input: String::new(),
            converter_tool: 0,
            converter_mode: 0,
            converter_error: String::new(),
            block_number: 0,
            keystores: Vec::new(),
            selected_keystore: String::new(),
            wallet_key: None,
            wallet_address: String::new(),
            address_input: String::new(),
            deploy_output: String::new(),
            deploy_busy: false,
            call_output: String::new(),
            storage_output: String::new(),
            trace_output: "No trace yet. Deploy or send a call, then open Trace.".into(),
            last_tx: String::new(),
            snapshot_id: String::new(),
            menu_open: -1,
            deployment_menu: -1,
            dock_tab: 0,
            show_logs: true,
            rpc: "http://127.0.0.1:8545".into(),
            owned_node: None,
            csrf: String::new(),
            client: reqwest::Client::builder()
                .cookie_store(true)
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .build()
                .expect("debugger client"),
            session_id: None,
            generation: 0,
            revision: 0,
            workspace,
            keystore_root: super::keystore::default_root().ok(),
        }
    }

    pub fn heading(&self) -> String {
        match self.activity {
            0 => match &self.selected_contract {
                Some(name) => name.clone(),
                None => "Contracts".into(),
            },
            1 => "Explorer".into(),
            2 => format!(
                "Converter · {}",
                super::converter::title(self.converter_tool)
            ),
            3 => match self.current_doc_kind() {
                7 => "Trusted Foundry Build".into(),
                _ => "Macros".into(),
            },
            _ => "Debugger".into(),
        }
    }

    pub fn replace_compiled_contracts(&mut self, contracts: Vec<ContractItem>) {
        let mut cleared = 0;
        for doc in self.documents.iter_mut().filter(|doc| doc.kind == 0) {
            let old_index = doc.contract_view.selected_function;
            if old_index == -1 {
                continue;
            }
            let replacement = doc.contract_name().and_then(|name| {
                let old = unique_contract(&self.contracts, name)?;
                let selected = old.functions.get(old_index as usize)?;
                let new = unique_contract(&contracts, name)?;
                matching_function_index(selected, &new.functions)
            });
            let new_index = replacement.map(|index| index as i32).unwrap_or(-1);
            if new_index == -1 {
                cleared += 1;
            }
            if new_index != old_index {
                doc.contract_view.selected_function = new_index;
                doc.contract_view.revision = doc.contract_view.revision.wrapping_add(1);
            }
        }
        self.contracts = contracts;
        self.view_revision = self.view_revision.wrapping_add(1);
        if self.current_contract_view().is_some() {
            self.restore_contract_view();
        } else {
            self.selected_function = -1;
        }
        if cleared > 0 {
            self.notice = format!("Cleared {cleared} function selection(s) that are unavailable or ambiguous in the new ABI.");
        }
    }

    pub fn visible_contracts(&self) -> Vec<ContractItem> {
        self.contracts
            .iter()
            .filter(|item| item.kind == "src")
            .cloned()
            .collect()
    }

    pub fn current_doc_kind(&self) -> i32 {
        self.documents
            .get(self.active_document as usize)
            .map(|doc| doc.kind)
            .unwrap_or(0)
    }

    pub fn open_activity(&mut self, activity: i32) {
        let activity = activity.clamp(0, 4);
        match activity {
            1 => self.open_tool_document(4, "Chain Explorer"),
            2 => self.open_tool_document(5, "Converter"),
            3 => self.open_tool_document(6, "Macros"),
            0 if self
                .documents
                .get(self.active_document as usize)
                .is_none_or(|doc| (4..=7).contains(&doc.kind)) =>
            {
                let index = self
                    .last_contracts_document
                    .as_deref()
                    .and_then(|id| {
                        self.documents
                            .iter()
                            .position(|doc| doc.id == id && (0..=3).contains(&doc.kind))
                    })
                    .or_else(|| {
                        self.documents
                            .iter()
                            .rposition(|doc| (0..=3).contains(&doc.kind))
                    });
                if let Some(index) = index {
                    self.select_document(index as i32);
                } else {
                    self.active_document = -1;
                    self.activity = 0;
                    self.selected_function = -1;
                    self.view_revision = self.view_revision.wrapping_add(1);
                }
            }
            _ => self.activity = activity,
        }
        self.menu_open = -1;
        self.status_left = format!("Opened {}", self.heading());
        self.push_log(format!("Opened {}", self.heading()));
    }

    pub fn open_foundry_build(&mut self) {
        self.open_tool_document(7, "Trusted Foundry Build");
        self.status_left = "Opened Trusted Foundry Build".into();
    }

    fn open_tool_document(&mut self, kind: i32, title: &str) {
        if let Some(doc) = self.documents.get(self.active_document as usize) {
            if (0..=3).contains(&doc.kind) {
                self.last_contracts_document = Some(doc.id.clone());
            }
        }
        let index = match self.documents.iter().position(|doc| doc.kind == kind) {
            Some(index) => index,
            None => {
                self.documents
                    .push(Document::new(title.into(), String::new(), kind));
                self.documents.len() - 1
            }
        };
        self.select_document(index as i32);
    }

    pub fn set_converter_tool(&mut self, index: i32) {
        self.converter_tool = index.clamp(0, 3);
        self.open_tool_document(5, "Converter");
        self.converter_mode = 0;
        self.recalculate_converter();
    }

    pub fn select_contract(&mut self, visible_index: i32) {
        let Some(item) = self
            .visible_contracts()
            .get(visible_index as usize)
            .cloned()
        else {
            return;
        };
        if unique_contract(&self.contracts, &item.name).is_none() {
            self.notice = format!("Contract {} is ambiguous in the compile output.", item.name);
            return;
        }
        if let Some(index) = self.documents.iter().position(|doc| {
            doc.kind == 0
                && doc.contract_name() == Some(item.name.as_str())
                && doc.contract_view.target.is_none()
        }) {
            self.select_document(index as i32);
            return;
        }
        self.open_contract_doc(item.name, item.functions);
    }

    fn deployment_epoch(&self, deployment: &Deployment) -> u64 {
        self.deployment_epochs
            .get(&deployment_key(deployment))
            .copied()
            .unwrap_or(self.deployment_default_epoch)
    }

    pub fn deployment_id(&self, index: usize) -> Option<String> {
        let deployment = self.deployments.get(index)?;
        Some(super::application::entity_id(
            self,
            "deployment",
            &format!(
                "{}:{}:{}",
                self.deployment_epoch(deployment),
                deployment.name,
                deployment.address.to_lowercase()
            ),
        ))
    }

    pub fn select_deployment(&mut self, index: i32) {
        let Ok(index) = usize::try_from(index) else {
            return;
        };
        let Some(id) = self.deployment_id(index) else {
            return;
        };
        self.select_deployment_by_id(&id);
    }

    pub fn select_deployment_by_id(&mut self, id: &str) -> bool {
        let matches: Vec<_> = (0..self.deployments.len())
            .filter(|&index| self.deployment_id(index).as_deref() == Some(id))
            .collect();
        let [index] = matches.as_slice() else {
            self.notice = "Deployment is missing or ambiguous. Refresh the deployment list.".into();
            return false;
        };
        let deployment = self.deployments[*index].clone();
        let epoch = self.deployment_epoch(&deployment);
        if epoch != self.network_epoch {
            self.notice =
                "This deployment belongs to an earlier network context; it was not rebound.".into();
            return false;
        }
        if self
            .contracts
            .iter()
            .filter(|item| item.name == deployment.name)
            .count()
            > 1
        {
            self.notice = format!(
                "Deployment {} has an ambiguous contract ABI.",
                deployment.name
            );
            return false;
        }
        if let Some(index) = self.documents.iter().position(|doc| {
            doc.kind == 0
                && doc.contract_view.target.as_ref().is_some_and(|target| {
                    target.deployment_id.as_deref() == Some(id) && target.network_epoch == epoch
                })
        }) {
            self.select_document(index as i32);
            return true;
        }
        let title = self.next_contract_title(&deployment.name);
        let mut doc = Document::new(title, String::new(), 0);
        doc.contract_name = Some(deployment.name.clone());
        doc.contract_view.target = Some(super::contract_state::Target {
            contract: deployment.name,
            address: deployment.address,
            deployment_id: Some(id.to_string()),
            network_epoch: epoch,
        });
        self.documents.push(doc);
        self.select_document(self.documents.len() as i32 - 1);
        true
    }

    fn next_contract_title(&self, name: &str) -> String {
        if !self
            .documents
            .iter()
            .any(|doc| doc.kind == 0 && doc.title == name)
        {
            return name.to_string();
        }
        for suffix in 2.. {
            let title = format!("{name} {suffix}");
            if !self
                .documents
                .iter()
                .any(|doc| doc.kind == 0 && doc.title == title)
            {
                return title;
            }
        }
        unreachable!()
    }

    pub fn current_contract_view(&self) -> Option<&super::contract_state::ContractView> {
        self.documents
            .get(self.active_document as usize)
            .filter(|doc| doc.kind == 0)
            .map(|doc| &doc.contract_view)
    }
    pub fn current_contract_view_mut(
        &mut self,
    ) -> Option<&mut super::contract_state::ContractView> {
        self.documents
            .get_mut(self.active_document as usize)
            .filter(|doc| doc.kind == 0)
            .map(|doc| &mut doc.contract_view)
    }
    pub fn current_document_id(&self) -> Option<String> {
        self.documents
            .get(self.active_document as usize)
            .map(|doc| doc.id.clone())
    }
    pub fn restore_contract_view(&mut self) {
        if let Some(view) = self.current_contract_view().cloned() {
            self.address_input = view
                .target
                .as_ref()
                .map(|target| target.address.clone())
                .unwrap_or_default();
            self.selected_function = view.selected_function;
            self.call_output = view
                .result
                .as_ref()
                .map(|result| result.simple.clone())
                .unwrap_or_default();
            self.storage_output = view.storage;
        }
    }

    fn open_contract_doc(&mut self, name: String, functions: Vec<FunctionInfo>) {
        let body = if functions.is_empty() {
            format!("{name}\n\nNo ABI functions in this compile output.")
        } else {
            format!(
                "{name}\n\nFunctions:\n{}",
                functions
                    .iter()
                    .map(|f| f.name.clone())
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        };
        let title = self.next_contract_title(&name);
        let mut doc = Document::new(title, body, 0);
        doc.contract_name = Some(name.clone());
        self.documents.push(doc);
        self.select_document(self.documents.len() as i32 - 1);
        self.status_left = format!("Selected {name}");
    }

    pub fn open_deploy_doc(&mut self) {
        let owner = self
            .documents
            .get(self.active_document as usize)
            .and_then(|doc| match doc.kind {
                0 => Some(doc.id.clone()),
                1 => doc.deployment_origin_document.clone(),
                _ => None,
            });
        if let Some(owner) = owner {
            self.open_deploy_for_document(&owner);
        } else {
            self.notice = "Select a contract document before deploying.".into();
        }
    }

    pub fn open_deploy_for_document(&mut self, owner_id: &str) -> bool {
        if self.transition_pending() || !self.workspace_ready || self.deploy_busy {
            return false;
        }
        let Some(owner) = self
            .documents
            .iter()
            .find(|doc| doc.kind == 0 && doc.id == owner_id)
        else {
            self.notice = "The originating contract document is no longer open.".into();
            return false;
        };
        let Some(name) = owner.contract_name().map(str::to_string) else {
            return false;
        };
        let owner_title = owner.title.clone();
        let existing = self.documents.iter().position(|doc| {
            doc.kind == 1 && doc.deployment_origin_document.as_deref() == Some(owner_id)
        });
        let index = match existing {
            Some(index) => index,
            None => {
                let mut doc = Document::new(format!("Deploy · {owner_title}"), String::new(), 1);
                doc.contract_name = Some(name.clone());
                doc.deployment_origin_document = Some(owner_id.to_string());
                self.documents.push(doc);
                self.documents.len() - 1
            }
        };
        self.select_document(index as i32);
        self.status_left = format!("Deploy {name}");
        true
    }

    pub fn deployment_origin_document(&self) -> Option<String> {
        self.documents
            .get(self.active_document as usize)
            .and_then(|doc| match doc.kind {
                0 => Some(doc.id.clone()),
                1 => doc.deployment_origin_document.clone(),
                _ => None,
            })
    }

    pub fn complete_deployment(
        &mut self,
        origin_document_id: &str,
        contract_name: &str,
        address: &str,
        token: &RequestToken,
    ) -> Result<Option<String>, String> {
        if token.operation != Operation::Deploy
            || !self.active
            || self.workspace_epoch != token.workspace_epoch
            || self.network_epoch != token.network_epoch
            || self.rpc != token.rpc
        {
            return Err("Deployment completed in a different workspace or network context; its address was not bound.".into());
        }
        if contract_name.is_empty() || address.parse::<ethers::types::Address>().is_err() {
            return Err("Deployment returned an invalid contract name or address; its address was not bound.".into());
        }
        let deployment = Deployment {
            name: contract_name.to_string(),
            address: address.to_string(),
        };
        let key = deployment_key(&deployment);
        if self
            .deployments
            .iter()
            .any(|item| deployment_key(item) == key)
        {
            return Err("This deployment is already recorded; no document was rebound.".into());
        }
        if self
            .deployment_epochs
            .get(&key)
            .is_some_and(|epoch| *epoch != token.network_epoch)
        {
            return Err(
                "This address was recorded in an earlier network context; no document was rebound."
                    .into(),
            );
        }
        let origin = self
            .documents
            .iter()
            .position(|doc| doc.kind == 0 && doc.id == origin_document_id);
        if origin.is_some_and(|index| self.documents[index].contract_name() != Some(contract_name))
        {
            return Err("The originating document no longer refers to this contract; its address was not bound.".into());
        }
        self.deployment_epochs.insert(key, token.network_epoch);
        self.deployments.push(deployment);
        let id = self
            .deployment_id(self.deployments.len() - 1)
            .expect("new deployment ID");
        let Some(origin) = origin else {
            self.notice =
                "Deployment recorded, but its originating contract document was closed.".into();
            return Ok(None);
        };
        let target = super::contract_state::Target {
            contract: contract_name.to_string(),
            address: address.to_string(),
            deployment_id: Some(id),
            network_epoch: token.network_epoch,
        };
        let index = if self.documents[origin].contract_view.target.is_none() {
            self.documents[origin].contract_view.target = Some(target);
            self.documents[origin].contract_view.revision = self.documents[origin]
                .contract_view
                .revision
                .wrapping_add(1);
            origin
        } else {
            let title = self.next_contract_title(contract_name);
            let mut doc = Document::new(title, self.documents[origin].body.clone(), 0);
            doc.contract_name = Some(contract_name.to_string());
            doc.contract_view.target = Some(target);
            self.documents.push(doc);
            self.documents.len() - 1
        };
        self.view_revision = self.view_revision.wrapping_add(1);
        if self.active_document == index as i32 {
            self.restore_contract_view();
        }
        Ok(Some(self.documents[index].id.clone()))
    }

    pub fn set_contract_address(&mut self, address: String) -> bool {
        if self.transition_pending()
            || !self.workspace_ready
            || self.send_busy
            || self.deploy_busy
            || self.wallet_busy
            || self.rpc_busy
            || address.len() > 42
            || (!address.is_empty() && address.parse::<ethers::types::Address>().is_err())
        {
            return false;
        }
        let Some(index) = usize::try_from(self.active_document)
            .ok()
            .filter(|index| self.documents.get(*index).is_some_and(|doc| doc.kind == 0))
        else {
            return false;
        };
        let Some(contract) = self.documents[index].contract_name().map(str::to_string) else {
            return false;
        };
        let old_target = self.documents[index].contract_view.target.clone();
        if old_target
            .as_ref()
            .is_some_and(|target| target.address == address)
            || (address.is_empty() && old_target.is_none())
        {
            self.address_input = address;
            return true;
        }
        let target = if address.is_empty() {
            None
        } else {
            Some(super::contract_state::Target {
                contract,
                address: address.clone(),
                deployment_id: None,
                network_epoch: old_target
                    .as_ref()
                    .map(|target| target.network_epoch)
                    .unwrap_or(self.network_epoch),
            })
        };
        let view = &mut self.documents[index].contract_view;
        view.target = target;
        view.revision = view.revision.wrapping_add(1);
        view.call_generation = view.call_generation.wrapping_add(1);
        view.storage_generation = view.storage_generation.wrapping_add(1);
        view.result = None;
        view.storage.clear();
        view.storage_raw = None;
        view.storage_busy = false;
        view.storage_status.clear();
        view.call_status.clear();
        self.address_input = address;
        self.call_output.clear();
        self.storage_output.clear();
        self.view_revision = self.view_revision.wrapping_add(1);
        true
    }

    fn deployment_context(&self, document_id: &str) -> Option<(String, &ContractItem)> {
        let doc = self.documents.iter().find(|doc| doc.id == document_id)?;
        let owner = match doc.kind {
            0 => doc.id.as_str(),
            1 => doc.deployment_origin_document.as_deref()?,
            _ => return None,
        };
        let owner_doc = self
            .documents
            .iter()
            .find(|doc| doc.kind == 0 && doc.id == owner)?;
        let name = owner_doc.contract_name()?;
        if doc.contract_name() != Some(name) {
            return None;
        }
        let target = unique_contract(&self.contracts, name).filter(|item| item.kind == "src")?;
        Some((owner.to_string(), target))
    }

    pub fn deployment_draft_for_document(&self, document_id: &str) -> DeploymentDraft {
        let Some((owner, target)) = self.deployment_context(document_id) else {
            return DeploymentDraft::new(None);
        };
        self.deployment_drafts
            .get(&owner)
            .cloned()
            .unwrap_or_else(|| DeploymentDraft::new(target.constructor.as_ref()))
    }

    pub fn selected_deployment_draft(&self) -> DeploymentDraft {
        self.current_document_id()
            .map(|id| self.deployment_draft_for_document(&id))
            .unwrap_or_else(|| DeploymentDraft::new(None))
    }

    pub fn edit_constructor_argument(&mut self, index: i32, value: String) -> bool {
        let Ok(index) = usize::try_from(index) else {
            return false;
        };
        let Some(id) = self.current_document_id() else {
            return false;
        };
        let Some((owner, target)) = self.deployment_context(&id) else {
            return false;
        };
        let Some(spec) = target.constructor.as_ref() else {
            return false;
        };
        if index >= spec.inputs.len() {
            return false;
        }
        let initial = DeploymentDraft::new(Some(spec));
        let draft = self.deployment_drafts.entry(owner).or_insert(initial);
        let Some(field) = draft.values.get_mut(index) else {
            return false;
        };
        *field = value;
        true
    }

    pub fn reset_constructor_draft(&mut self) -> bool {
        if self.deploy_busy {
            return false;
        }
        let Some(id) = self.current_document_id() else {
            return false;
        };
        let Some((owner, target)) = self.deployment_context(&id) else {
            return false;
        };
        let mut draft = DeploymentDraft::new(target.constructor.as_ref());
        if let Some(existing) = self.deployment_drafts.get(&owner) {
            draft.value_wei = existing.value_wei.clone();
        }
        self.deployment_drafts.insert(owner, draft);
        true
    }

    pub fn edit_deployment_value(&mut self, value: String) {
        let Some(id) = self.current_document_id() else {
            return;
        };
        let Some((owner, target)) = self.deployment_context(&id) else {
            return;
        };
        let initial = DeploymentDraft::new(target.constructor.as_ref());
        self.deployment_drafts
            .entry(owner)
            .or_insert(initial)
            .value_wei = value;
    }

    pub fn deployment_problem(&self) -> String {
        let Some(id) = self.current_document_id() else {
            return "Select a compiled contract before deploying.".into();
        };
        self.deployment_problem_for_document(&id)
    }

    pub fn deployment_problem_for_document(&self, document_id: &str) -> String {
        let Some((_, target)) = self.deployment_context(document_id) else {
            return "The selected contract is not in the current compile output or its originating document is closed. Compile it again before deploying.".into();
        };
        if self.transition_pending() || !self.workspace_ready || self.wallet_busy || self.send_busy
        {
            return "Finish the current confirmation or wallet operation before deploying.".into();
        }
        if self.deploy_busy {
            return "A deployment is already in progress. Wait for it to finish before submitting again.".into();
        }
        if let Some(error) = &target.constructor_error {
            return format!("Cannot read the constructor ABI: {error}. Recompile the contract with a valid ABI.");
        }
        let code = target
            .bytecode
            .strip_prefix("0x")
            .unwrap_or(&target.bytecode);
        if code.is_empty()
            || code.len() % 2 != 0
            || !code.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return "This contract has no valid creation bytecode. Compile a deployable contract and link any required libraries.".into();
        }
        let draft = self.deployment_draft_for_document(document_id);
        let validation = validate_draft(target.constructor.as_ref(), &draft);
        if !validation.is_valid() {
            let mut errors = Vec::new();
            if !validation.form_error.is_empty() {
                errors.push(validation.form_error);
            }
            for (index, error) in validation
                .field_errors
                .iter()
                .enumerate()
                .filter(|(_, error)| !error.is_empty())
            {
                let label = target
                    .constructor
                    .as_ref()
                    .and_then(|spec| spec.inputs.get(index))
                    .map(|param| {
                        if param.name.is_empty() {
                            format!("Constructor argument {} ({})", index + 1, param.kind)
                        } else {
                            format!(
                                "Constructor argument {} \"{}\" ({})",
                                index + 1,
                                param.name,
                                param.kind
                            )
                        }
                    })
                    .unwrap_or_else(|| format!("Constructor argument {}", index + 1));
                errors.push(format!("{label}: {error}"));
            }
            if !validation.value_error.is_empty() {
                errors.push(format!(
                    "Deployment value (wei): {}",
                    validation.value_error
                ));
            }
            return if errors.is_empty() {
                "Review the constructor arguments and deployment value.".into()
            } else {
                errors.join("\n")
            };
        }
        if self
            .wallet_key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .is_none()
        {
            return "Choose a wallet key before deploying. Constructor drafts can be edited without a key.".into();
        }
        String::new()
    }

    pub fn deployment_can_submit(&self) -> bool {
        self.deployment_problem().is_empty()
    }

    pub fn use_anvil_key(&mut self, index: i32) {
        if !self.can_start_wallet_operation() {
            return;
        }
        let Some((address, key)) = anvil_dev_account(index) else {
            self.notice = "Choose one of the first three Anvil dev accounts.".into();
            return;
        };
        self.wallet_generation = self.wallet_generation.wrapping_add(1);
        self.wallet_source = None;
        self.wallet_key = Some(key.to_string());
        self.wallet_address = address.to_string();
        self.notice.clear();
        self.status_left = format!("Using Anvil dev key #{index}");
        self.push_log(format!("Using Anvil dev key #{index} (local only)"));
    }

    pub fn remove_deployment(&mut self, index: i32) -> bool {
        let Ok(index) = usize::try_from(index) else {
            return false;
        };
        if index >= self.deployments.len() {
            return false;
        }
        let removed_id = self.deployment_id(index);
        let removed = self.deployments.remove(index);
        let mut detached = false;
        for doc in self.documents.iter_mut().filter(|doc| doc.kind == 0) {
            if let Some(target) = doc.contract_view.target.as_mut() {
                if target.deployment_id == removed_id && removed_id.is_some() {
                    target.deployment_id = None;
                    doc.contract_view.revision = doc.contract_view.revision.wrapping_add(1);
                    detached = true;
                }
            }
        }
        if !detached
            && self.current_contract_view().is_none()
            && self.address_input == removed.address
        {
            self.address_input.clear();
        }
        self.view_revision = self.view_revision.wrapping_add(1);
        if detached {
            self.notice = format!("Removed the {} deployment record. Open tabs retain their address as an attached target.", removed.name);
        }
        self.status_left = format!("Removed deployment {}", removed.name);
        true
    }

    pub fn start_sidebar_drag(&mut self, pointer_x: f32) {
        if pointer_x.is_finite() {
            self.sidebar_drag = Some((pointer_x, self.sidebar_width));
            self.sidebar_collapsed = false;
        }
    }

    pub fn drag_sidebar(&mut self, pointer_x: f32) {
        if let Some((start_x, width)) = self.sidebar_drag {
            self.set_sidebar_width(width + (pointer_x - start_x) / self.zoom);
        }
    }

    pub fn set_sidebar_width(&mut self, width: f32) {
        if !width.is_finite() {
            return;
        }
        self.sidebar_width = width.clamp(180.0, 460.0);
    }

    pub fn open_trace_doc(&mut self) {
        if let Some(existing) = self.documents.iter().position(|doc| doc.kind == 3) {
            self.active_document = existing as i32;
        } else {
            self.documents
                .push(Document::new("Trace".into(), String::new(), 3));
            self.active_document = self.documents.len() as i32 - 1;
        }
        self.activity = 0;
        self.status_left = "Trace".into();
    }

    pub fn open_wallet_doc(&mut self) {
        if let Some(existing) = self.documents.iter().position(|doc| doc.kind == 2) {
            self.active_document = existing as i32;
        } else {
            self.documents
                .push(Document::new("Wallet".into(), String::new(), 2));
            self.active_document = self.documents.len() as i32 - 1;
        }
        self.activity = 0;
        self.status_left = "Wallet".into();
    }

    pub fn select_document(&mut self, index: i32) {
        if let Some(doc) = self.documents.get(index as usize) {
            self.view_revision = self.view_revision.wrapping_add(1);
            self.activity = match doc.kind {
                4 => 1,
                5 => 2,
                6 => 3,
                7 => 3,
                _ => 0,
            };
            self.active_document = index;
            if (0..=3).contains(&doc.kind) {
                self.last_contracts_document = Some(doc.id.clone());
            }
            if matches!(doc.kind, 0 | 1) {
                self.selected_contract = doc.contract_name().map(str::to_string);
            }
            self.selected_function = -1;
            self.restore_contract_view();
        }
    }

    pub fn close_document(&mut self, index: i32) {
        if (index as usize) >= self.documents.len() {
            return;
        }
        let removed = self.documents.remove(index as usize);
        if self.last_contracts_document.as_deref() == Some(removed.id.as_str()) {
            self.last_contracts_document = None;
        }
        if self.documents.is_empty() {
            self.active_document = -1;
            self.selected_contract = None;
            self.selected_function = -1;
        } else if index == self.active_document {
            self.select_document(index.min(self.documents.len() as i32 - 1));
        } else if index < self.active_document {
            self.active_document -= 1;
        }
        if self.documents.is_empty() {
            self.view_revision = self.view_revision.wrapping_add(1);
        }
    }

    pub fn selected_contract_functions(&self) -> Vec<FunctionInfo> {
        let Some(name) = self.selected_contract.as_deref() else {
            return Vec::new();
        };
        unique_contract(&self.contracts, name)
            .map(|item| item.functions.clone())
            .unwrap_or_default()
    }

    pub fn select_function(&mut self, index: i32) {
        if let Some(owner) = self.current_document_id() {
            self.select_document_function(&owner, index);
        }
    }

    pub fn select_document_function(&mut self, owner_id: &str, index: i32) -> bool {
        if self.transition_pending() || !self.workspace_ready || self.send_busy {
            return false;
        }
        let Some(doc_index) = self
            .documents
            .iter()
            .position(|doc| doc.kind == 0 && doc.id == owner_id)
        else {
            return false;
        };
        let count = self.documents[doc_index]
            .contract_name()
            .and_then(|name| unique_contract(&self.contracts, name))
            .map(|item| item.functions.len())
            .unwrap_or(0);
        let selected = if index >= 0 && (index as usize) < count {
            index
        } else {
            -1
        };
        let view = &mut self.documents[doc_index].contract_view;
        if view.selected_function != selected {
            view.selected_function = selected;
            view.revision = view.revision.wrapping_add(1);
            self.view_revision = self.view_revision.wrapping_add(1);
        }
        if self.active_document == doc_index as i32 {
            self.restore_contract_view();
        }
        selected >= 0
    }

    pub fn selected_function_detail(&self) -> (String, Vec<(String, String)>) {
        let functions = self.selected_contract_functions();
        match functions.get(self.selected_function as usize) {
            Some(item) => (item.name.clone(), item.params.clone()),
            None => (String::new(), Vec::new()),
        }
    }

    pub fn selected_function_sig(&self) -> Vec<String> {
        let functions = self.selected_contract_functions();
        functions
            .get(self.selected_function as usize)
            .map(|item| item.params.iter().map(|(_, kind)| kind.clone()).collect())
            .unwrap_or_default()
    }

    pub fn document_body(&self) -> String {
        if self.current_doc_kind() == 3 {
            return self.trace_output.clone();
        }
        if self.activity == 4 {
            return self.debugger_body.clone();
        }
        if self.activity == 3 {
            return self.macros.get(self.submenu as usize).map(|item| item.body.clone()).unwrap_or_else(|| {
                "Macros stay in this session when you switch activities. A new macro is a note until its steps are connected to the workspace client.".into()
            });
        }
        self.documents
            .get(self.active_document as usize)
            .map(|doc| doc.body.clone())
            .unwrap_or_else(|| {
                "Select a contract from the sidebar. Open tabs stay when activities change.".into()
            })
    }

    pub fn new_macro(&mut self) {
        let name = format!("Macro {}", self.macros.len() + 1);
        self.macros.push(MacroNote {
            name: name.clone(),
            body: "No steps yet. This record is local to the desktop session and is not executed."
                .into(),
        });
        self.submenu = self.macros.len() as i32 - 1;
        self.open_tool_document(6, "Macros");
        self.push_log(format!("Created {name}"));
    }

    pub fn convert(&mut self, raw: &str) {
        self.converter_input = raw.to_string();
        self.recalculate_converter();
    }

    pub fn set_converter_mode(&mut self, mode: i32) {
        let count = match self.converter_tool {
            0 => 4,
            1 => 3,
            2 => 2,
            _ => 1,
        };
        if !(0..count).contains(&mode) {
            return;
        }
        self.converter_mode = mode;
        self.recalculate_converter();
    }

    fn recalculate_converter(&mut self) {
        match super::converter::convert(
            self.converter_tool,
            self.converter_mode,
            &self.converter_input,
        ) {
            Ok(output) => {
                self.converter_output = output;
                self.converter_error.clear();
            }
            Err(error) => {
                self.converter_output.clear();
                self.converter_error = error;
            }
        }
    }

    pub fn push_log(&mut self, line: String) {
        let seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            % 86400;
        self.logs.push(format!(
            "[{:02}:{:02}:{:02} UTC] {line}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        ));
        if self.logs.len() > 200 {
            self.logs.drain(0..self.logs.len() - 200);
        }
    }

    pub fn begin_request(
        &mut self,
        operation: Operation,
        name: &str,
        view_sensitive: bool,
    ) -> RequestToken {
        let id = self.job_tracker.begin(operation);
        self.jobs.push(JobEntry {
            id,
            name: name.into(),
            status: "running".into(),
            output: String::new(),
            context: self
                .selected_contract
                .clone()
                .unwrap_or_else(|| self.heading()),
            transaction_hash: None,
        });
        jobs::prune(&mut self.jobs);
        self.dock_tab = 1;
        RequestToken {
            id,
            operation,
            workspace_epoch: self.workspace_epoch,
            network_epoch: self.network_epoch,
            rpc: self.rpc.clone(),
            view_revision: view_sensitive.then_some(self.view_revision),
        }
    }

    pub fn accepts_request(&self, token: &RequestToken) -> bool {
        self.active
            && !self.workspace_busy
            && token.workspace_epoch == self.workspace_epoch
            && token.network_epoch == self.network_epoch
            && token.rpc == self.rpc
            && token
                .view_revision
                .map(|revision| revision == self.view_revision)
                .unwrap_or(true)
            && self.job_tracker.is_latest(token)
    }

    pub fn finish_request(
        &mut self,
        token: &RequestToken,
        result: &Result<String, String>,
        transaction_hash: Option<String>,
    ) {
        if let Some(job) = self.jobs.iter_mut().find(|job| job.id == token.id) {
            job.status = if result.is_ok() { "ok" } else { "failed" }.into();
            job.output = jobs::bounded_output(match result {
                Ok(output) | Err(output) => output,
            });
            job.transaction_hash = transaction_hash;
        }
        jobs::prune(&mut self.jobs);
    }

    pub fn operation_status(&self, operation: Operation) -> String {
        self.jobs
            .iter()
            .rev()
            .find(|job| {
                job.status == "running"
                    && match operation {
                        Operation::Compile => job.name == "compile workspace",
                        Operation::Deploy => job.name == "deploy contract",
                        Operation::Call => job.name == "send call" || job.name == "simulate call",
                        Operation::Storage => job.name == "read storage",
                        Operation::Explorer => job.name == "explorer lookup",
                        Operation::Chain => job.name == "refresh chain",
                    }
            })
            .map(|job| format!("{}…", job.name))
            .unwrap_or_default()
    }

    pub fn selected_job_output(&self) -> String {
        self.selected_job
            .and_then(|id| self.jobs.iter().find(|job| job.id == id))
            .map(|job| {
                format!(
                    "{}\n{}\n{}\n\n{}",
                    job.name, job.context, job.status, job.output
                )
            })
            .unwrap_or_else(|| "Select a job to inspect its result.".into())
    }

    pub fn transition_pending(&self) -> bool {
        !self.active
            || self.workspace_busy
            || self.project_opening
            || self.folder_picker_busy
            || self.security_confirmation
            || self.pending_workspace.is_some()
            || self.pending_keystore_delete.is_some()
    }

    pub fn can_start_wallet_operation(&self) -> bool {
        self.workspace_ready
            && !self.transition_pending()
            && !self.wallet_busy
            && !self.deploy_busy
            && !self.send_busy
    }

    pub fn can_change_workspace(&self) -> bool {
        self.active
            && !self.workspace_busy
            && !self.project_opening
            && !self.folder_picker_busy
            && !self.security_confirmation
            && !self.wallet_busy
            && !self.deploy_busy
            && !self.send_busy
            && !self.rpc_busy
            && self.pending_keystore_delete.is_none()
    }

    pub fn begin_wallet_operation(&mut self) -> Option<u64> {
        if !self.can_start_wallet_operation() {
            return None;
        }
        self.wallet_generation = self.wallet_generation.wrapping_add(1);
        self.wallet_busy = true;
        Some(self.wallet_generation)
    }

    pub fn accepts_wallet_result(&self, generation: u64) -> bool {
        self.active
            && !self.workspace_busy
            && self.wallet_busy
            && self.wallet_generation == generation
    }

    pub fn finish_keystore_delete(&mut self, name: &str) {
        self.wallet_generation = self.wallet_generation.wrapping_add(1);
        self.wallet_busy = false;
        self.pending_keystore_delete = None;
        if self.wallet_source.as_deref() == Some(name) {
            self.wallet_key = None;
            self.wallet_address.clear();
            self.wallet_source = None;
        }
        self.refresh_keystores();
        self.status_left = format!("Deleted keystore {name}");
        self.notice.clear();
    }

    pub fn refresh_keystores(&mut self) {
        match self
            .keystore_root
            .clone()
            .ok_or_else(|| "Could not resolve the keystore folder.".to_string())
            .and_then(|root| super::keystore::KeystoreStore::new(root).list())
        {
            Ok(names) => {
                self.keystores = names;
                self.keystore_error.clear();
            }
            Err(error) => {
                self.keystores.clear();
                self.keystore_error = error;
            }
        }
        if !self
            .keystores
            .iter()
            .any(|name| name == &self.selected_keystore)
        {
            self.selected_keystore = self.keystores.first().cloned().unwrap_or_default();
        }
        self.push_log(format!("Loaded {} keystore(s)", self.keystores.len()));
    }

    pub fn select_keystore(&mut self, index: i32) {
        if let Some(name) = self.keystores.get(index as usize) {
            self.selected_keystore = name.clone();
        }
    }

    pub fn load_macros(&mut self) {
        let path = self.workspace.join(".chasm").join("desktop-macros.json");
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        let Ok(values) = serde_json::from_str::<Vec<Value>>(&text) else {
            return;
        };
        self.macros = values
            .into_iter()
            .filter_map(|value| {
                Some(MacroNote {
                    name: value.get("name")?.as_str()?.to_string(),
                    body: value.get("body")?.as_str()?.to_string(),
                })
            })
            .collect();
    }

    pub fn save_macros(&self) {
        let dir = self.workspace.join(".chasm");
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let values: Vec<_> = self
            .macros
            .iter()
            .map(|item| json!({"name": item.name, "body": item.body}))
            .collect();
        let _ = std::fs::write(
            dir.join("desktop-macros.json"),
            serde_json::to_string_pretty(&values).unwrap_or_else(|_| "[]".into()),
        );
    }

    pub fn clear_logs(&mut self) {
        self.logs.clear();
    }

    pub fn set_zoom_level(&mut self, index: i32) -> bool {
        let Some(&zoom) = ZOOM_LEVELS.get(index as usize) else {
            return false;
        };
        self.zoom = zoom;
        self.zoom_index = index;
        true
    }
}

fn zoom_index_for(zoom: f32) -> i32 {
    ZOOM_LEVELS
        .iter()
        .position(|&level| level == zoom)
        .unwrap_or(0) as i32
}

pub fn load_zoom(workspace: &Path) -> f32 {
    std::fs::read_to_string(workspace.join(".chasm").join("desktop-settings.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| value.get("zoom").and_then(Value::as_f64))
        .map(|zoom| zoom as f32)
        .filter(|zoom| ZOOM_LEVELS.contains(zoom))
        .unwrap_or(1.0)
}

pub fn save_zoom(workspace: &Path, zoom: f32) -> Result<(), String> {
    if !ZOOM_LEVELS.contains(&zoom) {
        return Err("Unsupported zoom level.".into());
    }
    save_setting_fields(workspace, &[("zoom", json!(zoom))])
}

pub fn load_sidebar(workspace: &Path) -> (f32, bool) {
    let settings = std::fs::read_to_string(workspace.join(".chasm").join("desktop-settings.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .unwrap_or(Value::Null);
    let width = settings
        .get("sidebarWidth")
        .and_then(Value::as_f64)
        .map(|value| value as f32)
        .filter(|value| value.is_finite() && (180.0..=460.0).contains(value))
        .unwrap_or(250.0);
    (
        width,
        settings
            .get("sidebarCollapsed")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    )
}

pub fn save_sidebar(workspace: &Path, width: f32, collapsed: bool) -> Result<(), String> {
    if !width.is_finite() || !(180.0..=460.0).contains(&width) {
        return Err("Invalid sidebar width.".into());
    }
    save_setting_fields(
        workspace,
        &[
            ("sidebarWidth", json!(width)),
            ("sidebarCollapsed", json!(collapsed)),
        ],
    )
}

pub(super) fn save_setting_fields(
    workspace: &Path,
    fields: &[(&str, Value)],
) -> Result<(), String> {
    let dir = workspace.join(".chasm");
    let path = dir.join("desktop-settings.json");
    let mut settings = match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str::<Value>(&text)
            .map_err(|error| format!("Invalid desktop settings: {error}"))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(error) => return Err(error.to_string()),
    };
    let object = settings
        .as_object_mut()
        .ok_or("Desktop settings must be a JSON object.")?;
    for (name, value) in fields {
        object.insert((*name).into(), value.clone());
    }
    let bytes = serde_json::to_vec_pretty(&settings).map_err(|error| error.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let mut temp = tempfile::NamedTempFile::new_in(&dir).map_err(|error| error.to_string())?;
    temp.write_all(&bytes).map_err(|error| error.to_string())?;
    temp.as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    let temp = temp.into_temp_path();
    #[cfg(windows)]
    let mut temp = temp;
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::ERROR_FILE_NOT_FOUND;
        use windows_sys::Win32::Storage::FileSystem::{
            ReplaceFileW, SetFileAttributesW, FILE_ATTRIBUTE_NORMAL,
        };

        let target: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let staging: Vec<u16> = temp
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // Both NUL-terminated path buffers remain valid throughout these calls.
        unsafe {
            if SetFileAttributesW(staging.as_ptr(), FILE_ATTRIBUTE_NORMAL) == 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            if ReplaceFileW(
                target.as_ptr(),
                staging.as_ptr(),
                std::ptr::null(),
                0,
                std::ptr::null(),
                std::ptr::null(),
            ) != 0
            {
                temp.disable_cleanup(true);
                return Ok(());
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_FILE_NOT_FOUND as i32) {
                return Err(error.to_string());
            }
        }
    }
    temp.persist(&path)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// Recursively render a Solidity ABI type, expanding tuple components so that
/// same-name overloads with differently shaped tuples stay distinguishable.
pub fn abi_type_label(entry: &Value) -> String {
    let raw = entry.get("type").and_then(Value::as_str).unwrap_or("");
    if let Some(suffix) = raw.strip_prefix("tuple") {
        let inner = entry
            .get("components")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(abi_type_label)
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default();
        format!("({inner}){suffix}")
    } else {
        raw.to_string()
    }
}

fn checked_abi_type_label(entry: &Value) -> Result<String, String> {
    fn check(entry: &Value, depth: usize, remaining: &mut usize) -> Result<(), String> {
        if depth > 32 || *remaining == 0 {
            return Err("ABI type nesting is too large.".into());
        }
        *remaining -= 1;
        let raw = entry
            .get("type")
            .and_then(Value::as_str)
            .ok_or("ABI parameter type is missing.")?;
        let end = raw.find('[').unwrap_or(raw.len());
        let (base, mut suffix) = raw.split_at(end);
        let mut dimensions = 0;
        while !suffix.is_empty() {
            let rest = suffix.strip_prefix('[').ok_or("Invalid ABI array type.")?;
            let close = rest.find(']').ok_or("Invalid ABI array type.")?;
            let size = &rest[..close];
            if !size.is_empty()
                && (!size.bytes().all(|byte| byte.is_ascii_digit())
                    || size.starts_with('0')
                    || size.parse::<usize>().is_err())
            {
                return Err("Invalid ABI array length.".into());
            }
            dimensions += 1;
            if depth + dimensions > 32 {
                return Err("ABI type nesting is too large.".into());
            }
            suffix = &rest[close + 1..];
        }
        match base {
            "address" | "bool" | "string" | "bytes" => (),
            "tuple" => {
                let components = entry
                    .get("components")
                    .and_then(Value::as_array)
                    .ok_or("Tuple components are missing.")?;
                for component in components {
                    check(component, depth + dimensions + 1, remaining)?;
                }
            }
            _ if base.starts_with("uint") || base.starts_with("int") => {
                let width = if let Some(width) = base.strip_prefix("uint") {
                    width
                } else {
                    &base[3..]
                };
                let bits = if width.is_empty() {
                    256
                } else {
                    if width.starts_with('0') || !width.bytes().all(|byte| byte.is_ascii_digit()) {
                        return Err("Invalid ABI integer width.".into());
                    }
                    width
                        .parse::<usize>()
                        .map_err(|_| "Invalid ABI integer width.")?
                };
                if bits == 0 || bits > 256 || bits % 8 != 0 {
                    return Err("Invalid ABI integer width.".into());
                }
            }
            _ if base.starts_with("bytes") => {
                let width = &base[5..];
                if width.starts_with('0')
                    || !width.bytes().all(|byte| byte.is_ascii_digit())
                    || !(1..=32).contains(&width.parse::<usize>().unwrap_or(0))
                {
                    return Err("Invalid ABI bytes width.".into());
                }
            }
            _ => return Err("Unsupported ABI parameter type.".into()),
        }
        if base != "tuple" && entry.get("components").is_some() {
            return Err("Only tuple types may declare components.".into());
        }
        Ok(())
    }
    check(entry, 0, &mut 4096)?;
    Ok(abi_type_label(entry))
}

pub struct CompileReport {
    pub contracts: Vec<ContractItem>,
    pub skipped: usize,
}

impl CompileReport {
    pub fn warning(&self) -> Option<String> {
        (self.skipped > 0).then(|| {
            format!(
                "Skipped {} artifact(s) with an invalid ABI or missing name.",
                self.skipped
            )
        })
    }
}

pub fn contracts_from_compile_report(raw: &str) -> Result<CompileReport, String> {
    let value: Value = serde_json::from_str(raw).map_err(|error| error.to_string())?;
    if value.get("type").and_then(Value::as_str) != Some("compile_success") {
        return Err(value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("compile failed")
            .to_string());
    }
    let Some(items) = value.get("contracts").and_then(Value::as_array) else {
        return Ok(CompileReport {
            contracts: Vec::new(),
            skipped: 0,
        });
    };
    let mut contracts = Vec::new();
    let mut skipped = 0;
    for item in items {
        let Some(name) = item
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
        else {
            skipped += 1;
            continue;
        };
        let kind = item
            .get("source_type")
            .and_then(Value::as_str)
            .unwrap_or("src")
            .to_string();
        let artifact = item.get("artifact");
        let bytecode = artifact
            .and_then(|a| a.pointer("/bytecode/object"))
            .and_then(Value::as_str)
            .map(|s| {
                if s.starts_with("0x") {
                    s.to_string()
                } else {
                    format!("0x{s}")
                }
            })
            .unwrap_or_default();
        let raw_abi = artifact.and_then(|a| a.get("abi")).unwrap_or(&Value::Null);
        let Ok(constructor) = ConstructorSpec::from_abi(raw_abi) else {
            skipped += 1;
            continue;
        };
        let Ok(abi) = serde_json::from_value::<ethers::abi::Abi>(raw_abi.clone()) else {
            skipped += 1;
            continue;
        };
        let constructor_sig = constructor
            .as_ref()
            .filter(|spec| !spec.inputs.is_empty())
            .map(|spec| {
                format!(
                    "constructor({})",
                    spec.inputs
                        .iter()
                        .map(|param| param.kind.to_string())
                        .collect::<Vec<_>>()
                        .join(",")
                )
            })
            .unwrap_or_default();
        let functions = raw_abi
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry.get("type").and_then(Value::as_str) == Some("function"))
            .map(|entry| -> Result<FunctionInfo, String> {
                let name = entry
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
                    .ok_or("Function name is missing.")?
                    .to_string();
                let inputs = entry
                    .get("inputs")
                    .and_then(Value::as_array)
                    .ok_or("Function inputs are missing.")?;
                let mut params = Vec::new();
                for input in inputs {
                    let kind = checked_abi_type_label(input)?;
                    let param_name = input
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    params.push((param_name, kind));
                }
                for output in entry
                    .get("outputs")
                    .and_then(Value::as_array)
                    .ok_or("Function outputs are missing.")?
                {
                    checked_abi_type_label(output)?;
                }
                let inputs = params
                    .iter()
                    .map(|(param_name, kind)| format!("{kind} {param_name}").trim().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                let mutability = entry
                    .get("stateMutability")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                Ok(FunctionInfo {
                    name,
                    inputs,
                    mutability,
                    params,
                })
            })
            .collect::<Result<Vec<_>, _>>();
        let Ok(functions) = functions else {
            skipped += 1;
            continue;
        };
        contracts.push(ContractItem {
            abi,
            name: name.to_string(),
            kind,
            functions,
            bytecode,
            constructor_sig,
            constructor,
            constructor_error: None,
        });
    }
    Ok(CompileReport { contracts, skipped })
}

pub fn contracts_from_compile(raw: &str) -> Result<Vec<ContractItem>, String> {
    let report = contracts_from_compile_report(raw)?;
    if let Some(warning) = report.warning() {
        return Err(warning);
    }
    Ok(report.contracts)
}

pub fn compile_workspace_report(
    root: &Path,
    remappings: Vec<String>,
) -> Result<CompileReport, String> {
    let compiler =
        Compiler::new(root.to_path_buf(), remappings).map_err(|error| error.to_string())?;
    let raw = compiler
        .compile_to_json()
        .map_err(|error| error.to_string())?;
    contracts_from_compile_report(&raw)
}

pub fn compile_workspace(
    root: &Path,
    remappings: Vec<String>,
) -> Result<Vec<ContractItem>, String> {
    let report = compile_workspace_report(root, remappings)?;
    if let Some(warning) = report.warning() {
        return Err(warning);
    }
    Ok(report.contracts)
}

// ── Foundry workspace client ────────────────────────────────────────────────

fn cast_command() -> std::process::Command {
    let mut command = std::process::Command::new("cast");
    command.stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    }
    command
}

fn run_cast(args: &[String]) -> Result<String, String> {
    let output = cast_command()
        .args(args)
        .output()
        .map_err(|error| format!("cast not found: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if output.status.success() {
        Ok(if stdout.trim().is_empty() {
            stderr
        } else {
            stdout
        })
    } else {
        Err(if stderr.trim().is_empty() {
            stdout
        } else {
            stderr
        })
    }
}

pub fn split_args(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    for ch in raw.chars() {
        match quote {
            Some(q) => {
                if ch == q {
                    quote = None;
                } else {
                    current.push(ch);
                }
            }
            None => match ch {
                '"' | '\'' => quote = Some(ch),
                '[' | '(' => {
                    depth += 1;
                    current.push(ch);
                }
                ']' | ')' => {
                    depth -= 1;
                    current.push(ch);
                }
                c if c.is_whitespace() && depth == 0 => {
                    if !current.is_empty() {
                        out.push(current.clone());
                        current.clear();
                    }
                }
                c => current.push(c),
            },
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

fn extract_address(line: &str) -> Option<String> {
    line.split(|c: char| c.is_whitespace() || c == ':' || c == ',' || c == '"')
        .map(str::trim)
        .find(|token| {
            token.len() == 42
                && token.starts_with("0x")
                && token[2..].chars().all(|c| c.is_ascii_hexdigit())
        })
        .map(str::to_string)
}

fn extract_key(line: &str) -> Option<String> {
    line.split(|c: char| c.is_whitespace() || c == ':' || c == '"')
        .map(str::trim)
        .find(|token| {
            token.len() == 66
                && token.starts_with("0x")
                && token[2..].chars().all(|c| c.is_ascii_hexdigit())
        })
        .map(str::to_string)
}

pub fn parse_field(text: &str, field: &str) -> Option<String> {
    let field = field.to_ascii_lowercase();
    text.lines()
        .find(|line| line.to_ascii_lowercase().contains(&field))
        .and_then(|line| extract_address(line).or_else(|| extract_key(line)))
}

pub fn cast_block_number(rpc: &str) -> Result<u64, String> {
    let out = run_cast(&["block-number".into(), "--rpc-url".into(), rpc.into()])?;
    out.trim()
        .parse::<u64>()
        .map_err(|_| format!("unexpected block-number output: {}", out.trim()))
}

#[cfg(test)]
pub fn cast_deploy_args(key: &str, creation: &[u8], value: &str) -> Result<Vec<String>, String> {
    if creation.is_empty() {
        return Err(
            "This contract has no creation bytecode (libraries and interfaces cannot be deployed)."
                .into(),
        );
    }
    let value = value.trim();
    if value.is_empty() {
        return Err("Deployment value is missing.".into());
    }
    // `cast send --create` is a subcommand. Flags after the bytecode are parsed as
    // its arguments, which is why `--value` must come before `--create`.
    Ok(vec![
        "send".into(),
        "--private-key".into(),
        key.into(),
        "--value".into(),
        value.into(),
        "--create".into(),
        format!("0x{}", ethers::utils::hex::encode(creation)),
    ])
}

pub fn cast_rpc(rpc: &str, method: &str, params: &[String]) -> Result<String, String> {
    let mut cmd: Vec<String> = vec!["rpc".into(), method.into()];
    cmd.extend(params.iter().cloned());
    cmd.push("--rpc-url".into());
    cmd.push(rpc.into());
    run_cast(&cmd)
}

pub fn validate_keystore_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Enter a keystore name.".into());
    }
    if name.len() > 64
        || name.contains("..")
        || !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err("Use a short name with letters, numbers, dashes, or underscores.".into());
    }
    Ok(name.to_string())
}

pub fn create_keystore(
    name: &str,
    password: &str,
    confirm: &str,
    imported_key: &str,
) -> Result<String, String> {
    let name = validate_keystore_name(name)?;
    if password.is_empty() {
        return Err("Enter a password for the new keystore.".into());
    }
    if password != confirm {
        return Err("The password and confirmation do not match.".into());
    }
    super::keystore::validate_filename(&name)?;
    let store = super::keystore::KeystoreStore::new(super::keystore::default_root()?);
    let dir = store.ensure_directory()?;
    if dir.join(&name).exists() {
        return Err(format!("A keystore named {name} already exists."));
    }
    let imported = imported_key.trim();
    let mut command = cast_command();
    if imported.is_empty() {
        // Foundry generates the keypair from the OS CSPRNG. No key is returned to the UI.
        command.args([
            "wallet",
            "new",
            &dir.display().to_string(),
            &name,
            "--unsafe-password",
            password,
        ]);
    } else {
        address_from_private_key(imported)?;
        command.args([
            "wallet",
            "import",
            &name,
            "--keystore-dir",
            &dir.display().to_string(),
            "--private-key",
            imported,
            "--unsafe-password",
            password,
        ]);
    }
    let output = command
        .output()
        .map_err(|error| format!("cast not found: {error}"))?;
    if output.status.success() {
        return Ok(name);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    Err(redact_keystore_secret(if stderr.trim().is_empty() {
        &stdout
    } else {
        &stderr
    }))
}

fn redact_keystore_secret(text: &str) -> String {
    let cleaned: Vec<&str> = text
        .lines()
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            !lower.contains("private") && extract_key(line).is_none()
        })
        .collect();
    let cleaned = cleaned.join("\n");
    if cleaned.trim().is_empty() {
        "Keystore creation failed.".into()
    } else {
        cleaned
    }
}

pub fn cast_unlock(name: &str, password: &str) -> Result<(String, String), String> {
    if name.is_empty() {
        return Err("Select a keystore first.".into());
    }
    let path =
        super::keystore::KeystoreStore::new(super::keystore::default_root()?).file_path(name)?;
    let output = cast_command()
        .arg("wallet")
        .arg("decrypt-keystore")
        .arg(path)
        .arg("--unsafe-password")
        .arg(password)
        .output()
        .map_err(|_| "Could not start Foundry to unlock the keystore.".to_string())?;
    if !output.status.success() {
        return Err(
            "Could not unlock the keystore. Check the password and file permissions.".into(),
        );
    }
    let output = String::from_utf8_lossy(&output.stdout);
    let key = output
        .lines()
        .find(|line| line.to_ascii_lowercase().contains("private key"))
        .and_then(extract_key)
        .or_else(|| output.lines().find_map(extract_key))
        .ok_or_else(|| "Could not unlock the keystore. Check the password.".to_string())?;
    let address = output
        .lines()
        .find(|line| line.to_ascii_lowercase().contains("address"))
        .and_then(extract_address)
        .or_else(|| address_from_private_key(&key).ok())
        .ok_or_else(|| {
            "The keystore unlocked, but its address could not be derived.".to_string()
        })?;
    Ok((key, address))
}

fn address_from_private_key(key: &str) -> Result<String, String> {
    let raw = key.trim().trim_start_matches("0x");
    let bytes = ethers::utils::hex::decode(raw)
        .map_err(|_| "The private key is not valid hex.".to_string())?;
    if bytes.len() != 32 {
        return Err("A private key must be 32 bytes.".into());
    }
    let mut secret = [0u8; 32];
    secret.copy_from_slice(&bytes);
    use ethers::signers::Signer;
    let wallet = ethers::signers::LocalWallet::from_bytes(&secret)
        .map_err(|_| "The private key is not valid.".to_string())?;
    Ok(format!("{:?}", wallet.address()))
}

// ── Debugger gateway client ─────────────────────────────────────────────────

pub async fn establish_debugger(client: &reqwest::Client) -> Result<String, String> {
    let response = client
        .get("http://127.0.0.1:3000/api/debugger/capabilities")
        .header("host", "127.0.0.1:3000")
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|_| "The local debugger gateway did not respond.".to_string())?;
    let status = response.status();
    let body = response.text().await.map_err(|error| error.to_string())?;
    if !status.is_success() {
        return Err(format!("debugger capabilities failed: {status} {body}"));
    }
    let value: Value = serde_json::from_str(&body).map_err(|error| error.to_string())?;
    value
        .get("csrfToken")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "debugger did not return an owner token".into())
}

pub async fn start_withdrawal_lab(client: &reqwest::Client, csrf: &str) -> Result<Value, String> {
    debugger_post(
        client,
        csrf,
        "/api/debugger/sessions",
        json!({
            "requestId": request_id(),
            "mode": "offline",
            "captureId": "source-demo",
            "buildInfoId": "source-demo-build"
        }),
    )
    .await
}

pub async fn debugger_command(
    client: &reqwest::Client,
    csrf: &str,
    session_id: &str,
    generation: u64,
    revision: u64,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    debugger_post(
        client,
        csrf,
        &format!("/api/debugger/sessions/{session_id}/commands"),
        json!({
            "requestId": request_id(),
            "generation": generation,
            "expectedRevision": revision,
            "method": method,
            "params": params
        }),
    )
    .await
}

pub async fn debugger_snapshot(client: reqwest::Client, session_id: &str) -> Result<Value, String> {
    let response = client
        .get(format!(
            "http://127.0.0.1:3000/api/debugger/sessions/{session_id}"
        ))
        .header("host", "127.0.0.1:3000")
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|_| "The local debugger gateway did not respond.".to_string())?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|_| "The debugger session response could not be read.".to_string())?;
    if !status.is_success() {
        return Err(format!("Debugger session request failed ({status})."));
    }
    serde_json::from_str(&body).map_err(|_| "The debugger returned invalid session JSON.".into())
}

pub async fn cancel_debugger(
    client: &reqwest::Client,
    csrf: &str,
    session_id: &str,
) -> Result<(), String> {
    let response = client
        .delete(format!(
            "http://127.0.0.1:3000/api/debugger/sessions/{session_id}"
        ))
        .header("host", "127.0.0.1:3000")
        .header("x-chasm-csrf", csrf)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|_| "Could not confirm debugger cancellation.".to_string())?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(response
            .text()
            .await
            .unwrap_or_else(|_| "cancel failed".into()))
    }
}

pub fn classify_chain_query(query: &str) -> ChainQuery {
    let query = query.trim();
    if is_address(query) {
        ChainQuery::Address(query.to_string())
    } else if is_hash(query) {
        ChainQuery::Hash(query.to_string())
    } else if !query.is_empty() && query.chars().all(|ch| ch.is_ascii_digit()) {
        ChainQuery::BlockNumber(query.to_string())
    } else {
        ChainQuery::Invalid
    }
}

fn is_address(value: &str) -> bool {
    value.len() == 42
        && value.starts_with("0x")
        && value[2..].chars().all(|ch| ch.is_ascii_hexdigit())
}

fn is_hash(value: &str) -> bool {
    value.len() == 66
        && value.starts_with("0x")
        && value[2..].chars().all(|ch| ch.is_ascii_hexdigit())
}

fn hex_quantity(value: &str) -> String {
    let value = value.trim();
    let number = if let Some(hex) = value.strip_prefix("0x") {
        u64::from_str_radix(hex, 16).ok()
    } else {
        value.parse::<u64>().ok()
    };
    match number {
        Some(number) => format!("0x{number:x}"),
        None => "0x0".into(),
    }
}

fn quantity_to_u64(value: &Value) -> Option<u64> {
    let text = value.as_str()?;
    u64::from_str_radix(text.trim_start_matches("0x"), 16).ok()
}

fn quantity_to_dec(value: &Value) -> String {
    quantity_to_u64(value)
        .map(|number| number.to_string())
        .unwrap_or_else(|| value.as_str().unwrap_or("—").to_string())
}

fn wei_to_ether(value: &Value) -> String {
    let text = value.as_str().unwrap_or("0x0");
    let wei = u128::from_str_radix(text.trim_start_matches("0x"), 16).unwrap_or(0);
    format_ether(wei)
}

fn short_hex(value: &str) -> String {
    if value.len() <= 16 {
        value.to_string()
    } else {
        format!("{}…{}", &value[..8], &value[value.len() - 6..])
    }
}

fn rpc_error(value: &Value) -> Option<String> {
    value.get("error").map(|error| {
        error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("RPC request failed")
            .to_string()
    })
}

async fn rpc_call(
    client: &reqwest::Client,
    rpc: &str,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let response = client
        .post(rpc)
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}))
        .send()
        .await
        .map_err(|error| format!("Could not reach the node: {error}"))?;
    let body = response.text().await.map_err(|error| error.to_string())?;
    let value: Value = serde_json::from_str(&body)
        .map_err(|error| format!("Node returned invalid JSON: {error}"))?;
    if let Some(error) = rpc_error(&value) {
        return Err(error);
    }
    Ok(value.get("result").cloned().unwrap_or(Value::Null))
}

pub async fn lookup_chain(rpc: &str, query: &str) -> Result<String, String> {
    let client = reqwest::Client::new();
    match classify_chain_query(query) {
        ChainQuery::Invalid => Err("Enter an address, a 32-byte hash, or a block number.".into()),
        ChainQuery::Address(address) => {
            let (balance, code, nonce) = tokio::try_join!(
                rpc_call(&client, rpc, "eth_getBalance", json!([address, "latest"])),
                rpc_call(&client, rpc, "eth_getCode", json!([address, "latest"])),
                rpc_call(
                    &client,
                    rpc,
                    "eth_getTransactionCount",
                    json!([address, "latest"])
                ),
            )?;
            let code = code.as_str().unwrap_or("0x");
            let kind = if code == "0x" || code.is_empty() {
                "Address"
            } else {
                "Contract"
            };
            Ok(format!(
                "{kind}\n{address}\n\nBalance   {} ETH\nNonce     {}\nCode      {} bytes",
                wei_to_ether(&balance),
                quantity_to_dec(&nonce),
                code.len().saturating_sub(2) / 2,
            ))
        }
        ChainQuery::Hash(hash) => {
            let tx = rpc_call(&client, rpc, "eth_getTransactionByHash", json!([hash])).await?;
            if !tx.is_null() {
                let receipt = rpc_call(&client, rpc, "eth_getTransactionReceipt", json!([hash]))
                    .await
                    .unwrap_or(Value::Null);
                return Ok(format_transaction(&tx, &receipt));
            }
            let block = rpc_call(&client, rpc, "eth_getBlockByHash", json!([hash, false])).await?;
            if block.is_null() {
                Err("No transaction or block has that hash.".into())
            } else {
                Ok(format_block(&block))
            }
        }
        ChainQuery::BlockNumber(number) => {
            let tag = hex_quantity(&number);
            let block = rpc_call(&client, rpc, "eth_getBlockByNumber", json!([tag, false])).await?;
            if block.is_null() {
                Err(format!("Block {number} does not exist."))
            } else {
                Ok(format_block(&block))
            }
        }
    }
}

fn format_block(block: &Value) -> String {
    let hash = block
        .get("hash")
        .and_then(Value::as_str)
        .unwrap_or("pending");
    let parent = block
        .get("parentHash")
        .and_then(Value::as_str)
        .unwrap_or("—");
    let txs = block
        .get("transactions")
        .and_then(Value::as_array)
        .map(|items| items.len())
        .unwrap_or(0);
    format!(
        "Block #{}\nHash        {hash}\nParent      {parent}\nTimestamp   {}\nTransactions {txs}\nGas used    {}",
        quantity_to_dec(block.get("number").unwrap_or(&Value::Null)),
        quantity_to_dec(block.get("timestamp").unwrap_or(&Value::Null)),
        quantity_to_dec(block.get("gasUsed").unwrap_or(&Value::Null)),
    )
}

fn format_transaction(tx: &Value, receipt: &Value) -> String {
    let hash = tx.get("hash").and_then(Value::as_str).unwrap_or("—");
    let to = tx
        .get("to")
        .and_then(Value::as_str)
        .unwrap_or("Contract creation");
    let status = match receipt.get("status").and_then(Value::as_str) {
        Some("0x1") => "success",
        Some("0x0") => "reverted",
        _ => "pending",
    };
    let created = receipt
        .get("contractAddress")
        .and_then(Value::as_str)
        .unwrap_or("");
    let created = if created.is_empty() {
        String::new()
    } else {
        format!("\nCreated     {created}")
    };
    format!(
        "Transaction\nHash    {hash}\nStatus  {status}\nFrom    {}\nTo      {to}\nValue   {} ETH\nBlock   {}{created}",
        tx.get("from").and_then(Value::as_str).unwrap_or("—"),
        wei_to_ether(tx.get("value").unwrap_or(&Value::Null)),
        quantity_to_dec(tx.get("blockNumber").unwrap_or(&Value::Null)),
    )
}

pub async fn latest_chain(
    rpc: &str,
    limit: u64,
) -> Result<(u64, Vec<ChainRow>, Vec<ChainRow>), String> {
    let client = reqwest::Client::new();
    let head = rpc_call(&client, rpc, "eth_blockNumber", json!([])).await?;
    let head = quantity_to_u64(&head).ok_or("The node returned an invalid block number.")?;
    let start = head.saturating_sub(limit.saturating_sub(1));
    let mut blocks = Vec::new();
    let mut transactions = Vec::new();
    for number in (start..=head).rev() {
        let block = rpc_call(
            &client,
            rpc,
            "eth_getBlockByNumber",
            json!([hex_quantity(&number.to_string()), true]),
        )
        .await?;
        if block.is_null() {
            continue;
        }
        let hash = block.get("hash").and_then(Value::as_str).unwrap_or("");
        let txs = block
            .get("transactions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        blocks.push(ChainRow {
            title: format!("#{number}"),
            detail: format!("{} tx · {}", txs.len(), short_hex(hash)),
            query: number.to_string(),
        });
        for tx in txs.iter().rev().take(8) {
            let tx_hash = tx.get("hash").and_then(Value::as_str).unwrap_or("");
            let to = tx
                .get("to")
                .and_then(Value::as_str)
                .map(short_hex)
                .unwrap_or_else(|| "create".into());
            transactions.push(ChainRow {
                title: short_hex(tx_hash),
                detail: format!(
                    "#{} · {} ETH · {to}",
                    number,
                    wei_to_ether(tx.get("value").unwrap_or(&Value::Null))
                ),
                query: tx_hash.to_string(),
            });
        }
    }
    transactions.truncate(24);
    Ok((head, blocks, transactions))
}

async fn debugger_post(
    client: &reqwest::Client,
    csrf: &str,
    path: &str,
    body: Value,
) -> Result<Value, String> {
    let response = client
        .post(format!("http://127.0.0.1:3000{path}"))
        .header("host", "127.0.0.1:3000")
        .header("x-chasm-csrf", csrf)
        .timeout(std::time::Duration::from_secs(10))
        .json(&body)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    let status = response.status();
    let text = response.text().await.map_err(|error| error.to_string())?;
    if !status.is_success() {
        return Err(format!("{status} {text}"));
    }
    serde_json::from_str(&text).map_err(|error| error.to_string())
}

fn request_id() -> String {
    format!(
        "desktop-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    )
}

fn format_ether(wei: u128) -> String {
    let whole = wei / 1_000_000_000_000_000_000;
    let frac = wei % 1_000_000_000_000_000_000;
    format!("{whole}.{frac:018}")
}

#[cfg(test)]
#[path = "session_contract_tests.rs"]
mod session_contract_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with_contracts() -> DesktopState {
        let mut state = DesktopState::new(PathBuf::from("."), 1.0);
        state.contracts = ["Vault", "Bank", "Token"]
            .into_iter()
            .map(|name| ContractItem {
                abi: Default::default(),
                name: name.into(),
                kind: "src".into(),
                functions: vec![FunctionInfo {
                    name: "withdraw".into(),
                    inputs: "uint256 amount".into(),
                    mutability: "nonpayable".into(),
                    params: vec![("amount".into(), "uint256".into())],
                }],
                bytecode: "0x6000".into(),
                constructor_sig: "constructor(uint256)".into(),
                constructor: None,
                constructor_error: None,
            })
            .collect();
        state
    }

    fn deployment_state() -> DesktopState {
        let mut state = DesktopState::new(PathBuf::from("."), 1.0);
        state.contracts = contracts_from_compile(
            &json!({
                "type": "compile_success",
                "contracts": [
                    {"name": "Bank", "source_type": "src", "artifact": {
                        "bytecode": {"object": "6000"},
                        "abi": [{"type": "constructor", "stateMutability": "payable", "inputs": [
                            {"name": "owner", "type": "address"},
                            {"name": "limit", "type": "uint256"},
                            {"name": "label", "type": "string"}
                        ]}]
                    }},
                    {"name": "Vault", "source_type": "src", "artifact": {
                        "bytecode": {"object": "6001"},
                        "abi": [{"type": "constructor", "stateMutability": "nonpayable", "inputs": [
                            {"name": "delay", "type": "uint256"}
                        ]}]
                    }}
                ]
            })
            .to_string(),
        )
        .unwrap();
        state
    }

    fn populated_state() -> DesktopState {
        let mut state = state_with_contracts();
        state.select_contract(0);
        state.open_deploy_doc();
        state.select_contract(1);
        state.open_wallet_doc();
        state.open_trace_doc();
        state.select_document(0);
        state.select_function(0);
        state.documents[0].body = "Existing document body".into();
        state.activity = 4;
        state.submenu = 2;
        state.deployments = vec![
            Deployment {
                name: "Vault".into(),
                address: "0x1111111111111111111111111111111111111111".into(),
            },
            Deployment {
                name: "Bank".into(),
                address: "0x2222222222222222222222222222222222222222".into(),
            },
            Deployment {
                name: "Vault".into(),
                address: "0x3333333333333333333333333333333333333333".into(),
            },
        ];
        state.macros.push(MacroNote {
            name: "Saved macro".into(),
            body: "Saved steps".into(),
        });
        state.logs.push("Session log".into());
        let existing = state.begin_request(Operation::Call, "Existing job", false);
        state.finish_request(&existing, &Ok(String::new()), None);
        state.notice = "Existing notice".into();
        state.status_left = "VM paused".into();
        state.compile_label = "3 contracts".into();
        state.debugger_label = "Debugger paused".into();
        state.debugger_body = "PC 7, stack [42], memory 0x1234".into();
        state.debugger_live = true;
        state.explorer_result = "Existing lookup".into();
        state.converter_output = "decimal 17\nhex 0x11".into();
        state.block_number = 91;
        state.keystores = vec!["local-key".into()];
        state.selected_keystore = "local-key".into();
        state.wallet_key = Some("existing-key".into());
        state.wallet_address = "0x4444444444444444444444444444444444444444".into();
        state.address_input = "0x5555555555555555555555555555555555555555".into();
        state.deploy_output = "Existing deploy output".into();
        state.deploy_busy = true;
        state.deployment_drafts.insert(
            "Vault".into(),
            DeploymentDraft {
                values: vec![" 42 ".into()],
                value_wei: "17".into(),
            },
        );
        state.deployment_drafts.insert(
            "Bank".into(),
            DeploymentDraft {
                values: vec!["[1, 2]".into()],
                value_wei: "0".into(),
            },
        );
        state.call_output = "Existing call output".into();
        state.storage_output = "Existing storage output".into();
        state.trace_output = "Existing trace".into();
        state.last_tx = format!("0x{}", "6".repeat(64));
        state.snapshot_id = "snapshot-3".into();
        state.menu_open = 2;
        state.dock_tab = 1;
        state.show_logs = false;
        state.rpc = "http://127.0.0.1:9545".into();
        state.csrf = "existing-owner-token".into();
        state.session_id = Some("existing-vm-session".into());
        state.generation = 7;
        state.revision = 11;
        state
    }

    fn state_snapshot(state: &DesktopState) -> Value {
        let mut snapshot = json!({
            "activity": state.activity,
            "submenu": state.submenu,
            "zoom": state.zoom,
            "zoom_index": state.zoom_index,
            "contracts": format!("{:?}", state.contracts),
            "documents": format!("{:?}", state.documents),
            "active_document": state.active_document,
            "selected_contract": state.selected_contract,
            "selected_function": state.selected_function,
            "deployments": format!("{:?}", state.deployments),
            "macros": format!("{:?}", state.macros),
            "logs": state.logs,
            "jobs": format!("{:?}", state.jobs),
            "notice": state.notice,
            "status_left": state.status_left,
            "compile_label": state.compile_label,
            "debugger_label": state.debugger_label,
            "debugger_body": state.debugger_body,
            "debugger_live": state.debugger_live,
            "explorer_result": state.explorer_result,
            "converter_output": state.converter_output,
            "block_number": state.block_number,
            "keystores": state.keystores,
            "selected_keystore": state.selected_keystore,
        });
        let remaining = json!({
            "wallet_key": state.wallet_key,
            "wallet_address": state.wallet_address,
            "address_input": state.address_input,
            "deploy_output": state.deploy_output,
            "deploy_busy": state.deploy_busy,
            "deployment_drafts": state.deployment_drafts.iter().map(|(name, draft)| {
                (name.clone(), json!({"values": draft.values, "value_wei": draft.value_wei}))
            }).collect::<serde_json::Map<String, Value>>(),
            "call_output": state.call_output,
            "storage_output": state.storage_output,
            "trace_output": state.trace_output,
            "last_tx": state.last_tx,
            "snapshot_id": state.snapshot_id,
            "menu_open": state.menu_open,
            "dock_tab": state.dock_tab,
            "show_logs": state.show_logs,
            "rpc": state.rpc,
            "csrf": state.csrf,
            "client": format!("{:?}", state.client),
            "session_id": state.session_id,
            "generation": state.generation,
            "revision": state.revision,
            "workspace": state.workspace,
        });
        snapshot
            .as_object_mut()
            .unwrap()
            .extend(remaining.as_object().unwrap().clone());
        snapshot
    }

    fn assert_unchanged_except(before: Value, after: Value, changed: &[&str]) {
        let mut before = before.as_object().unwrap().clone();
        let mut after = after.as_object().unwrap().clone();
        for key in changed {
            before.remove(*key);
            after.remove(*key);
        }
        assert_eq!(before, after);
    }

    #[test]
    fn compiled_constructor_keeps_bank_field_names_and_abi_types() {
        let state = deployment_state();
        let bank = &state.contracts[0];
        assert!(bank.constructor_error.is_none());
        let spec = bank.constructor.as_ref().unwrap();
        assert!(spec.payable);
        assert_eq!(
            spec.inputs
                .iter()
                .map(|param| (param.name.as_str(), param.kind.to_string()))
                .collect::<Vec<_>>(),
            vec![
                ("owner", "address".into()),
                ("limit", "uint256".into()),
                ("label", "string".into()),
            ]
        );
        assert_eq!(bank.constructor_sig, "constructor(address,uint256,string)");

        let raw = json!({"type": "compile_success", "contracts": [{"name": "Nested", "artifact": {
            "bytecode": {"object": "6000"},
            "abi": [{"type": "constructor", "stateMutability": "nonpayable", "inputs": [
                {"name": "records", "type": "tuple[][2]", "components": [
                    {"name": "owner", "type": "address"},
                    {"name": "config", "type": "tuple", "components": [{"name": "enabled", "type": "bool"}]}
                ]}
            ]}]
        }}]});
        let items = contracts_from_compile(&raw.to_string()).unwrap();
        assert!(items[0].constructor_error.is_none());
        let param = &items[0].constructor.as_ref().unwrap().inputs[0];
        assert_eq!(param.name, "records");
        assert_eq!(param.kind.to_string(), "(address,(bool))[][2]");
        assert_eq!(
            items[0].constructor_sig,
            "constructor((address,(bool))[][2])"
        );
    }

    #[test]
    fn deployment_drafts_are_keyless_per_target_and_survive_navigation() {
        let mut state = deployment_state();
        assert!(state.deployment_drafts.is_empty());
        assert!(state.selected_deployment_draft().values.is_empty());
        state.select_contract(0);
        let before = state_snapshot(&state);
        assert_eq!(state.selected_deployment_draft().values, vec!["", "", ""]);
        assert_eq!(state.selected_deployment_draft().value_wei, "0");
        assert_eq!(state_snapshot(&state), before);
        let owner = "0x1111111111111111111111111111111111111111";
        assert!(state.edit_constructor_argument(0, owner.into()));
        assert!(state.edit_constructor_argument(1, " 42 ".into()));
        assert!(state.edit_constructor_argument(2, "  Bank treasury  ".into()));
        state.edit_deployment_value("123".into());
        let bank = state.selected_deployment_draft();
        assert_eq!(bank.values, vec![owner, " 42 ", "  Bank treasury  "]);
        assert_eq!(state.wallet_key, None);
        assert!(!state.deployment_can_submit());
        assert!(state.deployment_problem().contains("wallet key"));
        state.open_deploy_doc();
        state.select_contract(1);
        assert_eq!(state.selected_deployment_draft().values, vec![""]);
        assert!(state.edit_constructor_argument(0, "77".into()));
        let vault = state.selected_deployment_draft();
        assert_eq!(vault.value_wei, "0");
        state.open_deploy_doc();
        for activity in 0..=4 {
            state.open_activity(activity);
            state.open_wallet_doc();
            state.open_trace_doc();
            state.select_document(1);
            assert_eq!(state.selected_contract.as_deref(), Some("Bank"));
            assert_eq!(state.selected_deployment_draft().values, bank.values);
            assert_eq!(state.selected_deployment_draft().value_wei, bank.value_wei);
            state.select_document(3);
            assert_eq!(state.selected_contract.as_deref(), Some("Vault"));
            assert_eq!(state.selected_deployment_draft().values, vault.values);
            assert_eq!(state.selected_deployment_draft().value_wei, vault.value_wei);
        }
        for index in 0..ZOOM_LEVELS.len() as i32 {
            let before = state_snapshot(&state);
            assert!(state.set_zoom_level(index));
            assert_unchanged_except(before, state_snapshot(&state), &["zoom", "zoom_index"]);
        }
        state.select_contract(0);
        state.open_deploy_doc();
        assert_eq!(state.selected_deployment_draft().values, bank.values);
        state.close_document(state.active_document);
        state.select_contract(0);
        state.open_deploy_doc();
        assert_eq!(state.selected_deployment_draft().value_wei, bank.value_wei);
        assert_eq!(state.deployment_drafts.len(), 2);
        assert!(state.jobs.is_empty());
    }

    #[test]
    fn constructor_edits_change_only_one_field_and_reject_invalid_targets() {
        let mut state = deployment_state();
        let before = state_snapshot(&state);
        assert!(!state.edit_constructor_argument(0, "ignored".into()));
        state.edit_deployment_value("ignored".into());
        assert_eq!(state_snapshot(&state), before);
        state.select_contract(0);
        for index in [-1, i32::MIN, 3, i32::MAX] {
            let before = state_snapshot(&state);
            assert!(!state.edit_constructor_argument(index, "ignored".into()));
            assert_eq!(state_snapshot(&state), before);
        }
        state.edit_deployment_value(" 900 ".into());
        let before = state_snapshot(&state);
        assert!(state.edit_constructor_argument(2, "  literal text with spaces  ".into()));
        assert_unchanged_except(before, state_snapshot(&state), &["deployment_drafts"]);
        let draft = state.selected_deployment_draft();
        assert_eq!(draft.values, vec!["", "", "  literal text with spaces  "]);
        assert_eq!(draft.value_wei, " 900 ");
        state.contracts.clear();
        let before = state_snapshot(&state);
        assert!(!state.edit_constructor_argument(0, "ignored".into()));
        state.edit_deployment_value("ignored".into());
        assert_eq!(state_snapshot(&state), before);
    }

    #[test]
    fn deployment_readiness_requires_valid_fields_value_key_code_and_idle_state() {
        let mut state = deployment_state();
        assert!(!state.deploy_busy);
        assert!(!state.deployment_can_submit());
        assert!(state.deployment_problem().contains("Select"));
        state.select_contract(0);
        assert!(!state.deployment_can_submit());
        assert!(state.deployment_problem().contains("owner"));
        assert!(
            state.edit_constructor_argument(0, "0x1111111111111111111111111111111111111111".into())
        );
        assert!(state.edit_constructor_argument(1, "42".into()));
        assert!(state.edit_constructor_argument(2, "Bank treasury".into()));
        assert!(!state.deployment_can_submit());
        for key in [None, Some(String::new()), Some(" \t ".into())] {
            state.wallet_key = key;
            assert!(!state.deployment_can_submit());
            assert!(state.deployment_problem().contains("wallet key"));
        }
        state.use_anvil_key(0);
        assert!(state.deployment_can_submit());
        assert!(state.deployment_problem().is_empty());
        assert!(state.edit_constructor_argument(1, "not a uint".into()));
        assert!(!state.deployment_can_submit());
        assert!(state.deployment_problem().contains("limit"));
        assert!(state.edit_constructor_argument(1, "42".into()));
        for value in ["-1", "1.5", "1 ether", "not wei"] {
            state.edit_deployment_value(value.into());
            assert!(!state.deployment_can_submit());
            assert!(state.deployment_problem().contains("wei"));
        }
        state.edit_deployment_value("123".into());
        assert!(state.deployment_can_submit());
        state.deploy_busy = true;
        assert!(!state.deployment_can_submit());
        assert!(state.deployment_problem().contains("progress"));
        state.deploy_busy = false;
        for code in ["", "0x", "0x0", "0xzz", "0x__unlinked__"] {
            state.contracts[0].bytecode = code.into();
            assert!(!state.deployment_can_submit());
            assert!(state.deployment_problem().contains("creation bytecode"));
        }
        state.contracts[0].bytecode = "0x6000".into();
        state.contracts[0].constructor_error = Some("invalid constructor inputs".into());
        assert!(!state.deployment_can_submit());
        assert!(state
            .deployment_problem()
            .contains("invalid constructor inputs"));
        state.contracts[0].constructor_error = None;
        assert!(state.deployment_can_submit());
        state.select_contract(1);
        assert!(state.edit_constructor_argument(0, "77".into()));
        state.edit_deployment_value("1".into());
        assert!(!state.deployment_can_submit());
        state.edit_deployment_value("0".into());
        assert!(state.deployment_can_submit());
        state.contracts.clear();
        assert!(!state.deployment_can_submit());
        assert!(state.deployment_problem().contains("compile output"));
        assert!(state.jobs.is_empty());
    }

    #[test]
    fn recompiled_constructor_validates_existing_draft_against_current_abi() {
        let mut state = deployment_state();
        state.select_contract(1);
        state.use_anvil_key(0);
        assert!(state.edit_constructor_argument(0, "77".into()));
        assert!(state.deployment_can_submit());
        let draft = state.selected_deployment_draft();
        state.contracts[1].constructor = ConstructorSpec::from_abi(&json!([{
            "type": "constructor", "stateMutability": "nonpayable", "inputs": [{"name": "owner", "type": "address"}]
        }])).unwrap();
        assert!(!state.deployment_can_submit());
        assert!(state.deployment_problem().contains("owner"));
        assert_eq!(state.selected_deployment_draft().values, draft.values);
        state.contracts[1].constructor = ConstructorSpec::from_abi(&json!([{
            "type": "constructor", "stateMutability": "nonpayable", "inputs": [
                {"name": "delay", "type": "uint256"}, {"name": "extra", "type": "uint256"}
            ]
        }]))
        .unwrap();
        assert!(!state.deployment_can_submit());
        assert!(!state.deployment_problem().is_empty());
        assert_eq!(state.selected_deployment_draft().values, draft.values);
    }

    #[test]
    fn explicit_reset_recovers_constructor_arity_changes_without_silent_draft_loss() {
        let mut state = deployment_state();
        state.select_contract(1);
        state.use_anvil_key(0);
        state.edit_constructor_argument(0, "77".into());
        state.edit_deployment_value("0".into());
        state.contracts[1].constructor = ConstructorSpec::from_abi(&json!([{
            "type":"constructor", "stateMutability":"nonpayable", "inputs":[
                {"name":"delay","type":"uint256"}, {"name":"extra","type":"uint256"}
            ]
        }]))
        .unwrap();
        assert_eq!(state.selected_deployment_draft().values, vec!["77"]);
        assert!(!state.deployment_can_submit());
        assert!(state.reset_constructor_draft());
        assert_eq!(state.selected_deployment_draft().values, vec!["", ""]);
        assert!(state.edit_constructor_argument(0, "7".into()));
        assert!(state.edit_constructor_argument(1, "8".into()));
        assert!(state.deployment_can_submit());
        state.contracts[1].constructor = ConstructorSpec::from_abi(&json!([{
            "type":"constructor", "stateMutability":"nonpayable", "inputs":[{"name":"delay","type":"uint256"}]
        }])).unwrap();
        assert_eq!(state.selected_deployment_draft().values, vec!["7", "8"]);
        assert!(!state.deployment_can_submit());
        assert!(state.reset_constructor_draft());
        assert!(state.edit_constructor_argument(0, "9".into()));
        assert!(state.deployment_can_submit());
        state.deploy_busy = true;
        assert!(!state.reset_constructor_draft());
        assert_eq!(state.selected_deployment_draft().values, vec!["9"]);
        assert_eq!(state.selected_deployment_draft().value_wei, "0");
    }

    #[test]
    fn invalid_constructor_abi_is_reported_and_no_constructor_is_valid() {
        for abi in [
            Value::Null,
            json!([{"type": "constructor", "inputs": [{"name": "bad", "type": "not_a_type"}]}]),
        ] {
            let raw = json!({"type": "compile_success", "contracts": [{"name": "Bad", "artifact": {
                "bytecode": {"object": "6000"}, "abi": abi
            }}]});
            let report = contracts_from_compile_report(&raw.to_string()).unwrap();
            assert!(report.contracts.is_empty());
            assert_eq!(report.skipped, 1);
            assert!(report.warning().unwrap().contains("1 artifact"));
            assert!(contracts_from_compile(&raw.to_string()).is_err());
        }
        let raw = json!({"type": "compile_success", "contracts": [{"name": "Simple", "artifact": {
            "bytecode": {"object": "6000"}, "abi": []
        }}]});
        let mut state = DesktopState::new(PathBuf::from("."), 1.0);
        state.contracts = contracts_from_compile(&raw.to_string()).unwrap();
        assert!(state.contracts[0].constructor.is_none());
        assert!(state.contracts[0].constructor_error.is_none());
        state.select_contract(0);
        assert!(state.selected_deployment_draft().values.is_empty());
        assert!(!state.edit_constructor_argument(0, "unused".into()));
        state.use_anvil_key(0);
        assert!(state.deployment_can_submit());
        let args = cast_deploy_args("0xkey", &[0x60, 0x00], "0").unwrap();
        let value_at = args.iter().position(|arg| arg == "--value").unwrap();
        let create_at = args.iter().position(|arg| arg == "--create").unwrap();
        assert!(value_at < create_at);
        assert_eq!(args[value_at + 1], "0");
        assert_eq!(args.last().map(String::as_str), Some("0x6000"));
        assert!(cast_deploy_args("0xkey", &[], "0").is_err());
    }

    #[test]
    fn activity_switch_keeps_documents_and_macros() {
        let mut state = DesktopState::new(PathBuf::from("."), 1.0);
        state.contracts.push(ContractItem {
            abi: Default::default(),
            name: "WorkshopVault".into(),
            kind: "src".into(),
            functions: vec![FunctionInfo {
                name: "withdraw".into(),
                inputs: "uint256 amount".into(),
                mutability: "nonpayable".into(),
                params: vec![("amount".into(), "uint256".into())],
            }],
            bytecode: "0x6000".into(),
            constructor_sig: String::new(),
            constructor: None,
            constructor_error: None,
        });
        state.select_contract(0);
        state.new_macro();
        state.open_activity(2);
        state.open_activity(0);
        assert_eq!(state.documents.len(), 3);
        assert_eq!(state.documents[0].title, "WorkshopVault");
        assert_eq!(state.documents[1].title, "Macros");
        assert_eq!(state.documents[2].title, "Converter");
        assert_eq!(state.macros.len(), 1);
        assert_eq!(state.active_document, 0);
    }

    #[test]
    fn deploy_and_wallet_docs_are_separate_tabs() {
        let mut state = DesktopState::new(PathBuf::from("."), 1.0);
        state.contracts.push(ContractItem {
            abi: Default::default(),
            name: "Bank".into(),
            kind: "src".into(),
            functions: Vec::new(),
            bytecode: "0x6000".into(),
            constructor_sig: "constructor(address)".into(),
            constructor: None,
            constructor_error: None,
        });
        state.select_contract(0);
        state.open_deploy_doc();
        assert_eq!(state.current_doc_kind(), 1);
        state.open_wallet_doc();
        assert_eq!(state.current_doc_kind(), 2);
        assert_eq!(state.documents.len(), 3);
        state.select_document(0);
        assert_eq!(state.current_doc_kind(), 0);
    }

    #[test]
    fn multiple_contract_tabs_reopen_without_duplicates() {
        let mut state = state_with_contracts();
        for index in 0..3 {
            state.select_contract(index);
        }
        state.select_contract(1);
        assert_eq!(state.documents.len(), 3);
        assert_eq!(state.active_document, 1);
        assert_eq!(state.selected_contract.as_deref(), Some("Bank"));
        assert!(state.documents.iter().all(|doc| doc.kind == 0));
    }

    #[test]
    fn closing_inactive_tabs_preserves_active_document_identity() {
        let mut state = populated_state();
        state.select_document(2);
        state.select_function(0);
        let before = state_snapshot(&state);
        state.close_document(0);
        assert_eq!(state.active_document, 1);
        assert_eq!(state.documents[1].title, "Bank");
        assert_eq!(state.selected_contract.as_deref(), Some("Bank"));
        assert_eq!(state.selected_function, 0);
        assert_unchanged_except(
            before,
            state_snapshot(&state),
            &["documents", "active_document"],
        );

        let before = state_snapshot(&state);
        state.close_document(3);
        assert_eq!(state.active_document, 1);
        assert_eq!(state.documents[1].title, "Bank");
        assert_unchanged_except(before, state_snapshot(&state), &["documents"]);
        for index in [-1, i32::MIN, 3, i32::MAX] {
            let before = state_snapshot(&state);
            state.close_document(index);
            assert_eq!(state_snapshot(&state), before);
        }
    }

    #[test]
    fn closing_active_tabs_selects_next_then_previous_contract() {
        let mut state = state_with_contracts();
        for index in 0..3 {
            state.select_contract(index);
        }
        state.select_document(0);
        state.select_function(0);
        state.close_document(0);
        assert_eq!(state.active_document, 0);
        assert_eq!(state.documents[0].title, "Bank");
        assert_eq!(state.selected_contract.as_deref(), Some("Bank"));
        assert_eq!(state.selected_function, -1);

        state.select_document(1);
        state.select_function(0);
        state.close_document(1);
        assert_eq!(state.active_document, 0);
        assert_eq!(state.documents[0].title, "Bank");
        assert_eq!(state.selected_contract.as_deref(), Some("Bank"));
        assert_eq!(state.selected_function, -1);
    }

    #[test]
    fn closing_active_contract_selects_deploy_tab_contract() {
        let mut state = state_with_contracts();
        state.select_contract(0);
        state.open_deploy_doc();
        state.select_contract(1);
        state.select_document(0);
        state.close_document(0);
        assert_eq!(state.active_document, 0);
        assert_eq!(state.current_doc_kind(), 1);
        assert_eq!(state.selected_contract.as_deref(), Some("Vault"));
        state.close_document(0);
        assert_eq!(state.current_doc_kind(), 0);
        assert_eq!(state.selected_contract.as_deref(), Some("Bank"));
    }

    #[test]
    fn closing_last_document_clears_only_document_selection() {
        for kind in 0..=3 {
            let mut state = populated_state();
            let title = match kind {
                0 => "Vault",
                1 => "Deploy · Vault",
                2 => "Wallet",
                _ => "Trace",
            };
            state.documents = vec![Document::new(title.into(), "Existing body".into(), kind)];
            state.active_document = 0;
            let before = state_snapshot(&state);
            state.close_document(0);
            assert!(state.documents.is_empty());
            assert_eq!(state.active_document, -1);
            assert_eq!(state.selected_contract, None);
            assert_eq!(state.selected_function, -1);
            assert_unchanged_except(
                before,
                state_snapshot(&state),
                &[
                    "documents",
                    "active_document",
                    "selected_contract",
                    "selected_function",
                ],
            );
        }
    }

    #[test]
    fn special_tabs_open_from_every_activity_without_contract_aliases() {
        let mut state = state_with_contracts();
        state.select_contract(0);
        for activity in 0..=4 {
            state.activity = activity;
            state.open_wallet_doc();
            assert_eq!(state.activity, 0);
            assert_eq!(state.current_doc_kind(), 2);
            assert_eq!(state.selected_contract.as_deref(), Some("Vault"));
            state.select_document(state.active_document);
            assert_eq!(state.selected_contract.as_deref(), Some("Vault"));

            state.activity = activity;
            state.open_trace_doc();
            assert_eq!(state.activity, 0);
            assert_eq!(state.current_doc_kind(), 3);
            state.select_document(state.active_document);
            assert_eq!(state.selected_contract.as_deref(), Some("Vault"));
        }
        assert_eq!(state.documents.len(), 3);
        state.select_document(0);
        state.close_document(0);
        assert_eq!(state.current_doc_kind(), 2);
        assert_eq!(state.selected_contract.as_deref(), Some("Vault"));
        state.close_document(0);
        assert_eq!(state.current_doc_kind(), 3);
        assert_eq!(state.selected_contract.as_deref(), Some("Vault"));
        state.close_document(0);
        assert_eq!(state.selected_contract, None);
        assert_eq!(state.selected_function, -1);

        state.open_trace_doc();
        state.open_wallet_doc();
        state.select_document(0);
        assert_eq!(state.selected_contract, None);
        state.close_document(0);
        assert_eq!(state.selected_contract, None);
    }

    #[test]
    fn contracts_named_wallet_and_trace_have_distinct_special_tabs() {
        let mut state = state_with_contracts();
        state.contracts[0].name = "Wallet".into();
        state.contracts[1].name = "Trace".into();
        state.select_contract(0);
        state.select_contract(1);
        state.open_wallet_doc();
        state.open_trace_doc();
        assert_eq!(state.documents.len(), 4);
        assert_eq!(
            state
                .documents
                .iter()
                .map(|doc| doc.kind)
                .collect::<Vec<_>>(),
            vec![0, 0, 2, 3]
        );
        state.select_document(0);
        assert_eq!(state.selected_contract.as_deref(), Some("Wallet"));
        state.select_document(3);
        assert_eq!(state.selected_contract.as_deref(), Some("Wallet"));
        state.select_contract(1);
        assert_eq!(state.active_document, 1);
        assert_eq!(state.documents.len(), 4);
    }

    #[test]
    fn deployment_selection_opens_distinct_stable_documents() {
        let mut state = populated_state();
        let original_bank = state.documents[2].id.clone();
        let bank_id = state.deployment_id(1).unwrap();
        assert!(state.select_deployment_by_id(&bank_id));
        assert_eq!(state.selected_contract.as_deref(), Some("Bank"));
        assert_eq!(state.address_input, state.deployments[1].address);
        assert_eq!(
            state.documents[state.active_document as usize].title,
            "Bank 2"
        );
        assert!(state.documents[2].contract_view.target.is_none());
        let bank_doc = state.current_document_id().unwrap();
        assert_ne!(bank_doc, original_bank);
        state.select_deployment(2);
        assert_eq!(state.selected_contract.as_deref(), Some("Vault"));
        assert_eq!(state.address_input, state.deployments[2].address);
        let vault_doc = state.current_document_id().unwrap();
        assert_eq!(
            state.documents[state.active_document as usize].title,
            "Vault 2"
        );
        assert!(state.select_deployment_by_id(&bank_id));
        assert_eq!(
            state.current_document_id().as_deref(),
            Some(bank_doc.as_str())
        );
        state.select_deployment(0);
        assert_eq!(
            state.documents[state.active_document as usize].title,
            "Vault 3"
        );
        assert_ne!(
            state.current_document_id().as_deref(),
            Some(vault_doc.as_str())
        );
        assert_eq!(state.deployments.len(), 3);
        for index in [-1, i32::MIN, 3, i32::MAX] {
            let before = state_snapshot(&state);
            state.select_deployment(index);
            assert_eq!(state_snapshot(&state), before);
        }
    }

    #[test]
    fn deployment_without_compile_output_still_opens_its_contract() {
        let mut state = state_with_contracts();
        state.deployments.push(Deployment {
            name: "PreviousBuild".into(),
            address: "0x7777777777777777777777777777777777777777".into(),
        });
        state.select_deployment(0);
        assert_eq!(state.selected_contract.as_deref(), Some("PreviousBuild"));
        assert_eq!(state.address_input, state.deployments[0].address);
        assert_eq!(state.active_document, 0);
        assert_eq!(state.documents[0].title, "PreviousBuild");
        assert_eq!(state.current_doc_kind(), 0);
        assert!(state.selected_contract_functions().is_empty());
    }

    #[test]
    fn zoom_levels_leave_live_session_forms_and_documents_unchanged() {
        let mut state = populated_state();
        let workspace = tempfile::tempdir().unwrap();
        state.workspace = workspace.path().to_path_buf();
        let before = state_snapshot(&state);
        for index in [0, 2, 3, 0, 1] {
            assert!(state.set_zoom_level(index));
            assert_eq!(state.zoom, ZOOM_LEVELS[index as usize]);
            assert_eq!(state.zoom_index, index);
            assert_unchanged_except(
                before.clone(),
                state_snapshot(&state),
                &["zoom", "zoom_index"],
            );
            assert_eq!(std::fs::read_dir(workspace.path()).unwrap().count(), 0);
        }
        for index in [-1, i32::MIN, 4, i32::MAX] {
            let before = state_snapshot(&state);
            assert!(!state.set_zoom_level(index));
            assert_eq!(state_snapshot(&state), before);
        }
    }

    #[test]
    fn zoom_save_load_roundtrip_preserves_other_settings() {
        let workspace = tempfile::tempdir().unwrap();
        assert_eq!(load_zoom(workspace.path()), 1.0);
        assert_eq!(std::fs::read_dir(workspace.path()).unwrap().count(), 0);
        save_zoom(workspace.path(), 1.25).unwrap();
        assert_eq!(load_zoom(workspace.path()), 1.25);
        let dir = workspace.path().join(".chasm");
        let path = dir.join("desktop-settings.json");
        let other = json!({"theme": "dark", "layout": {"dock": 240}, "recent": ["one", "two"], "custom": null});
        std::fs::write(&path, serde_json::to_vec_pretty(&other).unwrap()).unwrap();
        for zoom in ZOOM_LEVELS {
            save_zoom(workspace.path(), zoom).unwrap();
            assert_eq!(load_zoom(workspace.path()), zoom);
            let mut saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(
                saved
                    .as_object_mut()
                    .unwrap()
                    .remove("zoom")
                    .unwrap()
                    .as_f64()
                    .unwrap() as f32,
                zoom
            );
            assert_eq!(saved, other);
            assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
            let state =
                DesktopState::new(workspace.path().to_path_buf(), load_zoom(workspace.path()));
            assert_eq!(state.zoom, zoom);
            assert_eq!(state.zoom_index, zoom_index_for(zoom));
        }
    }

    #[cfg(windows)]
    #[test]
    fn zoom_save_replaces_existing_settings_with_open_reader_on_windows() {
        use std::io::Read as _;

        let workspace = tempfile::tempdir().unwrap();
        let dir = workspace.path().join(".chasm");
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("desktop-settings.json");
        std::fs::write(
            &path,
            br#"{"zoom":1.0,"theme":"dark","layout":{"dock":240}}"#,
        )
        .unwrap();
        for zoom in [1.0, 1.25, 1.5, 1.0, 1.1] {
            let mut reader = std::fs::File::open(&path).unwrap();
            let previous = std::fs::read(&path).unwrap();
            save_zoom(workspace.path(), zoom).unwrap_or_else(|error| {
                panic!("save_zoom failed with a Windows settings reader open: {error}");
            });
            let mut old_bytes = Vec::new();
            reader.read_to_end(&mut old_bytes).unwrap();
            assert_eq!(old_bytes, previous);
            assert_eq!(load_zoom(workspace.path()), zoom);
            let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(saved["theme"], "dark");
            assert_eq!(saved["layout"], json!({"dock": 240}));
            assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        }
    }

    #[cfg(windows)]
    #[test]
    fn zoom_save_preserves_settings_when_windows_reader_denies_delete_sharing() {
        use std::io::Read as _;
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};

        let workspace = tempfile::tempdir().unwrap();
        let dir = workspace.path().join(".chasm");
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("desktop-settings.json");
        let original: &[u8] = br#"{"zoom":1.0,"theme":"dark"}"#;
        std::fs::write(&path, original).unwrap();
        let mut reader = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(&path)
            .unwrap();
        assert!(save_zoom(workspace.path(), 1.25).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        let mut read_bytes = Vec::new();
        reader.read_to_end(&mut read_bytes).unwrap();
        assert_eq!(read_bytes, original);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        drop(reader);
        save_zoom(workspace.path(), 1.25).unwrap();
        assert_eq!(load_zoom(workspace.path()), 1.25);
        let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["theme"], "dark");
    }

    #[test]
    fn malformed_settings_are_not_clobbered() {
        let workspace = tempfile::tempdir().unwrap();
        let dir = workspace.path().join(".chasm");
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("desktop-settings.json");
        for text in [
            "{broken",
            "",
            "{\"zoom\":1.25,}",
            "[]",
            "null",
            "42",
            "\"settings\"",
        ] {
            std::fs::write(&path, text).unwrap();
            assert_eq!(load_zoom(workspace.path()), 1.0);
            assert!(save_zoom(workspace.path(), 1.5).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), text.as_bytes());
            assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        }
        std::fs::write(&path, [0xff, 0xfe]).unwrap();
        assert_eq!(load_zoom(workspace.path()), 1.0);
        assert!(save_zoom(workspace.path(), 1.5).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), vec![0xff, 0xfe]);
    }

    #[test]
    fn invalid_zoom_is_not_saved_and_invalid_stored_zoom_defaults() {
        let workspace = tempfile::tempdir().unwrap();
        for zoom in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.75, 1.2, 2.0] {
            assert!(save_zoom(workspace.path(), zoom).is_err());
            assert_eq!(std::fs::read_dir(workspace.path()).unwrap().count(), 0);
        }
        let dir = workspace.path().join(".chasm");
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("desktop-settings.json");
        let original: &[u8] = br#"{"zoom":1.25,"theme":"dark"}"#;
        std::fs::write(&path, original).unwrap();
        for zoom in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.75, 1.2, 2.0] {
            assert!(save_zoom(workspace.path(), zoom).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), original);
            assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        }
        for text in [
            r#"{"zoom":"1.25"}"#,
            r#"{"zoom":null}"#,
            r#"{"zoom":0.75}"#,
            r#"{"zoom":2.0}"#,
            r#"{"zoom":1e300}"#,
            "{}",
        ] {
            std::fs::write(&path, text).unwrap();
            assert_eq!(load_zoom(workspace.path()), 1.0);
            assert_eq!(std::fs::read(&path).unwrap(), text.as_bytes());
        }
        save_zoom(workspace.path(), 1.1).unwrap();
        assert_eq!(load_zoom(workspace.path()), 1.1);
    }

    #[test]
    fn zoom_settings_io_errors_do_not_replace_existing_target() {
        let workspace = tempfile::tempdir().unwrap();
        let path = workspace
            .path()
            .join(".chasm")
            .join("desktop-settings.json");
        std::fs::create_dir_all(&path).unwrap();
        let marker = path.join("keep");
        std::fs::write(&marker, "existing data").unwrap();
        assert_eq!(load_zoom(workspace.path()), 1.0);
        assert!(save_zoom(workspace.path(), 1.25).is_err());
        assert_eq!(std::fs::read_to_string(marker).unwrap(), "existing data");
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[test]
    fn compile_payload_keeps_function_names_and_bytecode() {
        let raw = r#"{"type":"compile_success","contracts":[{"name":"Vault","source_type":"src","artifact":{"bytecode":{"object":"6000"},"abi":[{"type":"function","name":"withdraw","inputs":[{"name":"amount","type":"uint256"}],"outputs":[],"stateMutability":"nonpayable"},{"type":"constructor","inputs":[]}]}}]}"#;
        let items = contracts_from_compile(raw).unwrap();
        assert_eq!(items[0].functions[0].name, "withdraw");
        assert_eq!(items[0].functions[0].inputs, "uint256 amount");
        assert_eq!(items[0].functions[0].mutability, "nonpayable");
        assert_eq!(items[0].bytecode, "0x6000");
    }

    #[test]
    fn tuple_overloads_render_distinguishable_signatures() {
        let raw = r#"{"type":"compile_success","contracts":[{"name":"Pairs","source_type":"src","artifact":{"abi":[
            {"type":"function","name":"f","inputs":[{"name":"a","type":"tuple","components":[{"name":"","type":"uint256"},{"name":"","type":"address"}]}],"outputs":[]},
            {"type":"function","name":"f","inputs":[{"name":"a","type":"tuple","components":[{"name":"","type":"address"},{"name":"","type":"uint256"}]}],"outputs":[]},
            {"type":"function","name":"g","inputs":[{"name":"a","type":"tuple[]","components":[{"name":"","type":"uint256"}]}],"outputs":[]}
        ]}}]}"#;
        let items = contracts_from_compile(raw).unwrap();
        let signatures: Vec<String> = items[0]
            .functions
            .iter()
            .map(|item| format!("{}({})", item.name, item.inputs))
            .collect();
        assert_eq!(signatures[0], "f((uint256,address) a)");
        assert_eq!(signatures[1], "f((address,uint256) a)");
        assert_ne!(signatures[0], signatures[1]);
        assert_eq!(signatures[2], "g((uint256)[] a)");
    }

    #[test]
    fn keystore_names_reject_paths_and_errors_hide_private_keys() {
        assert!(validate_keystore_name("local-dev").is_ok());
        assert!(validate_keystore_name("../wallet").is_err());
        assert!(validate_keystore_name("wallet.json").is_err());
        assert!(validate_keystore_name("").is_err());
        let leaked =
            "Private key: 0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
        let cleaned = redact_keystore_secret(leaked);
        assert!(!cleaned.contains("0xac0974"));
        assert!(!cleaned.to_ascii_lowercase().contains("private"));
        let workspace = tempfile::tempdir().unwrap();
        assert!(
            super::super::keystore::KeystoreStore::new(workspace.path().to_path_buf())
                .file_path("missing-keystore")
                .is_err()
        );
        assert!(address_from_private_key(
            "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
        )
        .unwrap()
        .eq_ignore_ascii_case("0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"));
        assert!(create_keystore("bad", "one", "two", "0x1234").is_err());
    }

    #[test]
    fn welcome_cancel_and_first_project_do_not_act_like_a_switch() {
        let mut state = DesktopState::welcome();
        assert!(state.startup_screen);
        assert!(!state.workspace_ready);
        assert!(state.workspace.as_os_str().is_empty());
        assert!(state.logs.is_empty());
        assert!(state.contracts.is_empty());
        assert!(state.can_change_workspace());
        assert!(!state.can_start_wallet_operation());
        state.folder_picker_busy = true;
        state.folder_picker_busy = false;
        assert!(state.startup_screen);
        assert!(state.next_workspace.is_none());
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().to_path_buf();
        assert!(state.accept_project_folder(path.clone()));
        assert!(state.pending_workspace.is_none());
        assert_eq!(state.next_workspace, Some(path.clone()));
        assert!(state.project_opening);
        assert!(!state.can_change_workspace());
        let mut ready = DesktopState::new(path.clone(), 1.0);
        assert!(!ready.accept_project_folder(path));
        assert!(ready.pending_workspace.is_none());
        let other = tempfile::tempdir().unwrap();
        assert!(!ready.accept_project_folder(other.path().to_path_buf()));
        assert_eq!(ready.pending_workspace.as_deref(), Some(other.path()));
        assert!(!ready.project_opening);
    }

    #[test]
    fn request_results_are_scoped_and_history_survives_selection_changes() {
        let mut state = state_with_contracts();
        state.select_contract(0);
        let first = state.begin_request(Operation::Call, "first", true);
        let second = state.begin_request(Operation::Call, "second", true);
        state.finish_request(&second, &Ok("new result".into()), None);
        assert!(state.accepts_request(&second));
        state.finish_request(&first, &Err("old error".into()), None);
        assert!(!state.accepts_request(&first));
        assert_eq!(
            state
                .jobs
                .iter()
                .find(|job| job.id == first.id)
                .unwrap()
                .status,
            "failed"
        );
        assert_eq!(
            state
                .jobs
                .iter()
                .find(|job| job.id == second.id)
                .unwrap()
                .output,
            "new result"
        );
        let submitted = state.begin_request(Operation::Call, "submitted", true);
        state.select_contract(1);
        state.finish_request(
            &submitted,
            &Ok("receipt".into()),
            Some("fixture hash".into()),
        );
        assert!(!state.accepts_request(&submitted));
        assert_eq!(
            state
                .jobs
                .iter()
                .find(|job| job.id == submitted.id)
                .unwrap()
                .transaction_hash
                .as_deref(),
            Some("fixture hash")
        );
        let old_abi_call = state.begin_request(Operation::Call, "old ABI call", true);
        state.replace_compiled_contracts(state.contracts.clone());
        assert!(!state.accepts_request(&old_abi_call));
        assert_eq!(state.selected_function, -1);
        state.finish_request(&old_abi_call, &Ok("old ABI result retained".into()), None);
        assert_eq!(
            state
                .jobs
                .iter()
                .find(|job| job.id == old_abi_call.id)
                .unwrap()
                .output,
            "old ABI result retained"
        );
        state.deployments.push(Deployment {
            name: "Bank".into(),
            address: "fixture-address".into(),
        });
        state.address_input = "fixture-address".into();
        let storage = state.begin_request(Operation::Storage, "read", true);
        assert!(state.remove_deployment(0));
        assert!(!state.accepts_request(&storage));
        let query = state.begin_request(Operation::Explorer, "lookup", false);
        state.rpc = "other fixture endpoint".into();
        assert!(!state.accepts_request(&query));
    }

    #[test]
    fn sidebar_preferences_and_converter_modes_preserve_exact_input() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(".chasm")).unwrap();
        let settings = root.path().join(".chasm").join("desktop-settings.json");
        std::fs::write(&settings, r#"{"zoom":1.25,"unknown":"preserve"}"#).unwrap();
        save_sidebar(root.path(), 320.0, true).unwrap();
        assert_eq!(load_sidebar(root.path()), (320.0, true));
        assert_eq!(load_zoom(root.path()), 1.25);
        assert_eq!(
            serde_json::from_str::<Value>(&std::fs::read_to_string(&settings).unwrap()).unwrap()
                ["unknown"],
            "preserve"
        );
        std::fs::write(&settings, "broken settings").unwrap();
        assert!(save_sidebar(root.path(), 300.0, false).is_err());
        assert_eq!(
            std::fs::read_to_string(&settings).unwrap(),
            "broken settings"
        );
        let mut state = DesktopState::new(root.path().to_path_buf(), 1.25);
        state.start_sidebar_drag(500.0);
        state.drag_sidebar(625.0);
        assert_eq!(state.sidebar_width, 350.0);
        state.set_converter_tool(0);
        state.set_converter_mode(2);
        state.convert(" x ");
        assert_eq!(state.converter_output, "0x207820");
        state.set_converter_tool(2);
        assert_eq!(state.converter_input, " x ");
        assert_eq!(
            state.converter_output,
            super::super::converter::convert(2, 0, " x ").unwrap()
        );
        let hash = state.converter_output.clone();
        state.new_macro();
        state.open_activity(2);
        assert_eq!(state.converter_tool, 2);
        assert_eq!(state.converter_output, hash);
        assert_eq!(state.converter_input, " x ");
        state.activity = 1;
        state.open_wallet_doc();
        state.activity = 2;
        state.select_document(state.active_document);
        assert_eq!(state.activity, 0);
    }

    #[test]
    fn wallet_delete_preserves_unrelated_signer_and_rejects_old_results() {
        let folder = tempfile::tempdir().unwrap();
        std::fs::write(folder.path().join("active.json"), "encrypted fixture").unwrap();
        let mut state = DesktopState::new(folder.path().to_path_buf(), 1.0);
        state.keystore_root = Some(folder.path().to_path_buf());
        state.refresh_keystores();
        state.wallet_source = Some("active.json".into());
        state.wallet_key = Some("not-a-real-key".into());
        state.wallet_address = "active-address".into();
        let generation = state.begin_wallet_operation().unwrap();
        assert!(!state.can_change_workspace());
        assert!(state.begin_wallet_operation().is_none());
        state.wallet_busy = false;
        state.finish_keystore_delete("other.json");
        assert_eq!(state.wallet_key.as_deref(), Some("not-a-real-key"));
        assert_eq!(state.wallet_address, "active-address");
        assert!(!state.accepts_wallet_result(generation));
        state.finish_keystore_delete("active.json");
        assert!(state.wallet_key.is_none());
        assert!(state.wallet_address.is_empty());
        state.use_anvil_key(1);
        let address = state.wallet_address.clone();
        state.finish_keystore_delete("active.json");
        assert_eq!(state.wallet_address, address);
        state.pending_workspace = Some(folder.path().join("other"));
        assert!(!state.can_start_wallet_operation());
        state.send_busy = true;
        assert!(!state.can_change_workspace());
    }

    #[test]
    fn anvil_accounts_sidebar_and_deployment_removal_are_bounded() {
        let mut state = DesktopState::new(PathBuf::from("."), 1.0);
        state.use_anvil_key(1);
        assert_eq!(
            state.wallet_address,
            "0x70997970C51812dc3A010C7d01b50e0d17dc79C8"
        );
        state.use_anvil_key(2);
        assert_eq!(
            state.wallet_address,
            "0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC"
        );
        state.use_anvil_key(3);
        assert!(state.notice.contains("first three"));
        state.deployments = vec![
            Deployment {
                name: "Bank".into(),
                address: "0x1111111111111111111111111111111111111111".into(),
            },
            Deployment {
                name: "Vault".into(),
                address: "0x2222222222222222222222222222222222222222".into(),
            },
        ];
        state.address_input = state.deployments[0].address.clone();
        assert!(state.remove_deployment(0));
        assert!(state.address_input.is_empty());
        assert_eq!(state.deployments.len(), 1);
        assert!(!state.remove_deployment(4));
        state.set_sidebar_width(20.0);
        assert_eq!(state.sidebar_width, 180.0);
        state.set_sidebar_width(900.0);
        assert_eq!(state.sidebar_width, 460.0);
        state.set_sidebar_width(f32::NAN);
        assert_eq!(state.sidebar_width, 460.0);
    }

    #[test]
    fn chain_queries_distinguish_address_hash_and_block() {
        assert_eq!(
            classify_chain_query(" 0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266 "),
            ChainQuery::Address("0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266".into())
        );
        assert_eq!(
            classify_chain_query(
                "0x1111111111111111111111111111111111111111111111111111111111111111"
            ),
            ChainQuery::Hash(
                "0x1111111111111111111111111111111111111111111111111111111111111111".into()
            )
        );
        assert_eq!(
            classify_chain_query("12"),
            ChainQuery::BlockNumber("12".into())
        );
        assert_eq!(classify_chain_query("0x12"), ChainQuery::Invalid);
        assert_eq!(classify_chain_query("latest"), ChainQuery::Invalid);
        assert_eq!(hex_quantity("0"), "0x0");
        assert_eq!(hex_quantity("12"), "0xc");
    }

    #[test]
    fn args_and_cast_output_parsing() {
        assert_eq!(split_args("1 0xabc"), vec!["1", "0xabc"]);
        assert_eq!(
            split_args("[1,2,3] 0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"),
            vec!["[1,2,3]", "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"]
        );
        let output = "transactionHash 0x1111111111111111111111111111111111111111111111111111111111111111\ncontractAddress 0x2222222222222222222222222222222222222222\n";
        assert_eq!(
            parse_field(output, "contractAddress").as_deref(),
            Some("0x2222222222222222222222222222222222222222")
        );
        assert_eq!(
            parse_field(output, "transactionHash").map(|v| v.len()),
            Some(66)
        );
    }
}
