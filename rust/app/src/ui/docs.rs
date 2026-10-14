/*! Writes SETTINGS.md (the reference an agent reads before changing the app) and settings.schema.json
next to the settings file, from the same tables the interpreter uses, so they never drift. */

use super::actions::ACTIONS;
use super::render::NODE_TYPES;
use super::style::STYLE_KEYS;
use crate::settings;
use serde_json::{json, Value};

const GUIDE: &str = r##"# Agent Box settings reference

Agent Box is a container plus data: **everything you see and every button's behavior is defined in
settings**. The built-in look (super.engineering's) is itself just the default settings, printed in
full at the end of this file. Change anything by overriding it in your settings file.

- Your file: `~/.agent-box/state/settings/settings.jsonc` (JSON with comments). Edit it in the app
  (⌘, or the gear) or with any editor; the app reloads on save.
- It is merged over the defaults: objects merge key by key, `null` removes a default key, other arrays
  replace the default, except `keys` (merged by `key`) and `commands` (merged by `label`).
- Problems (bad JSON, failing expressions, unknown actions) show at the top of the settings panel and
  are written to `problems.txt` next to the settings file (it says `No problems.` when all is well).
  Agents: read it after each save.
- The designer (the palette button at the top left, ⌘⇧D) is a chat with an agent that edits this file
  while you watch. Each request first snapshots the file into `history/`; `settings.undo` restores.
- Scripts and agents can drive the app: write an action (or a list of actions) to `run.json` next to
  the settings file; the app runs it once and deletes the file.
- UI state (selection, toggles, sizes) persists in `ui-state.json` next to the settings file.

## Top-level keys

| key | what it is |
| --- | --- |
| `patch` | Edits to default nodes by `id` (move, set, insert, replace, remove). **Prefer this to copying a component**: a copy freezes it, so later improvements to the defaults stop reaching you. See Patch. |
| `views` | Root nodes by name. `views.main` is the window. |
| `components` | Reusable nodes; use one as `{"type": "Name", "props": {...}}` and read props inside as `.props.x`. |
| `theme` | Named values referenced as `@name` in styles (colors, fonts) and readable as `.theme.name`. `animations: false` turns animation off. |
| `state` | UI state and starting values. Read as `.state.key`; change with `state.set` / `state.toggle` / `state.cycle`. |
| `keys` | `[{"key": "cmd-k", "do": <action>, "if": <jq>}]`. Key names: `cmd`, `ctrl`, `alt`, `shift` + `-` + key (`cmd-shift-p`, `ctrl-\``, `cmd-\\`). |
| `commands` | Command palette entries: `[{"label", "do", "if"}]`. Worktrees are added automatically. |
| `motion` | How things move: `move` (glide to new positions), `enter` (how new nodes come in), `style` (colors and sizes ease), `enabled`. See Motion. |
| `chat` | Look of the built-in transcript, composer, and waiting card: style objects per part. |
| `terminal` | Terminal font, size, padding, colors. |
| `connection` | The box: `ssh_host`, `daemon` path, `retry_seconds`. |

## Patch

Ops run in order after merging, each naming a node by its `id` anywhere in `views` or `components`:

```jsonc
"patch": [
  { "id": "designer-button", "move": { "after": "sidebar-drag" } },     // also "before": id, or "into": id with "at": n (negative from the end)
  { "id": "sidebar-header", "set": { "style": { "padding": [0, 12, 0, 88] } } },  // merged into the node; null removes a key
  { "id": "chat-progress", "replace": { "type": "row", "children": ["Working…"] } },
  { "id": "status-pill", "remove": true },
  { "insert": { "type": "Pill", "props": { "text": "Docs", "do": "settings.docs" } }, "after": "status-pill" }
]
```

Nodes with ids in the defaults include `sidebar`, `sidebar-header`, `sidebar-toggle`, `designer-button`,
`sidebar-drag`, `sidebar-list`, `sidebar-footer`, `main-area`, `toolbar`, `toolbar-drag`, `status-pill`,
`chat-card`, `transcript`, `chat-progress`, `waiting`, `composer-box`, `branch-bar`, `changes-pill`,
`terminal-card`, `tab-terminal`, `tab-changes`, and `designer` (search the defaults below for `"id"`).
Unknown ids are reported in `problems.txt`.

## Nodes

A node is an object (or a string, shorthand for a text node, or an array of nodes):

```jsonc
{
  "type": "row",                      // see the node types below; default "box"
  "id": "my-row",                     // optional stable identity (needed for animation/transition state)
  "if": ".state.sidebar_open",        // jq; the node renders only when truthy
  "for": ".worktrees",                // jq returning an array; the node repeats for each item...
  "as": "w",                          // ...with the item available as .w (default .item) and .index
  "key": ".w.path",                   // jq; identity per repeated item
  "props": { "text": "{{ .w.title }}" },
  "style": { "padding": [0, 12], "background": "@card", "hover": { "background": "@hover" } },
  "on": { "click": {"do": "select", "worktree": "{{ .w.path }}"}, "double_click": "window.zoom" },
  "menu": [ {"label": "Rename", "do": {"do": "section.rename", "id": "{{ .s.id }}"}}, {"separator": true} ],
  "tooltip": "{{ .w.branch }}",
  "animate": { "from": {"opacity": 0, "y": 6}, "duration": 160 },   // plays when the node first appears
  "transition": { "width": 200 },     // these style keys (numbers or colors) ease to their new value (ms)
  "motion": false,                    // opt this node out of gliding, enter animation, and style easing
  "children": [ ... ]
}
```

Any string anywhere in `props`, `style`, `on`, `menu`, or `tooltip` may contain `{{ jq }}` templates.
A string that is exactly one template yields the raw value (a number, bool, object), so
`"width": "{{ .state.sidebar_width }}"` is a number.

## Motion

Changes animate instead of jumping:

- **Glide (`motion.move`):** when a change (a settings save, an action, new data) moves a node, it
  slides from its old position to its new one: `{"duration": 320, "easing": "spring" | "ease_out" | "linear"}`.
  Identity is the node's `id`, or its position in the tree when it has none. **Give a node an `id`
  and keep it when moving the node somewhere else**, so it glides there instead of disappearing and
  reappearing. Inside a `for`, ids are made unique per item automatically. An `id` on a component
  instance (`{"type": "Sidebar", "id": "sidebar"}`) applies to the component.
- **Enter (`motion.enter`):** nodes that appear because of a change come in with
  `{"from": {"opacity": 0, "x": 0, "y": 8}, "duration": 260}`. A node's own `animate` overrides this
  and plays every time the node appears, not only after changes.
- **Style (`motion.style`):** `{"duration": 180}` eases colors, opacity, radius, sizes, gaps, and
  offsets whenever they change (a theme edit, hover-free state changes). `transition` on a node sets
  per-key durations.
- Scrolling, window resizing, and dragging a resize handle never animate.
- `"motion": {"enabled": false}` turns everything off; `"motion": false` on a node opts it out.

## Expressions (jq)

Expressions are [jq](https://jqlang.org/manual/) evaluated against the context below (via jaq, so
standard jq functions work: `map`, `select`, `length`, `any`, `sort_by`, `test`, `ascii_downcase`,
`if ... then ... else ... end`, `//`, string interpolation `"\(.x)"`).

Examples:
- `.selected.worktree.branch // "detached"`
- `.worktrees | map(select(.status == "waiting")) | length`
- `if .w.selected then "@selection" else "transparent" end`
- `.sections | map(select(.items | length > 0))`
- `.selected.chats | map(select(.state == "working")) | length > 0`

### Context

| path | contents |
| --- | --- |
| `.projects[]` | `{name, repo, hue, letter, items: [worktree]}`: repos with their worktrees outside any section |
| `.sections[]` | `{id, name, collapsed, items: [worktree]}` |
| `.worktrees[]` | every worktree: `{id, path, branch, title, emoji, label, project, repo, hue, letter, is_primary, uncommitted, unpushed, behind, mr: {number, url, state, checks} \| null, age, age_text, status, section: {id, name} \| null, selected, chats: [chat]}`. `status` is one of working, waiting, merged, failing, pending, dirty, behind, idle. |
| `.chats[]` | every chat: `{id, name, kind, host, status, dir, branch, rc_url, alive, state, waiting_on, last_activity, age_text, active, tmux}`. `state` is idle, working, waiting, or stopped; `host` is box or mac; `activity` is a short line on what a working agent is doing now ("Reading settings.jsonc", "Thinking", "Writing"), empty otherwise (filled for chats whose transcript is open). |
| `.designer` | `{working, activity, has_messages}` for the designer chat |
| `.selected.worktree` | the selected worktree (or null) |
| `.selected.chat` | its active chat (or null) |
| `.selected.chats` | all chats in the selected worktree |
| `.selected.diff` | the selected worktree's changes for `state.diff_base`: `{loaded, loading, error, base, base_label, files, added, removed, truncated}` |
| `.state` | UI state (see `state` in the defaults) |
| `.theme` | theme values |
| `.app` | `{connected, status, ssh_host, settings_error, errors}` |
| `.now` | unix seconds |
| `.props`, `.<as>`, `.index`, `.input` | inside components, `for` loops, and a `prompt`'s `then` |
"##;

const RECIPES: &str = r##"
## Recipes

**Move the terminal beside the chat:** override `components.MainArea` so the chat card and the
terminal card share a `row`; keep their ids (`chat-card`, `terminal-card`) so they glide into place.

**Hide the terminal by default:** `{"state": {"terminal_open": false}}`

**Move the sidebar to the right:** override `views.main` and put the `Sidebar` component after the
main column in the row's `children`.

**Worktrees as cards across the top instead of a sidebar:**
```jsonc
{
  "state": { "sidebar_open": false },
  "views": { "main": { "type": "column", "style": {"width": "full", "height": "full"}, "children": [
    { "type": "row", "style": {"padding": [52, 14, 10, 90], "gap": 8, "wrap": true}, "children": [
      { "for": ".worktrees", "as": "w", "key": ".w.path", "type": "column",
        "style": {"width": 220, "padding": 10, "radius": 10, "border": 1, "border_color": "@line",
                  "background": "{{ if .w.selected then \"@selection\" else \"@card\" end }}",
                  "hover": {"border_color": "@accent"}, "cursor": "pointer"},
        "on": {"click": {"do": "select", "worktree": "{{ .w.path }}"}},
        "animate": {"from": {"opacity": 0, "y": 8}, "duration": 180},
        "children": [
          {"type": "text", "props": {"text": "{{ .w.title }}"}, "style": {"font_weight": "semibold", "truncate": true}},
          {"type": "text", "props": {"text": "{{ .w.status }} · {{ .w.age_text }}"}, "style": {"color": "@text_muted", "font_size": 12}}
        ] }
    ] },
    { "type": "MainArea", "style": {"grow": 1} }
  ] } }
}
```

**A button that runs something:** anywhere in a view:
```jsonc
{ "type": "row", "style": {"padding": [4, 10], "radius": 6, "background": "@card", "hover": {"background": "@hover"}, "cursor": "pointer"},
  "on": {"click": {"do": "shell", "run": "open -a Cursor"}},
  "children": [{"type": "icon", "props": {"name": "code"}}, "Open Cursor"] }
```

**A key that sends a message to the current chat:** `{"keys": [{"key": "cmd-shift-c", "do": {"do": "chat.send", "text": "Commit and push."}}]}`

**Ask for text, then use it:** `{"do": "prompt", "title": "Branch name", "then": {"do": "chat.new", "kind": "feature", "branch": "{{ .input }}", "repo": "redo"}}`

**Change colors:** override `theme` keys (`{"theme": {"accent": "#ff7a59"}}`); every default node uses `@` tokens.
"##;

pub fn write_reference() {
    let dir = settings::dir();
    let _ = std::fs::create_dir_all(&dir);
    let defaults = settings::defaults_value();
    let mut md = String::from(GUIDE);
    md.push_str("\n## Node types\n\n| type | what it is |\n| --- | --- |\n");
    for (t, d) in NODE_TYPES {
        md.push_str(&format!("| `{t}` | {d} |\n"));
    }
    md.push_str("\n## Style keys\n\n| key | value |\n| --- | --- |\n");
    for (k, d) in STYLE_KEYS {
        md.push_str(&format!("| `{k}` | {d} |\n"));
    }
    md.push_str("\n## Actions\n\nUsed by `on`, `menu`, `keys`, and `commands`. An action is a name (`\"palette.open\"`), an object\n`{\"do\": \"name\", ...args}`, or a list of actions run in order.\n\n| action | arguments |\n| --- | --- |\n");
    for (a, d) in ACTIONS {
        md.push_str(&format!("| `{a}` | {d} |\n"));
    }
    md.push_str("\n## Theme tokens in the defaults\n\n");
    if let Some(Value::Object(t)) = defaults.get("theme") {
        for (k, v) in t {
            md.push_str(&format!("- `@{k}`: `{v}`\n"));
        }
    }
    md.push_str("\n## Components in the defaults\n\n");
    if let Some(Value::Object(c)) = defaults.get("components") {
        for (k, v) in c {
            let about = v.get("about").and_then(Value::as_str).unwrap_or("");
            md.push_str(&format!("- `{k}`: {about}\n"));
        }
    }
    md.push_str(RECIPES);
    md.push_str("\n## The complete default settings\n\n```jsonc\n");
    md.push_str(settings::DEFAULTS);
    md.push_str("\n```\n");
    let _ = std::fs::write(dir.join("SETTINGS.md"), md);
    let _ = std::fs::write(dir.join("settings.schema.json"), serde_json::to_string_pretty(&schema()).unwrap_or_default());
    let p = settings::path();
    if !p.exists() {
        let _ = std::fs::write(
            &p,
            "// Agent Box settings: everything here overrides the built-in defaults.\n// Reference: SETTINGS.md in this folder (for you and for agents).\n{\n  \"$schema\": \"./settings.schema.json\"\n}\n",
        );
    }
}

fn schema() -> Value {
    let types: Vec<&str> = NODE_TYPES.iter().map(|(t, _)| *t).filter(|t| !t.starts_with('<')).collect();
    let actions: Vec<String> = ACTIONS.iter().flat_map(|(a, _)| a.split(" / ").map(str::to_string)).collect();
    let styles: serde_json::Map<String, Value> = STYLE_KEYS
        .iter()
        .flat_map(|(k, d)| k.split(" / ").map(move |k| (k.to_string(), json!({"description": d}))))
        .collect();
    json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "title": "Agent Box settings",
        "description": "See SETTINGS.md in this folder.",
        "type": "object",
        "definitions": {
            "action": {"anyOf": [
                {"type": "string", "enum": actions},
                {"type": "object", "properties": {"do": {"type": "string", "enum": actions}}, "required": ["do"]},
                {"type": "array", "items": {"$ref": "#/definitions/action"}}
            ]},
            "style": {"type": "object", "properties": styles},
            "node": {"anyOf": [
                {"type": "string"},
                {"type": "array", "items": {"$ref": "#/definitions/node"}},
                {"type": "object", "properties": {
                    "type": {"type": "string", "description": format!("One of {} or a component name", types.join(", "))},
                    "id": {"type": "string"}, "if": {"type": "string"}, "for": {"type": "string"},
                    "as": {"type": "string"}, "key": {"type": "string"}, "props": {"type": "object"},
                    "style": {"$ref": "#/definitions/style"},
                    "on": {"type": "object", "additionalProperties": {"$ref": "#/definitions/action"}},
                    "menu": {"type": "array"}, "tooltip": {"type": "string"},
                    "animate": {"type": "object"}, "transition": {"type": "object"},
                    "children": {"$ref": "#/definitions/node"}
                }}
            ]}
        },
        "properties": {
            "views": {"type": "object", "additionalProperties": {"$ref": "#/definitions/node"}},
            "components": {"type": "object", "additionalProperties": {"$ref": "#/definitions/node"}},
            "theme": {"type": "object"},
            "state": {"type": "object"},
            "keys": {"type": "array", "items": {"type": "object", "properties": {"key": {"type": "string"}, "do": {"$ref": "#/definitions/action"}, "if": {"type": "string"}}}},
            "commands": {"type": "array", "items": {"type": "object", "properties": {"label": {"type": "string"}, "do": {"$ref": "#/definitions/action"}, "if": {"type": "string"}}}},
            "chat": {"type": "object"}, "terminal": {"type": "object"}, "connection": {"type": "object"}
        }
    })
}
