/*! Settings are the whole app: views, components, theme, state, keys, and commands, as JSON with
comments, deep-merged over the built-in defaults (which are themselves just settings). */

use anyhow::{Context as _, Result};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::path::PathBuf;

pub const DEFAULTS: &str = include_str!("../default-settings.jsonc");

#[derive(Debug, Clone, Deserialize)]
pub struct Settings {
    /** The default host's connection; `local` (this Mac's daemon) and `hosts.<name>.connection` are read
    from `raw` (see `behavior::host_specs`). */
    pub connection: Connection,
    /** Named colors, fonts, and sizes; nodes refer to them as `@name`. */
    pub theme: Map<String, Value>,
    /** UI state and its starting values; actions change it, expressions read it as `.state`. */
    pub state: Map<String, Value>,
    /** Root views by name; `main` is the window. */
    pub views: Map<String, Value>,
    /** Reusable nodes, used as `{"type": "<Name>", "props": {...}}`. */
    pub components: Map<String, Value>,
    pub keys: Vec<Value>,
    /** Extra command palette entries: `{"label", "do", "if"}`. */
    pub commands: Vec<Value>,
    pub chat: Value,
    /** Layout glides, enter animations, and style easing (see SETTINGS.md, Motion). */
    #[serde(default)]
    pub motion: Value,
    /** Actions to run when something happens: chat.waiting, chat.finished, chat.stopped, designer.finished. */
    #[serde(default)]
    pub events: Map<String, Value>,
    /** Inline JavaScript by name (more in `scripts/NAME.js`); see SETTINGS.md, Scripts. */
    #[serde(default)]
    pub scripts: Map<String, Value>,
    /** `badge`: a template for the Dock icon's badge (empty or null hides it). */
    #[serde(default)]
    pub dock: Value,
    /** Where chats and worktrees run, by host (`box`, `mac`): `label`, `icon`, `color`. Read as `.hosts`. */
    #[serde(default)]
    pub hosts: Value,
    /** A sound per event (`chat.finished`, `chat.question`, `chat.waiting`, ...), plus `volume`, `when`, `enabled`. */
    #[serde(default)]
    pub sounds: Value,
    /** The designer agent: `add_dirs` (folders it may read besides the settings folder), `tools`, `permission_mode`. */
    #[serde(default)]
    pub designer: Value,
    /** The logo (art, colors, tile) drawn by `logo` nodes, the Dock, and the bundle icon; see SETTINGS.md, Logo. */
    #[serde(default)]
    pub logo: Value,
    /** Look of the built-in diff view. */
    #[serde(default)]
    pub diff: Value,
    pub terminal: TerminalSettings,
    /** Every other top-level section (window, strings, dialogs, palette, mouse, sidebar, daemon, inspect, ...),
    read with `Settings::get("section.key")`. */
    #[serde(flatten)]
    pub extra: Map<String, Value>,
    /** Problems applying `patch` (unknown ids and the like), shown with other settings problems. */
    #[serde(skip)]
    pub warnings: Vec<String>,
    /** The merged settings as JSON (after `patch`), for `.settings` and runtime patches from scripts. */
    #[serde(skip)]
    pub raw: Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Connection {
    pub ssh_host: String,
    /** Extra `ssh` arguments for the box connection. */
    #[serde(default = "default_ssh_options")]
    pub ssh_options: Vec<String>,
    /** How often to copy skills, CLAUDE.md and the like to the box (`bin/env-push`); 0 turns it off. */
    #[serde(default = "default_env_push")]
    pub env_push_seconds: u64,
}

fn default_ssh_options() -> Vec<String> {
    ["-o", "ServerAliveInterval=15", "-o", "ServerAliveCountMax=3"].map(String::from).to_vec()
}

fn default_env_push() -> u64 {
    60
}

#[derive(Debug, Clone, Deserialize)]
pub struct TerminalSettings {
    pub font_family: String,
    pub font_size: f32,
    pub line_height: f32,
    pub padding: [f32; 2],
    pub foreground: String,
    pub background: String,
    /** Key combos (`cmd-backspace`, `alt-left`, `shift-enter`) to the text they send; `{"paste": true}`
    pastes the clipboard; null leaves the combo to the app. Checked before the terminal's own keys. */
    #[serde(default)]
    pub keys: Map<String, Value>,
    /** Everything else under `terminal`: palette, cursor, size, scrollback, wheel, select, copy_key. */
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for Settings {
    fn default() -> Self {
        from_raw(defaults_value()).expect("built-in settings are complete")
    }
}

/** The settings folder: `AGENT_BOX_SETTINGS_DIR`, else ~/.agent-box/state/settings. */
pub fn dir() -> PathBuf {
    match std::env::var("AGENT_BOX_SETTINGS_DIR") {
        Ok(d) if !d.is_empty() => PathBuf::from(d),
        _ => PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".agent-box/state/settings"),
    }
}

pub fn path() -> PathBuf {
    dir().join("settings.jsonc")
}

/** a.box's source checkout: `AGENT_BOX_SOURCE`, else `app.source_dir` from the user's file or the
built-in defaults (read before the merged settings exist, since the live defaults live there). */
pub fn source_dir() -> PathBuf {
    if let Ok(p) = std::env::var("AGENT_BOX_SOURCE") {
        if !p.is_empty() {
            return PathBuf::from(crate::script::expand(&p));
        }
    }
    let from = |text: &str| serde_json_lenient::from_str::<Value>(text).ok().and_then(|v| v.pointer("/app/source_dir").and_then(Value::as_str).map(str::to_string));
    let user = std::fs::read_to_string(path()).ok().and_then(|t| from(&t));
    let dir = user.or_else(|| from(DEFAULTS)).unwrap_or_else(|| "~/dev/agent-box".into());
    PathBuf::from(crate::script::expand(&dir))
}

/** A defaults file read live instead of the built-in copy: `AGENT_BOX_DEFAULTS`, or the source checkout's
`default-settings.jsonc` when it exists. Editing it reloads the app like any settings change. */
pub fn live_defaults_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("AGENT_BOX_DEFAULTS") {
        return Some(PathBuf::from(p));
    }
    let p = source_dir().join("rust/app/default-settings.jsonc");
    p.exists().then_some(p)
}

pub fn defaults_text() -> String {
    live_defaults_path().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_else(|| DEFAULTS.to_string())
}

pub fn defaults_value() -> Value {
    serde_json_lenient::from_str(&defaults_text()).or_else(|_| serde_json_lenient::from_str(DEFAULTS)).expect("built-in settings parse")
}

/** The user's file over the defaults. Objects merge key by key; `keys` merge by `id`, else by `key` and
`if` together, and `commands` by `id`, else `label`; an event given as a list of handlers adds to the
default's (by `id`); any other array replaces the default; `null` removes a default. */
pub fn load() -> Result<Settings> {
    let mut base: Value = serde_json_lenient::from_str(&defaults_text()).context("parsing the default settings")?;
    let p = path();
    if p.exists() {
        let text = std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
        let user: Value = serde_json_lenient::from_str(&text).with_context(|| format!("parsing {}", p.display()))?;
        merge(&mut base, user, "");
    }
    let warnings = match base.as_object_mut().and_then(|o| o.remove("patch")) {
        Some(Value::Array(ops)) => crate::ui::patch::apply(&mut base, &ops),
        _ => vec![],
    };
    let mut s = from_raw(base)?;
    s.warnings = warnings;
    Ok(s)
}

/** Settings from already-merged JSON (`Settings::raw` with runtime changes on top). */
pub fn from_raw(raw: Value) -> Result<Settings> {
    let mut s: Settings = serde_json::from_value(raw.clone()).context("settings")?;
    s.raw = raw;
    Ok(s)
}

fn same_entry(list: &str, a: &Value, b: &Value) -> bool {
    if let (Some(x), Some(y)) = (a.get("id"), b.get("id")) {
        return x == y;
    }
    match list {
        "keys" => a.get("key").is_some() && a.get("key") == b.get("key") && a.get("if") == b.get("if"),
        "commands" => a.get("label").is_some() && a.get("label") == b.get("label"),
        _ => false,
    }
}

fn merge(base: &mut Value, over: Value, path: &str) {
    let key = path.rsplit('.').next().unwrap_or(path);
    let is_event = path.strip_prefix("events.").is_some_and(|e| !e.is_empty());
    match (base, over) {
        // An event's handler list adds to the default's; an object keeps merging into it as before.
        (b, Value::Array(o)) if is_event && !b.is_null() => {
            let mut list = match std::mem::take(b) {
                Value::Array(a) => a,
                one => vec![one],
            };
            for item in o {
                match list.iter_mut().find(|x| same_entry("", x, &item)) {
                    Some(existing) => *existing = item,
                    None => list.push(item),
                }
            }
            *b = Value::Array(list);
        }
        (Value::Object(b), Value::Object(o)) => {
            for (k, v) in o {
                if k == "$schema" {
                    continue;
                }
                if v.is_null() {
                    b.remove(&k);
                    continue;
                }
                let child = if path.is_empty() { k.clone() } else { format!("{path}.{k}") };
                merge(b.entry(k.clone()).or_insert(Value::Null), v, &child);
            }
        }
        (Value::Array(b), Value::Array(o)) if key == "keys" || key == "commands" => {
            for item in o {
                match b.iter_mut().find(|x| same_entry(key, x, &item)) {
                    Some(existing) => *existing = item,
                    None => b.push(item),
                }
            }
        }
        (b, o) => *b = o,
    }
}

pub fn parse_color(hex: &str) -> gpui_kit::Hsla {
    let h = hex.trim_start_matches('#');
    let v = u32::from_str_radix(h, 16).unwrap_or(0xff00ff);
    let (rgb, a) = if h.len() == 8 { (v >> 8, (v & 0xff) as f32 / 255.0) } else { (v, 1.0) };
    let mut c: gpui_kit::Hsla = gpui_kit::rgb(rgb).into();
    c.a = a;
    c
}

impl Settings {
    /** A value by dotted path into any section, e.g. `get("strings.connecting")` or `get("chat.labels.thinking")`. */
    pub fn get(&self, path: &str) -> Option<&Value> {
        let mut parts = path.split('.');
        let first = parts.next()?;
        let mut v = match first {
            "chat" => &self.chat,
            "motion" => &self.motion,
            "diff" => &self.diff,
            "designer" => &self.designer,
            "hosts" => &self.hosts,
            other => self.extra.get(other)?,
        };
        for p in parts {
            v = v.get(p)?;
        }
        Some(v)
    }

    /** A user-facing string from `strings`, else `default`. */
    pub fn text(&self, key: &str, default: &str) -> String {
        self.extra.get("strings").and_then(|s| s.get(key)).and_then(Value::as_str).unwrap_or(default).to_string()
    }

    /** A string by dotted path, else `default`. */
    pub fn text_at(&self, path: &str, default: &str) -> String {
        self.get(path).and_then(Value::as_str).unwrap_or(default).to_string()
    }

    /** A list of strings by dotted path, else `default`. */
    pub fn strings(&self, path: &str, default: &[&str]) -> Vec<String> {
        self.get(path).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_else(|| default.iter().map(|s| s.to_string()).collect())
    }

    /** A number by dotted path, else `default`. */
    pub fn num(&self, path: &str, default: f64) -> f64 {
        self.get(path).and_then(Value::as_f64).unwrap_or(default)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn keys_merge_by_key_and_if_and_events_add_handler_lists() {
        let mut base = json!({
            "keys": [{"key": "cmd-k", "do": "a"}, {"key": "cmd-w", "if": ".x", "do": "b"}],
            "commands": [{"label": "L", "do": "c"}],
            "events": {"chat.finished": {"if": ".app.focused | not", "do": "n"}, "chat.waiting": {"do": "w"}},
        });
        super::merge(&mut base, json!({
            "keys": [{"key": "cmd-k", "do": "a2"}, {"key": "cmd-w", "if": ".y", "do": "b2"}, {"id": "k", "key": "cmd-j", "do": "j"}],
            "commands": [{"label": "L", "do": "c2"}],
            "events": {"chat.finished": [{"id": "mine", "do": "m"}], "chat.waiting": {"do": "w2"}},
        }), "");
        assert_eq!(base["keys"].as_array().unwrap().len(), 4);
        assert_eq!(base["keys"][0]["do"], "a2");
        assert_eq!(base["keys"][1]["do"], "b");
        assert_eq!(base["commands"][0]["do"], "c2");
        assert_eq!(base["events"]["chat.finished"], json!([{"if": ".app.focused | not", "do": "n"}, {"id": "mine", "do": "m"}]));
        assert_eq!(base["events"]["chat.waiting"], json!({"do": "w2"}));
    }

    #[test]
    fn the_defaults_define_each_agent_the_daemon_launches() {
        let v: serde_json::Value = serde_json_lenient::from_str(super::DEFAULTS).expect("defaults parse");
        let agents = &v["daemon"]["agents"];
        assert_eq!(agents["claude"]["session"], "assigned");
        assert_eq!(agents["codex"]["transcripts"], "codex");
        assert_eq!(agents["codex"]["prompt_char"], "›");
        assert!(agents["codex"]["resume"].as_array().is_some_and(|a| a.iter().any(|x| x == "{session}")));
    }
}
