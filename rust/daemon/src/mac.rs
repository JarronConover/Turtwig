/*! Mac mode: the same registry, shim and tmux code as the box, with host "mac". super.engineering
is only a source to import from (bin/import-se-mac); its running Claudes are reported, not driven. */

use regex::Regex;
use std::collections::HashSet;
use std::sync::OnceLock;
use std::time::Duration;

use crate::util;

pub fn is_mac() -> bool {
    match std::env::var("AGENT_BOX_HOST").as_deref() {
        Ok("mac") => true,
        Ok("box") => false,
        _ => cfg!(target_os = "macos"),
    }
}

pub fn host() -> &'static str {
    if is_mac() { "mac" } else { "box" }
}

/** Session ids that some Claude process on this machine is running (for a stopped chat: not a.box's). */
pub fn sessions_running() -> HashSet<String> {
    static SESSION: OnceLock<Regex> = OnceLock::new();
    let re = SESSION.get_or_init(|| Regex::new(r"--(?:resume|session-id)[ =]([0-9a-f]{8}-[0-9a-f-]{27})").unwrap());
    let o = util::run("ps", &["-axo", "args="], &[], None, Duration::from_secs(5));
    o.stdout
        .lines()
        .filter(|l| l.split_whitespace().take(2).any(|w| w.ends_with("/claude") || w == "claude"))
        .filter_map(|l| re.captures(l).map(|c| c[1].to_string()))
        .collect()
}
