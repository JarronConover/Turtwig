/*! Options the app sends for this daemon (settings `daemon`, by the Configure call), read anywhere with
`num`, `flag`, `text`, `get`. They're also written to `~/.agent-box/config.json` for the scripts (and
read back at startup, so a restarted daemon keeps them before the app reconnects). */

use serde_json::Value;
use std::sync::RwLock;
use std::time::Duration;

static OPTIONS: RwLock<Option<Value>> = RwLock::new(None);

fn file() -> std::path::PathBuf {
    crate::util::agent_home().join("config.json")
}

pub fn set(options: Value) {
    let path = file();
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, serde_json::to_string_pretty(&options).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
    *OPTIONS.write().unwrap() = Some(options);
}

/** The options last saved to config.json, before the app sends any. */
pub fn load() {
    if let Some(v) = std::fs::read_to_string(file()).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()) {
        *OPTIONS.write().unwrap() = Some(v);
    }
}

/** Every option the app sent. */
pub fn all() -> Value {
    OPTIONS.read().unwrap().clone().unwrap_or(Value::Object(Default::default()))
}

pub fn get(key: &str) -> Option<Value> {
    OPTIONS.read().unwrap().as_ref().and_then(|o| o.get(key).cloned()).filter(|v| !v.is_null())
}

pub fn num(key: &str, default: f64) -> f64 {
    get(key).and_then(|v| v.as_f64()).unwrap_or(default)
}

pub fn count(key: &str, default: usize) -> usize {
    num(key, default as f64).max(0.) as usize
}

pub fn ms(key: &str, default: u64) -> Duration {
    Duration::from_millis(num(key, default as f64).max(0.) as u64)
}

pub fn secs(key: &str, default: u64) -> Duration {
    Duration::from_secs_f64(num(key, default as f64).max(0.))
}

pub fn flag(key: &str, default: bool) -> bool {
    get(key).and_then(|v| v.as_bool()).unwrap_or(default)
}

pub fn text(key: &str, default: &str) -> String {
    get(key).and_then(|v| v.as_str().map(str::to_string)).unwrap_or_else(|| default.to_string())
}
