/*! User settings: the whole UI (layout, widgets, theme, keys, actions) is data in one JSON-with-comments
file, merged over the built-in defaults, validated by a generated JSON Schema, and reloaded on change. */

use anyhow::{Context as _, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const DEFAULTS: &str = include_str!("../default-settings.jsonc");

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    #[serde(rename = "$schema", skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /** Box to connect to: the ssh host and the daemon binary on it. */
    pub connection: Connection,
    pub theme: Theme,
    pub animation: Animation,
    /** The window, as a tree of containers and widgets. */
    pub layout: Node,
    pub sidebar: SidebarSettings,
    pub chat: ChatSettings,
    pub terminal: TerminalSettings,
    /** Key bindings: `{"key": "cmd-k", "action": "palette"}`. Later entries win. */
    pub keys: Vec<KeyBinding>,
    /** Extra commands for the palette and the keymap; `run` is a shell command, `{path}` and `{branch}` expand. */
    pub actions: Vec<CustomAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Connection {
    pub ssh_host: String,
    pub daemon: String,
    /** Seconds between reconnect attempts. */
    pub retry_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Theme {
    pub window: String,
    pub card: String,
    pub line: String,
    pub text: String,
    pub text_muted: String,
    pub text_faint: String,
    pub age: String,
    pub accent: String,
    pub hover: String,
    pub selection: String,
    pub selection_border: String,
    pub warning: String,
    pub danger: String,
    pub merged: String,
    pub success: String,
    pub font_family: String,
    pub mono_font_family: String,
    pub font_size: f32,
    pub card_radius: f32,
    pub row_radius: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Animation {
    pub enabled: bool,
    /** Milliseconds for hovers, selection, and collapse. */
    pub duration_ms: u64,
    /** Milliseconds for the sidebar slide. */
    pub sidebar_ms: u64,
    pub spinner_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Node {
    /** Children side by side. */
    Row { children: Vec<Node>, #[serde(default)] style: Style },
    /** Children stacked. */
    Column { children: Vec<Node>, #[serde(default)] style: Style },
    /** A bordered, rounded panel (super.engineering's cards). */
    Card { children: Vec<Node>, #[serde(default)] style: Style },
    Sidebar { #[serde(default)] style: Style },
    Toolbar { #[serde(default)] style: Style },
    ChatTabs { #[serde(default)] style: Style },
    Chat { #[serde(default)] style: Style },
    BranchBar { #[serde(default)] style: Style },
    TerminalTabs { #[serde(default)] style: Style },
    Terminal { #[serde(default)] style: Style },
    Spacer { #[serde(default)] style: Style },
}

/** Sizing and spacing any node can override. */
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Style {
    pub width: Option<f32>,
    pub height: Option<f32>,
    /** Share of the free space (like CSS flex-grow). */
    pub grow: Option<f32>,
    pub min_width: Option<f32>,
    pub min_height: Option<f32>,
    /** [top, right, bottom, left] */
    pub padding: Option<[f32; 4]>,
    pub gap: Option<f32>,
    pub background: Option<String>,
    /** Whether the user can drag this node's size; persisted per node `id`. */
    pub resizable: bool,
    pub id: Option<String>,
    pub hidden: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct SidebarSettings {
    pub workspace_title: String,
    pub show_projects: bool,
    pub show_sections: bool,
    pub show_empty_sections: bool,
    pub show_age: bool,
    pub show_status: bool,
    pub show_badges: bool,
    /** none | activity | name */
    pub sort: String,
    pub row_height: f32,
    pub project_row_height: f32,
    pub title_size: f32,
    pub label_size: f32,
    pub row_text_size: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ChatSettings {
    pub show_thinking: bool,
    pub collapse_tool_calls: bool,
    pub show_tool_results: bool,
    pub text_size: f32,
    pub max_width: Option<f32>,
    /** Enter sends and shift-enter adds a line, or the reverse. */
    pub enter_sends: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct TerminalSettings {
    pub font_size: f32,
    pub line_height: f32,
    pub padding: [f32; 2],
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KeyBinding {
    pub key: String,
    pub action: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CustomAction {
    pub name: String,
    pub run: String,
    #[serde(default)]
    pub key: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        serde_json_lenient::from_str(DEFAULTS).expect("built-in settings parse")
    }
}

macro_rules! defaults_from_settings {
    ($($t:ty => $field:ident),*) => {$(
        impl Default for $t {
            fn default() -> Self {
                let all: serde_json::Value = serde_json_lenient::from_str(DEFAULTS).expect("built-in settings parse");
                serde_json::from_value(all[stringify!($field)].clone()).expect("built-in settings section")
            }
        }
    )*};
}
defaults_from_settings!(Connection => connection, Theme => theme, Animation => animation,
    SidebarSettings => sidebar, ChatSettings => chat, TerminalSettings => terminal);

pub fn path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".agent-box/state/settings/settings.jsonc")
}

/** The user's file deep-merged over the defaults, so a file only needs what it changes. */
pub fn load() -> Result<Settings> {
    let mut base: serde_json::Value = serde_json_lenient::from_str(DEFAULTS)?;
    let p = path();
    if p.exists() {
        let text = std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
        let user: serde_json::Value =
            serde_json_lenient::from_str(&text).with_context(|| format!("parsing {}", p.display()))?;
        merge(&mut base, user);
    }
    serde_json::from_value(base).context("settings don't match the schema")
}

fn merge(base: &mut serde_json::Value, over: serde_json::Value) {
    match (base, over) {
        (serde_json::Value::Object(b), serde_json::Value::Object(o)) => {
            for (k, v) in o {
                merge(b.entry(k).or_insert(serde_json::Value::Null), v);
            }
        }
        (b, o) => *b = o,
    }
}

/** Writes the schema next to the settings file so editors can validate and autocomplete. */
pub fn write_schema() -> Result<()> {
    let p = path();
    std::fs::create_dir_all(p.parent().unwrap())?;
    let schema = schemars::schema_for!(Settings);
    std::fs::write(p.with_file_name("settings.schema.json"), serde_json::to_string_pretty(&schema)?)?;
    if !p.exists() {
        std::fs::write(&p, "// Agent Box settings. Anything here overrides the built-in defaults.\n// Schema: settings.schema.json\n{\n  \"$schema\": \"./settings.schema.json\"\n}\n")?;
    }
    Ok(())
}

pub fn parse_color(hex: &str) -> gpui_kit::Hsla {
    let h = hex.trim_start_matches('#');
    let v = u32::from_str_radix(h, 16).unwrap_or(0xff00ff);
    let (rgb, a) = if h.len() == 8 { (v >> 8, (v & 0xff) as f32 / 255.0) } else { (v, 1.0) };
    let mut c: gpui_kit::Hsla = gpui_kit::rgb(rgb).into();
    c.a = a;
    c
}
