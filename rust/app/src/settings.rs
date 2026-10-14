/*! Settings are the whole app: views, components, theme, state, keys, and commands, as JSON with
comments, deep-merged over the built-in defaults (which are themselves just settings). */

use anyhow::{Context as _, Result};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::path::PathBuf;

pub const DEFAULTS: &str = include_str!("../default-settings.jsonc");

#[derive(Debug, Clone, Deserialize)]
pub struct Settings {
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
    /** `badge`: a template for the Dock icon's badge (empty or null hides it). */
    #[serde(default)]
    pub dock: Value,
    /** Look of the built-in diff view. */
    #[serde(default)]
    pub diff: Value,
    pub terminal: TerminalSettings,
    /** Problems applying `patch` (unknown ids and the like), shown with other settings problems. */
    #[serde(skip)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Connection {
    pub ssh_host: String,
    pub daemon: String,
    pub retry_seconds: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TerminalSettings {
    pub font_family: String,
    pub font_size: f32,
    pub line_height: f32,
    pub padding: [f32; 2],
    pub foreground: String,
    pub background: String,
}

impl Default for Settings {
    fn default() -> Self {
        serde_json::from_value(defaults_value()).expect("built-in settings are complete")
    }
}

pub fn dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".agent-box/state/settings")
}

pub fn path() -> PathBuf {
    dir().join("settings.jsonc")
}

/** A defaults file read live instead of the built-in copy: `AGENT_BOX_DEFAULTS`, or the source checkout's
`default-settings.jsonc` when it exists. Editing it reloads the app like any settings change. */
pub fn live_defaults_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("AGENT_BOX_DEFAULTS") {
        return Some(PathBuf::from(p));
    }
    let p = PathBuf::from(std::env::var("HOME").unwrap_or_default()).join("dev/agent-box/rust/app/default-settings.jsonc");
    p.exists().then_some(p)
}

pub fn defaults_text() -> String {
    live_defaults_path().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_else(|| DEFAULTS.to_string())
}

pub fn defaults_value() -> Value {
    serde_json_lenient::from_str(&defaults_text()).or_else(|_| serde_json_lenient::from_str(DEFAULTS)).expect("built-in settings parse")
}

/** The user's file over the defaults. Objects merge key by key; `keys` merge by `key` and `commands`
by `label`; any other array replaces the default; `null` removes a default. */
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
    let mut s: Settings = serde_json::from_value(base).context("settings")?;
    s.warnings = warnings;
    Ok(s)
}

fn merge(base: &mut Value, over: Value, key: &str) {
    match (base, over) {
        (Value::Object(b), Value::Object(o)) => {
            for (k, v) in o {
                if k == "$schema" {
                    continue;
                }
                if v.is_null() {
                    b.remove(&k);
                    continue;
                }
                merge(b.entry(k.clone()).or_insert(Value::Null), v, &k);
            }
        }
        (Value::Array(b), Value::Array(o)) if key == "keys" || key == "commands" => {
            let id = if key == "keys" { "key" } else { "label" };
            for item in o {
                match b.iter_mut().find(|x| x.get(id).is_some() && x.get(id) == item.get(id)) {
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
