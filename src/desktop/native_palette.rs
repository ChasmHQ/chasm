//! Ctrl+K command palette: every entry is a target plus how to open it.
use super::session::DesktopState;
use super::{MainWindow, PaletteData, PaletteItem, PaletteLogic};
use slint::{ComponentHandle, Weak};
use std::sync::{Arc, Mutex};

type State = Arc<Mutex<DesktopState>>;

#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    /// Deployment index and function index.
    Function(i32, i32),
    Contract(i32),
    Transaction(String),
    Activity(i32),
    ExplorerTab(i32),
    Toolchain,
    Deploy,
    Recompile,
    Snapshot,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub title: String,
    pub detail: String,
    pub kind: &'static str,
    pub target: Target,
}

pub const LIMIT: usize = 60;

/// Everything the palette can open, in a stable order.
pub fn entries(state: &DesktopState) -> Vec<Entry> {
    let mut entries = Vec::new();
    for (index, deployment) in state.deployments.iter().enumerate() {
        let functions = state.contracts.iter().find(|contract| contract.name == deployment.name).map(|contract| contract.functions.clone()).unwrap_or_default();
        let short = format!("{}…{}", &deployment.address[..6.min(deployment.address.len())], &deployment.address[deployment.address.len().saturating_sub(4)..]);
        for (position, function) in functions.iter().enumerate() {
            entries.push(Entry {
                title: format!("{}.{}({})", deployment.name, function.name, function.params.iter().map(|(_, kind)| kind.as_str()).collect::<Vec<_>>().join(",")),
                detail: format!("{} at {short}", function.mutability),
                kind: "function",
                target: Target::Function(index as i32, position as i32),
            });
        }
    }
    let mut visible = 0;
    for contract in &state.contracts {
        if contract.kind != "src" {
            continue;
        }
        entries.push(Entry { title: contract.name.clone(), detail: format!("{} functions · open its tab", contract.functions.len()), kind: "contract", target: Target::Contract(visible) });
        visible += 1;
    }
    for row in state.recent_transactions.iter().take(20) {
        entries.push(Entry { title: row.title.clone(), detail: row.detail.clone(), kind: "tx", target: Target::Transaction(row.query.clone()) });
    }
    let actions: [(&str, &str, Target); 11] = [
        ("Deploy the selected contract", "Workspace", Target::Deploy),
        ("Recompile workspace", "Ctrl+B", Target::Recompile),
        ("Take a chain snapshot", "Timeline", Target::Snapshot),
        ("Timeline and cheats", "Explorer", Target::ExplorerTab(3)),
        ("History of calls", "Explorer", Target::ExplorerTab(2)),
        ("Chain explorer", "Explorer", Target::ExplorerTab(0)),
        ("Compiler, libraries and fork network", "Workspace", Target::Toolchain),
        ("Contracts", "Ctrl+1", Target::Activity(0)),
        ("Converter", "Ctrl+3", Target::Activity(2)),
        ("Macros", "Ctrl+4", Target::Activity(3)),
        ("Debugger", "Ctrl+5", Target::Activity(4)),
    ];
    for (title, detail, target) in actions {
        entries.push(Entry { title: title.into(), detail: detail.into(), kind: "action", target });
    }
    entries
}

/// Case-insensitive match of every word of the query, best first: a title
/// that starts with the query, then title matches, then detail matches.
pub fn filter(entries: Vec<Entry>, query: &str) -> Vec<Entry> {
    let query = query.trim().to_ascii_lowercase();
    if query.is_empty() {
        return entries.into_iter().take(LIMIT).collect();
    }
    // A pasted transaction hash is offered even when it is not in the list.
    let mut extra = Vec::new();
    if query.len() == 66 && query.starts_with("0x") && query[2..].bytes().all(|byte| byte.is_ascii_hexdigit()) {
        extra.push(Entry { title: format!("Debug transaction {}…{}", &query[..10], &query[60..]), detail: "open in the step debugger".into(), kind: "tx", target: Target::Transaction(query.clone()) });
    }
    let words: Vec<&str> = query.split_whitespace().collect();
    let mut scored: Vec<(u8, usize, Entry)> = entries
        .into_iter()
        .enumerate()
        .filter_map(|(order, entry)| {
            let title = entry.title.to_ascii_lowercase();
            let all = format!("{title} {} {}", entry.detail.to_ascii_lowercase(), entry.kind);
            if !words.iter().all(|word| all.contains(word)) {
                return None;
            }
            let score = if title.starts_with(&query) { 0 } else if words.iter().all(|word| title.contains(word)) { 1 } else { 2 };
            Some((score, order, entry))
        })
        .collect();
    scored.sort_by_key(|(score, order, _)| (*score, *order));
    extra.extend(scored.into_iter().map(|(_, _, entry)| entry));
    extra.truncate(LIMIT);
    extra
}

pub fn render(ui: &MainWindow, state: &DesktopState) {
    let data = ui.global::<PaletteData>();
    if !data.get_open() {
        return;
    }
    let shown = filter(entries(state), data.get_query().as_str());
    if data.get_selected() >= shown.len() as i32 {
        data.set_selected((shown.len() as i32 - 1).max(0));
    }
    data.set_items(super::model(shown.into_iter().map(|entry| PaletteItem { title: entry.title.into(), detail: entry.detail.into(), kind: entry.kind.into() }).collect()));
}

pub fn wire(ui: &MainWindow, state: &State, runtime: tokio::runtime::Handle) {
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<PaletteLogic>().on_query_edited(move |_| {
        if let Some(ui) = weak.upgrade() {
            render(&ui, &held.lock().unwrap());
        }
    });
    let weak = ui.as_weak();
    let held = state.clone();
    ui.global::<PaletteLogic>().on_run(move |index| {
        let Some(ui) = weak.upgrade() else { return };
        let data = ui.global::<PaletteData>();
        let target = {
            let state = held.lock().unwrap();
            filter(entries(&state), data.get_query().as_str()).into_iter().nth(index.max(0) as usize).map(|entry| entry.target)
        };
        let Some(target) = target else { return };
        data.set_open(false);
        data.set_query("".into());
        data.set_selected(0);
        run(&ui, &weak, &held, &runtime, target);
    });
}

pub fn run(ui: &MainWindow, weak: &Weak<MainWindow>, held: &State, runtime: &tokio::runtime::Handle, target: Target) {
    let logic = ui.global::<super::Logic>();
    let app = ui.global::<super::AppData>();
    match target {
        Target::Function(deployment, function) => logic.invoke_open_instance_function(deployment, function),
        Target::Contract(index) => {
            logic.invoke_open_activity(0);
            logic.invoke_select_contract(index);
        }
        Target::Transaction(hash) => {
            app.set_debugger_mode(0);
            ui.global::<super::TraceData>().set_input(hash.clone().into());
            super::native_trace::load(weak.clone(), held.clone(), runtime.clone(), hash);
        }
        Target::Activity(index) => logic.invoke_open_activity(index),
        Target::ExplorerTab(tab) => {
            app.set_explorer_tab(tab);
            logic.invoke_open_activity(1);
        }
        Target::Toolchain => logic.invoke_open_toolchain(),
        Target::Deploy => logic.invoke_open_deploy(),
        Target::Recompile => logic.invoke_recompile(),
        Target::Snapshot => ui.global::<super::ChainLogic>().invoke_snapshot(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(title: &str, detail: &str, kind: &'static str) -> Entry {
        Entry { title: title.into(), detail: detail.into(), kind, target: Target::Recompile }
    }

    #[test]
    fn every_word_must_match_and_title_prefixes_rank_first() {
        let entries = vec![
            entry("Bank.withdraw(uint256)", "nonpayable at 0x5fbd…0aa3", "function"),
            entry("Bank.deposit()", "payable at 0x5fbd…0aa3", "function"),
            entry("Deploy the selected contract", "Workspace", "action"),
            entry("Bank", "4 functions", "contract"),
        ];
        let titles = |query: &str| filter(entries.clone(), query).into_iter().map(|entry| entry.title).collect::<Vec<_>>();
        assert_eq!(titles("bank dep"), ["Bank.deposit()"]);
        assert_eq!(titles("bank")[0], "Bank.withdraw(uint256)");
        assert_eq!(titles("payable"), ["Bank.withdraw(uint256)", "Bank.deposit()"]);
        assert!(titles("zzz").is_empty());
        let hash = format!("0x{}", "ab".repeat(32));
        let found = filter(entries.clone(), &hash);
        assert_eq!(found[0].target, Target::Transaction(hash));
    }
}
