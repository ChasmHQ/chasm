//! The next step for a notice: every error the banner shows names one action
//! that usually fixes it, so the user is never left with text alone.

/// Button label and the action id `run` understands, for a notice text.
pub fn suggest(notice: &str) -> Option<(&'static str, &'static str)> {
    let text = notice.to_ascii_lowercase();
    let rules: [(&[&str], &str, &str); 10] = [
        (&["missing libraries"], "Open compiler and libraries", "toolchain"),
        (&["compiler run failed", "compile failed", "parsererror", "typeerror"], "Recompile", "recompile"),
        (&["take a snapshot first", "snapshot is unknown", "already used"], "Open timeline", "timeline"),
        (&["unlock a local keystore", "choose an anvil development account", "select a keystore"], "Use Anvil account #0", "dev_account"),
        (&["the fork of", "fork rpc"], "Open network settings", "toolchain"),
        (&["select a contract document", "choose a deployment", "originating contract document"], "Open contracts", "contracts"),
        (&["deployment is missing or ambiguous", "earlier network context"], "Refresh chain", "refresh"),
        (&["wait for", "in progress"], "Open jobs", "jobs"),
        (&["debugger unavailable", "debugger gateway"], "Open debugger", "debugger"),
        (&["could not reach the node", "anvil did not become ready", "owned local anvil node is unavailable"], "Reopen project", "reopen"),
    ];
    rules
        .iter()
        .find(|(needles, _, _)| needles.iter().any(|needle| text.contains(needle)))
        .map(|(_, label, action)| (*label, *action))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_notices_get_a_next_step() {
        assert_eq!(suggest("Missing libraries: Solmate. Open Workspace…").map(|(_, action)| action), Some("toolchain"));
        assert_eq!(suggest("Compiler run failed:\nError (6275)").map(|(_, action)| action), Some("recompile"));
        assert_eq!(suggest("Take a snapshot first. Revert only returns…").map(|(_, action)| action), Some("timeline"));
        assert_eq!(suggest("Unlock a local keystore or choose an Anvil development account before Send.").map(|(_, action)| action), Some("dev_account"));
        assert_eq!(suggest("Removed the Bank deployment record."), None);
    }
}
