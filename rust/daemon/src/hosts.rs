/*! This host's id (AGENT_BOX_HOST, ~/.agent-box/host, `hosts.*.detect`, else mac on macOS, box elsewhere)
and what bin/abxcfg.py resolves for it (`hosts`, `repos`, `logs`), re-read after each Configure. */

use regex::Regex;
use serde_json::{Map, Value};
use std::sync::{OnceLock, RwLock};
use std::time::Duration;

use crate::{config, util};

static RESOLVED: RwLock<Option<(String, Value)>> = RwLock::new(None);

fn started_as() -> Option<String> {
    static ENV: OnceLock<Option<String>> = OnceLock::new();
    ENV.get_or_init(|| std::env::var("AGENT_BOX_HOST").ok().filter(|h| !h.is_empty())).clone()
}

fn helper(host: Option<&str>) -> Value {
    let script = util::agent_box_dir().join("bin").join("abxcfg.py");
    let envs: Vec<(&str, &str)> = host.map(|h| vec![("AGENT_BOX_HOST", h)]).unwrap_or_default();
    let o = util::run("python3", &[&script.to_string_lossy(), "resolved"], &envs, None, Duration::from_secs(20));
    serde_json::from_str(&o.stdout).unwrap_or(Value::Null)
}

fn detects(rule: &Value) -> bool {
    let Some(rule) = rule.as_object().filter(|r| !r.is_empty()) else { return false };
    let s = |k: &str| rule.get(k).and_then(Value::as_str).filter(|v| !v.is_empty());
    if let Some(var) = s("env") {
        if std::env::var(var).map_or(true, |v| v.is_empty()) {
            return false;
        }
    }
    if let Some(os) = s("os") {
        let here = if cfg!(target_os = "macos") { "darwin" } else { std::env::consts::OS };
        if !here.starts_with(os) {
            return false;
        }
    }
    if let Some(pattern) = s("hostname") {
        let name = util::run("hostname", &[], &[], None, Duration::from_secs(5)).stdout;
        if !Regex::new(pattern).is_ok_and(|re| re.is_match(name.trim())) {
            return false;
        }
    }
    true
}

fn detect(resolved: &Value) -> String {
    if let Some(h) = started_as() {
        return h;
    }
    if let Ok(named) = std::fs::read_to_string(util::agent_home().join("host")) {
        if !named.trim().is_empty() {
            return named.trim().to_string();
        }
    }
    let hosts = resolved.get("hosts").and_then(Value::as_object).cloned().unwrap_or_default();
    for (id, h) in &hosts {
        if h.get("detect").is_some_and(detects) {
            return id.clone();
        }
    }
    if cfg!(target_os = "macos") { "mac".into() } else { "box".into() }
}

/** Resolve again (after the options change), and hand the id to every script this daemon runs. */
pub fn reload() {
    let first = helper(started_as().as_deref());
    let id = detect(&first);
    let resolved = if first.get("host_id").and_then(Value::as_str) == Some(id.as_str()) { first } else { helper(Some(&id)) };
    std::env::set_var("AGENT_BOX_HOST", &id);
    *RESOLVED.write().unwrap() = Some((id, resolved));
}

fn with<T>(f: impl FnOnce(&str, &Value) -> T) -> T {
    if RESOLVED.read().unwrap().is_none() {
        reload();
    }
    let guard = RESOLVED.read().unwrap();
    let (id, v) = guard.as_ref().expect("resolved");
    f(id, v)
}

pub fn here() -> String {
    with(|id, _| id.to_string())
}

/** Everything resolved (ConfigGet). */
pub fn resolved() -> Value {
    with(|_, v| v.clone())
}

/** This host's `hosts.HOST.KEY`. */
pub fn get(key: &str) -> Option<Value> {
    with(|_, v| v.pointer(&format!("/host/{key}")).cloned()).filter(|v| !v.is_null())
}

pub fn flag(key: &str, default: bool) -> bool {
    get(key).and_then(|v| v.as_bool()).unwrap_or(default)
}

pub fn label(id: &str) -> String {
    with(|_, v| v.pointer(&format!("/hosts/{id}/label")).and_then(Value::as_str).map(str::to_string)).unwrap_or_else(|| id.to_string())
}

/** How other machines reach this one over ssh ("" when they can't). */
pub fn ssh() -> String {
    if RESOLVED.read().unwrap().as_ref().is_some_and(|(_, v)| v.is_null()) {
        return if here() == "box" { config::text("ssh_host", "coder.tj-agents") } else { String::new() };
    }
    get("ssh").and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}

pub fn repos() -> Map<String, Value> {
    with(|_, v| v.get("repos").and_then(Value::as_object).cloned()).unwrap_or_default()
}

pub fn default_repo() -> String {
    with(|_, v| v.get("default_repo").and_then(Value::as_str).map(str::to_string)).unwrap_or_else(|| "redo".into())
}

/** The repo key for a checkout folder: a repo's key, one of its `names` or its primary's folder name; else the name. */
pub fn repo_for(folder: &str) -> String {
    for (key, r) in repos() {
        let names = r.get("names").and_then(Value::as_array).cloned().unwrap_or_default();
        let primary = r.get("primary").and_then(Value::as_str).and_then(|p| p.rsplit('/').find(|s| !s.is_empty()));
        if key == folder || names.iter().any(|n| n.as_str() == Some(folder)) || primary == Some(folder) {
            return key;
        }
    }
    folder.to_string()
}

/** A repo's `hue`, else one hashed from its key (as bin/abxcfg.py's). */
pub fn hue(repo: &str) -> f32 {
    if let Some(h) = repos().get(repo).and_then(|r| r.get("hue")).and_then(Value::as_f64) {
        return h as f32;
    }
    let hex = sha1_smol::Sha1::from(repo.as_bytes()).digest().to_string();
    (u64::from_str_radix(&hex[..8], 16).unwrap_or(0) % 360) as f32
}

/** A named log's path from `logs`. */
pub fn log_path(name: &str) -> Option<String> {
    with(|_, v| v.pointer(&format!("/logs/{name}")).and_then(Value::as_str).map(str::to_string))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn detect_rules_need_every_part() {
        assert!(!super::detects(&json!({})));
        assert!(!super::detects(&json!({"env": "ABX_SURELY_UNSET_VAR"})));
        assert!(super::detects(&json!({"env": "PATH"})));
        let os = if cfg!(target_os = "macos") { "darwin" } else { std::env::consts::OS };
        assert!(super::detects(&json!({"os": os, "env": "PATH"})));
        assert!(!super::detects(&json!({"os": "plan9"})));
    }

    #[test]
    fn an_unknown_repo_hashes_its_hue_as_the_scripts_do() {
        assert_eq!(super::hue("side-project"), 334.0);
    }
}
