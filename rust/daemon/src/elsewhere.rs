/*! Agent processes on this machine that Turtwig didn't start (super.engineering's, on the Mac): a stopped
chat one of them runs is reported as running elsewhere, not driven. */

use regex::Regex;
use std::collections::HashSet;
use std::time::Duration;

use crate::{agents, util};

/** Session ids some agent process here is running: a process named in its agent's `process_names` whose
arguments match `process_session_pattern` (group 1: the session). */
pub fn sessions_running() -> HashSet<String> {
    let matchers: Vec<(Vec<String>, Regex)> = agents::names()
        .iter()
        .filter_map(|a| {
            let procs: Vec<String> = agents::get(a, "process_names")?.as_array()?.iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
            let re = Regex::new(agents::get(a, "process_session_pattern")?.as_str()?).ok()?;
            Some((procs, re))
        })
        .collect();
    if matchers.is_empty() {
        return HashSet::new();
    }
    let o = util::run("ps", &["-axo", "args="], &[], None, Duration::from_secs(util::tmux_secs()));
    let mut out = HashSet::new();
    for l in o.stdout.lines() {
        let words: Vec<&str> = l.split_whitespace().take(2).collect();
        for (procs, re) in &matchers {
            if words.iter().any(|w| procs.iter().any(|p| w == p || w.ends_with(&format!("/{p}")))) {
                if let Some(c) = re.captures(l).and_then(|c| c.get(1)) {
                    out.insert(c.as_str().to_string());
                }
            }
        }
    }
    out
}
