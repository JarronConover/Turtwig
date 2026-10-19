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

/** Several apps can configure one daemon (a Mac's and a test copy, or two people's on a shared box). Each
app's `own` options (the ones its user set) are kept by app, and the options are the latest defaults with
every app's own merged over them, the most recently configured last; so an app that sets nothing never
undoes what another set. Apps not heard from in `config_source_days` (30) drop out. */
pub fn configure(defaults: Value, own: Value, app: &str) {
    let path = crate::util::agent_home().join("config-sources.json");
    let mut sources: Value = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_else(|| serde_json::json!({}));
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let days = defaults.get("config_source_days").and_then(Value::as_u64).unwrap_or(30);
    let mut apps = sources.get("apps").and_then(Value::as_object).cloned().unwrap_or_default();
    apps.insert(app.to_string(), serde_json::json!({"own": own, "at": now}));
    apps.retain(|_, a| now.saturating_sub(a["at"].as_u64().unwrap_or(0)) <= days * 86_400);
    let options = combine(&defaults, &apps);
    sources = serde_json::json!({"defaults": defaults, "apps": apps});
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, serde_json::to_string_pretty(&sources).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
    set(options);
}

/** The defaults with each app's `own` merged over them, oldest `at` first. */
fn combine(defaults: &Value, apps: &serde_json::Map<String, Value>) -> Value {
    let mut order: Vec<&Value> = apps.values().collect();
    order.sort_by_key(|a| a["at"].as_u64().unwrap_or(0));
    let mut options = defaults.clone();
    for a in order {
        merge(&mut options, &a["own"]);
    }
    options
}

fn merge(base: &mut Value, over: &Value) {
    match (base, over) {
        (Value::Object(b), Value::Object(o)) => {
            for (k, v) in o {
                match b.get_mut(k) {
                    Some(slot) if slot.is_object() && v.is_object() => merge(slot, v),
                    _ => {
                        b.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (b, o) => *b = o.clone(),
    }
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn an_app_that_sets_nothing_keeps_what_another_app_set() {
        let defaults = json!({"move_leftovers": "hide", "state_repo_url": null, "agents": {"claude": {"bin": "claude"}}});
        let apps = json!({
            "mac": {"own": {"move_leftovers": "delete", "state_repo_url": "git@x:s.git", "agents": {"claude": {"model": "opus"}}}, "at": 10},
            "test": {"own": {}, "at": 20},
        });
        let out = super::combine(&defaults, apps.as_object().unwrap());
        assert_eq!(out["move_leftovers"], "delete");
        assert_eq!(out["state_repo_url"], "git@x:s.git");
        assert_eq!(out["agents"]["claude"], json!({"bin": "claude", "model": "opus"}));
        let apps = json!({"mac": {"own": {"move_leftovers": "delete"}, "at": 10}, "other": {"own": {"move_leftovers": "keep"}, "at": 30}});
        assert_eq!(super::combine(&defaults, apps.as_object().unwrap())["move_leftovers"], "keep");
    }
}
