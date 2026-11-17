//! Names for addresses the workspace knows: its deployments and the Anvil
//! development accounts. Views append the name after a full address, so
//! `0xf39F…2266` reads as `0xf39F…2266 (Anvil #0)`.
use super::session::DesktopState;
use std::collections::HashMap;

/// Lowercased address -> label. Deployments win over accounts, and repeated
/// deployments of one contract are numbered in deploy order (`Bank`, `Bank #2`).
pub fn labels(state: &DesktopState) -> HashMap<String, String> {
    let mut labels = HashMap::new();
    for index in 0..10 {
        if let Some((address, _)) = super::session::anvil_dev_account(index) {
            labels.insert(address.to_ascii_lowercase(), format!("Anvil #{index}"));
        }
    }
    if !state.wallet_address.is_empty() {
        let key = state.wallet_address.to_ascii_lowercase();
        let label = labels.get(&key).map(|label| format!("{label}, signer")).unwrap_or_else(|| "signer".into());
        labels.insert(key, label);
    }
    let mut seen: HashMap<&str, usize> = HashMap::new();
    for deployment in &state.deployments {
        let count = seen.entry(deployment.name.as_str()).or_insert(0);
        *count += 1;
        let label = if *count == 1 { deployment.name.clone() } else { format!("{} #{count}", deployment.name) };
        labels.insert(deployment.address.to_ascii_lowercase(), label);
    }
    labels
}

/// Append `(label)` after every full 20-byte address in `text` that has one.
/// Hashes and longer hex strings are left alone, and an address already
/// followed by ` (` is not labelled twice.
pub fn annotate(text: &str, labels: &HashMap<String, String>) -> String {
    if labels.is_empty() || !text.contains("0x") {
        return text.to_string();
    }
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len() + 32);
    let mut last = 0;
    let mut index = 0;
    while index + 42 <= bytes.len() {
        let starts = bytes[index] == b'0'
            && matches!(bytes[index + 1], b'x' | b'X')
            && (index == 0 || !bytes[index - 1].is_ascii_alphanumeric());
        if !starts {
            index += 1;
            continue;
        }
        let digits = bytes[index + 2..].iter().take_while(|byte| byte.is_ascii_hexdigit()).count();
        let end = index + 2 + digits;
        if digits == 40 && !text[end..].starts_with(" (") {
            if let Some(label) = labels.get(&text[index..end].to_ascii_lowercase()) {
                out.push_str(&text[last..end]);
                out.push_str(" (");
                out.push_str(label);
                out.push(')');
                last = end;
            }
        }
        index = end.max(index + 1);
    }
    out.push_str(&text[last..]);
    out
}

/// Workspace function selectors (0x + 8 hex) -> `name(types)`. A selector
/// shared by functions of different names is left out rather than guessed.
pub fn selectors(state: &DesktopState) -> HashMap<String, String> {
    use ethers::abi::FunctionExt;
    let mut names: HashMap<String, Option<String>> = HashMap::new();
    for contract in &state.contracts {
        for function in contract.abi.functions() {
            let selector = format!("0x{}", ethers::utils::hex::encode(function.selector()));
            let signature = function.abi_signature();
            names
                .entry(selector)
                .and_modify(|known| {
                    if known.as_deref() != Some(signature.as_str()) {
                        *known = None;
                    }
                })
                .or_insert(Some(signature));
        }
    }
    names.into_iter().filter_map(|(selector, name)| Some((selector, name?))).collect()
}

/// Transaction row detail with the recipient's label and the function name.
pub fn transaction_detail(row: &super::session::ChainRow, labels: &HashMap<String, String>, selectors: &HashMap<String, String>) -> String {
    let mut detail = row.detail.clone();
    if let Some(label) = row.to.as_ref().and_then(|to| labels.get(&to.to_ascii_lowercase())) {
        if let Some(at) = detail.find("  to ") {
            let end = detail[at + 5..].find("  ").map(|offset| at + 5 + offset).unwrap_or(detail.len());
            detail.insert_str(end, &format!(" ({label})"));
        }
    }
    if let Some(name) = row.selector.as_ref().and_then(|selector| selectors.get(selector)) {
        if let Some(selector) = &row.selector {
            detail = detail.replace(selector.as_str(), name);
        }
    }
    detail
}

/// Label for a 32-byte stack or storage word that holds a known address.
pub fn word_label<'a>(word: &str, labels: &'a HashMap<String, String>) -> Option<&'a String> {
    let digits = word.trim_start_matches("0x").trim_start_matches('0');
    if digits.len() < 30 || digits.len() > 40 {
        return None;
    }
    labels.get(&format!("0x{digits:0>40}").to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::session::Deployment;

    #[test]
    fn deployments_and_accounts_get_names_and_text_is_annotated_once() {
        let mut state = DesktopState::new(std::path::PathBuf::from("."), 1.0);
        let (account, _) = super::super::session::anvil_dev_account(0).unwrap();
        state.wallet_address = account.into();
        state.deployments = vec![
            Deployment { name: "Bank".into(), address: "0x5FbDB2315678afecb367f032d93F642f64180aa3".into() },
            Deployment { name: "Bank".into(), address: "0xe7f1725E7734CE288F8367e1Bb143E90bb3F0512".into() },
        ];
        let labels = labels(&state);
        assert_eq!(labels[&account.to_ascii_lowercase()], "Anvil #0, signer");
        assert_eq!(labels["0xe7f1725e7734ce288f8367e1bb143e90bb3f0512"], "Bank #2");
        let text = format!("From {account}\nTo 0x5FbDB2315678afecb367f032d93F642f64180aa3\nHash 0x{}", "ab".repeat(32));
        let shown = annotate(&text, &labels);
        assert!(shown.contains(&format!("From {account} (Anvil #0, signer)")));
        assert!(shown.contains("0x5FbDB2315678afecb367f032d93F642f64180aa3 (Bank)"));
        assert!(shown.ends_with(&"ab".repeat(32)), "a 32-byte hash is not an address");
        assert_eq!(annotate(&shown, &labels), shown, "annotating twice changes nothing");
        assert_eq!(word_label("0x5fbdb2315678afecb367f032d93f642f64180aa3", &labels).map(String::as_str), Some("Bank"));
        assert_eq!(word_label("0x2a", &labels), None);
        let row = super::super::session::ChainRow {
            title: "0x68…".into(),
            detail: "#2  2 ETH  to 0x5fbdb2…180aa3  0xd0e30db0".into(),
            query: String::new(),
            to: Some("0x5FbDB2315678afecb367f032d93F642f64180aa3".into()),
            selector: Some("0xd0e30db0".into()),
        };
        let selectors = HashMap::from([("0xd0e30db0".to_string(), "deposit()".to_string())]);
        assert_eq!(transaction_detail(&row, &labels, &selectors), "#2  2 ETH  to 0x5fbdb2…180aa3 (Bank)  deposit()");
    }
}
