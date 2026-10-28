/*! Writes SETTINGS.md (the reference an agent reads before changing the app) and settings.schema.json
next to the settings file, from the same tables the interpreter uses, so they never drift. */

use super::actions::ACTIONS;
use super::render::NODE_TYPES;
use super::style::STYLE_KEYS;
use crate::settings;
use serde_json::{json, Value};

const GUIDE: &str = r##"# a.box settings reference

a.box is a container plus data: **everything you see and every button's behavior is defined in
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
  the settings file, or to `run.d/<id>.json` (any number at once, run oldest first); the app runs each
  once and deletes the file. A request in `run.d` (or `{"id": "...", "action": ..., "read": jq}` in
  `run.json`) gets `results/<id>.json`: `{id, ok, error, errors, value}` (errors noted while it ran;
  `value`: its `read` expression against the context afterwards). `abx do --wait` waits for it. On the
  other machine, `abx do` relays through the daemon and its result comes back there;
  `inspect.remote_actions` ("all", "none", or a list of action names) says which relayed actions run.
- And see it: `inspect.json` (kept current) holds the live context, the window, and where every node
  with an `id` is drawn; `activity.log` lists every action run (clicks, keys, agents) with a timestamp;
  the `app.snapshot` action writes `snapshot.png`. From a shell, `abx` wraps all of this
  (`abx state`, `abx layout`, `abx do`, `abx shot`, `abx log`, `abx problems`).
- Beyond the context, `inspect.json` has `views` (each transcript's recent items, open steps,
  background work and attachments; the diff's files; every terminal's screen lines), `internal`
  (back/forward visits, recent sends, opened and closed chats, subagent tabs, hook events),
  `dialog` (an open text dialog's fields and values: answer it with `dialog.submit`), `palette`
  (its entries: run one with `palette.run`), `clickable` (for `ui.click`, double-clicks and presses
  included) and `menus` (right-click menus: run an item with `ui.menu`), and `ui`: every `input`
  node's text (`ui.type`, `ui.submit`), scroll positions (`ui.scroll`), the focused and hovered nodes
  (`ui.focus`, `ui.hover`, `ui.key`), tooltips, menus with their items, and drop targets (`ui.drop`).
  `layout` lists every node with an `id`, moving or not. `.app.notifications`,
  `.app.dialog`, `.app.focus_area` and `.app.palette_open` are in the context itself.
- The built-in defaults are read live from `default-settings.jsonc` in the source checkout when it
  exists, so editing them hot-reloads too; only changes to the app's Rust code need a restart.
- UI state (selection, toggles, sizes) persists in `ui-state.json` next to the settings file.

## Top-level keys

| key | what it is |
| --- | --- |
| `patch` | Edits to default nodes by `id` (move, set, insert, replace, remove). **Prefer this to copying a component**: a copy freezes it, so later improvements to the defaults stop reaching you. See Patch. |
| `views` | Root nodes by name. `views.main` is the window. |
| `components` | Reusable nodes; use one as `{"type": "Name", "props": {...}}` and read props inside as `.props.x`. |
| `theme` | Named values referenced as `@name` in styles (colors, fonts) and readable as `.theme.name`. `animations: false` turns animation off. |
| `state` | UI state and starting values. Read as `.state.key`; change with `state.set` / `state.toggle` / `state.cycle`. |
| `events` | Actions to run when things happen. Each is `{"if": jq, "do": action}` or a list of them (a list in your file adds to the default's handlers; give one an `id` to replace the handler with that id). See Events. |
| `events_options` | `transitions`: which chat state changes are which events, `[{from: state \| [states] \| "*", to, event}]` (default: anything to waiting is `chat.waiting`, working to idle `chat.finished`, working/waiting/idle to stopped `chat.stopped`). |
| `dock` | `badge`: a template for the Dock icon's badge (empty or 0 hides it; re-evaluated whenever the context changes); `menu`: items for the Dock icon's right-click menu, like `menu_bar` items. |
| `hosts` | Where things run, by host (`box`, `mac`, or any name): `label`, `icon`, `color`, and `connection` `{ssh_host, daemon, ssh_options?, retry_seconds?, watch_ms?, enabled?}` (no ssh_host: run the daemon here). Without its own `connection`, `app.default_host` uses `connection` and `app.local_host` uses `local`. Read as `.hosts`; every chat has `.host` and every worktree `.host`. |
| `app` | `local_host` ("mac": the machine the app runs on), `default_host` ("box": where things go when nothing says), `source_dir` (a.box's checkout, ~/dev/agent-box), `bin_dir` (its scripts; default `bin` there), `path` (PATH for everything the app runs), `auto_update`, `update_check_secs`, `update_off_check_secs`, `min_update_check_secs`, `update_command` / `restart_command` (null: the built-in ones). |
| `window` | `title`, `width`, `height`, `traffic_lights`, `appearance` (dark, light, system: follows macOS live), `native_titlebar`, `remember_frame` (reopen where it was), `activate_on_start`, `on_close` (close: the window closes and the app keeps running; hide; quit; keep: the window stays). Title and appearance re-apply on save. |
| `menu_bar` | `app_menu`: the a.box menu's items (default Settings, Command Palette, `{"splice": "app"}` for `app`'s items, Quit); `app`: items added to it; `menus`: menus of their own `{name, items}`. Items: `{label, do, if, checked, disabled, shortcut, items (a submenu), id}` or `{separator: true}`; label/if/checked are jq against the context and the menus rebuild when they change. |
| `uploads` | `dir` (where pasted and dropped files are copied, ~/.agent-box/uploads) and `remote_dir` (the same on the other machine). |
| `sounds` | A sound per event: `chat.finished`, `chat.question` (a finished turn ending in a question), `chat.waiting`, `chat.stopped`, `designer.finished`. Values are macOS sound names or audio file paths; null silences one. Also `volume` (0-1), `when` (`always` or `unfocused`), `enabled`. The `sound` action plays one anywhere. |
| `logo` | The logo shown by `logo` nodes, the Dock while the app runs, and the app bundle's icon. See Logo. |
| `keys` | `[{"key": "cmd-k", "do": <action>, "if": <jq>}]`. Key names: `cmd`, `ctrl`, `alt`, `shift` + `-` + key (`cmd-shift-p`, `ctrl-\``, `cmd-\\`). |
| `commands` | Command palette entries: `[{"label", "do", "if"}]`. Worktrees are added automatically. |
| `motion` | How things move: `move` (glide to new positions), `enter` (how new nodes come in), `style` (colors and sizes ease), `enabled`. See Motion. |
| `chat` | Look of the built-in transcript, composer, and waiting card: style objects per part. |
| `terminal` | Terminal font, size, padding, colors. |
| `connection` | The default host's connection: `ssh_host`, `daemon` path, `retry_seconds`, `ssh_options`, `watch_ms`, `env_push_seconds`, `env_push_command` (null: `bin_dir`'s env-push). `local` is this Mac's daemon (`enabled`, `daemon`). |
| `input` | `press_dedupe_ms` (50): how long one press counts as the same press for every node under it. |
| `scripts_options` | `context_cache_ms`, `min_timer_ms`, `keep_data` (true: `.data` survives a script reload). |
| `setup` | The setup guide a fresh install opens with (components `Setup`, state `setup_step`): `command` (null: `bin/setup` in `app.source_dir`), `welcome`, `tools` (the CLIs it checks for), `tour` (its stops: `{id, title, text}`). "Set up a.box…" in the palette reopens it. |
| `settings_agent` | `command`: the `settings.agent` run (`{dir}`, `{claude}`, `$REQUEST`; null: the built-in one). |

## Logo

`logo.art` is SVG markup or the path of an `.svg` file or an image (PNG, JPEG, GIF, WebP) (relative paths look in this folder first, then next
to the defaults; the built-in art is `logo.png`). Saving the file updates the app live. `currentColor` in
an SVG becomes `logo.ink`; with `tint: true` an image is redrawn in `ink` too, keeping its transparency. `tile` is the rounded square behind it (a color, or null for none), with
`corner` (radius) and `padding` as fractions of its side. `dock` overrides any of these for the Dock icon
(`margin` leaves room around the tile like other macOS icons); `"dock": false` keeps the bundle's icon.
Colors can be `@theme` tokens. The bundle icon (Finder, the Dock when the app is closed) is rebuilt from
these by `bundle.sh`.

```jsonc
{"logo": {"art": "my-logo.svg", "ink": "#2b1d3a", "tile": "#ffd6e7"}}
```

## Scripts

Anything the built-in actions and data can't do, a script can. Scripts are Rust: `scripts/<name>.rs` next to
the settings file (or `scripts` in settings, `{"name.rs": "source"}`), each with a `fn init()`. The app compiles
each one to WebAssembly in the background (a few seconds after a save; the first build takes longer) and runs it
with wasmtime: near-native speed, its own small memory, nothing running while idle. Saving one rebuilds it and
restarts every script (fresh state, old timers stopped); until a changed script builds, the rest run.
Compile errors and panics go to `problems.txt`; `ab::log` writes `scripts.log`. A script that runs longer than
`scripts_options.time_limit_ms` (1000) in one go is stopped, so the app never freezes. Scripts can use crates
from `scripts_options.dependencies` (`{"regex": "1"}`) besides `ab` and `serde_json` (`ab::json!`, `ab::Value`).

```rust
fn init() {
    // a new action, usable anywhere an action is (buttons, keys, palette, run.json, other scripts)
    ab::action("box.uptime", |_args, _ctx| {
        ab::daemon("exec", json!({"cmd": "uptime -p"}), |r, _err| {
            ab::data("uptime", r["stdout"].as_str().unwrap_or("").trim());
        });
    });
    // react to things: start, model, settings events (chat.waiting, chat.finished, ...), and your own ab::emit
    ab::on("start", |_, _| { ab::every(60_000, || ab::run(json!("box.uptime"))); });
    ab::on("chat.finished", |p, _| ab::log(format!("finished {}", p["chat"]["name"])));
}
```

Then show it with a patch: `{ "insert": { "type": "text", "props": { "text": "{{ .data.uptime }}" } }, "after": "status-pill" }`.

| `ab::` | does |
| --- | --- |
| `action(name, \|args, ctx\| ..)` | define an action |
| `on(event, \|payload, ctx\| ..)` / `emit(event, payload)` | handle / raise events (`start` fires on every (re)load; `model` on every host update; every event in Events too, its fields as the payload) |
| `run(action)` | run any action (`json!({"do": ..})` or `json!("name")`) |
| `context()` / `state()` / `set(key, value)` | read the context and UI state; set state (what `set` and `data` change shows in `context()` at once) |
| `data(name, value)` | publish a value as `.data.<name>` for expressions (kept across script reloads unless `scripts_options.keep_data` is false) |
| `every(ms, f)` / `after(ms, f)` / `cancel(id)` | timers |
| `shell(cmd, json!({cwd, host, timeout, env}), \|result, err\| ..)` | run a command on the Mac (or on host's daemon): `{code, stdout, stderr}` |
| `daemon(method, params, f)` / `daemon_on(Some(host), method, params, f)` | call a daemon (`exec` runs bash there: `{cmd, cwd?, timeout_secs?}`) |
| `http(json!({url, method, headers, body, timeout_secs}), f)` / `fetch(url, f)` | HTTP through curl without a shell: `{status, ok, headers, body}` |
| `read(path)` / `write(path, text, \|err\| ..)` | files (`~/` allowed); read gives None when the file can't be read |
| `call(name, args, f)` / `call_sync(name, args)` | the app's abilities by name (table below); call_sync for the fs and env ones |
| `clipboard::read(f)` / `clipboard::write(text)` | the clipboard (`{text}`) |
| `fs::list(path)` / `fs::exists` / `fs::stat` / `fs::mkdir` / `fs::remove` / `fs::watch(path, \|paths\| ..)` | files and folders, at once (watch calls f on changes) |
| `spawn(cmd, json!({cwd, env, stdin}), \|what, data\| ..)` | a process whose output streams in (`Proc::Stdout`/`Stderr` lines, then `Proc::Exit`); returns a handle with `write(data)` and `kill()` |
| `inspect(f)` | what inspect.json holds right now |
| `patch(ops)` / `key(key, action, json!({if, not_in}))` / `command(label, action, json!({if}))` | change the UI at runtime, add a key or a palette entry; undone when scripts reload |
| `notify(text)` / `log(text)` | a notice in the window / a line in scripts.log |

The `ab` crate is `rust/script-sdk` in a.box's source (`app.source_dir`); builds live in `scripts_options.build_dir`
(`~/.agent-box/script-build`) and need `cargo` with the `wasm32-unknown-unknown` target (added on first build).

## Patch

Ops run in order after merging, each naming a node by its `id` anywhere in `views` or `components`:

```jsonc
"patch": [
  { "id": "designer-button", "move": { "after": "sidebar-drag" } },     // also "before": id, or "into": id with "at": n (negative from the end)
  { "id": "sidebar-header", "set": { "style": { "padding": [0, 12, 0, 88] } } },  // merged into the node; null removes a key
  { "id": "waiting", "replace": { "type": "waiting", "style": { "padding": [0, 12] } } },
  { "id": "status-pill", "remove": true },
  { "insert": { "type": "Pill", "props": { "text": "Docs", "do": "settings.docs" } }, "after": "status-pill" }
]
```

Nodes with ids in the defaults include `sidebar`, `sidebar-header`, `sidebar-toggle`, `designer-button`,
`sidebar-drag`, `sidebar-list`, `sidebar-footer`, `main-area`, `toolbar`, `toolbar-drag`, `status-pill`,
`chat-card`, `transcript`, `waiting`, `composer-box`, `branch-bar`, `changes-pill`,
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
  // menu items also take "icon", "shortcut" (shown at the right), "checked" (jq: a checkbox),
  // "danger" (red), "disabled", and "items" (a submenu)
  "tooltip": "{{ .w.branch }}",
  "drag": { "kind": "worktree", "value": "{{ .w.path }}", "label": "{{ .w.title }}" },  // makes the node draggable
  // and on a drop target: "on": { "drop_item": { "accept": "worktree", "do": {"do": "worktree.move", "before": "{{ .w.path }}"} } }
  // (the dragged value arrives as the action's `dragged`, the pointer's zone as `zone`; add "valid": a jq
  // test on .dragged/.zone so impossible or no-op drops show nothing and do nothing; "drop_zones": "edges"
  // (before/after by height), "edges_x" (by width, for rows of tabs), "edges+inside", or per kind {kind: mode})
  "animate": { "from": {"opacity": 0, "y": 6}, "duration": 160 },   // plays when the node first appears
  "transition": { "width": 200 },     // these style keys (numbers or colors) ease to their new value (ms)
  "motion": false,                    // opt this node out of gliding, enter animation, and style easing
  "layer": "top",                     // paint above everything else and keep clicks off what's under it (popovers, dialogs)
  "occlude": true,                    // keep clicks and hover off whatever is under this node
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
| `.chats[]` | every chat: `{id, name, kind, agent, host, status, dir, branch, rc_url, alive, state, waiting_on, last_activity, age_text, active, tmux}`. `state` is idle, working, waiting, or stopped; `host` is box or mac; `agent` is the CLI it runs (a key of `.agents`); `activity` is a short line on what a working agent is doing now ("Reading settings.jsonc", "Thinking", "Writing"), empty otherwise (filled for chats whose transcript is open); `status_line` is the agent's own working line read from its pane, `{verb, since, tokens, changed_at, notes}` or null (`changed_at`: when the token count last moved); `orchestrator` is whether it leads its section (`chat.orchestrator`); `model_used` is the model its newest reply came from (else the `model` it was started with, else null: the agent's default). |
| `.agents` | settings `daemon.agents`: each agent CLI a chat can run (`{name, icon, color, ...}` by key, e.g. `.agents[.c.agent].icon`) |
| `.designer` | `{working, activity, has_messages}` for the designer chat |
| `.selected.worktree` | the selected worktree (or null) |
| `.selected.chat` | its active chat (or null) |
| `.selected.chats` | all chats in the selected worktree |
| `.selected.diff` | the selected worktree's changes for `state.diff_base`: `{loaded, loading, error, base, base_label, files, added, removed, truncated}` |
| `.state` | UI state (see `state` in the defaults) |
| `.theme` | theme values |
| `.app` | `{connected, status, ssh_host, settings_error, errors, dragging (files over the window), focused (the window is in front), window {width, height}, layout {id: {x, y, w, h}}, hooks (recent hook events), host_errors {host: [the connection's last stderr lines]}, hosts_up, local_host, default_host, notifications [{at, kind: toast \| system \| sound, text, title}], nav {back, forward, at, visits}, script_actions, palette (while views.palette draws it), settings_sheet (while views.settings does), dialog {title, fields, values}}` |
| `.selected.transcript` | the active chat's newest items (`inspect.transcript_items`): `{kind: user \| assistant \| thinking \| notice \| image \| tool, text, tool, summary, is_error, done}` |
| `.composers` | what's typed in each chat's message box, by chat id |
| `.settings` | the merged settings, without views, components, theme, and patch |
| `.now` | unix seconds |
| `.props`, `.<as>`, `.index`, `.input`, `.inputs`, `.output`, `.path` | inside components, `for` loops, and the `then` of prompt / clipboard.read (`.input`), form (`.inputs`), shell / box / http (`.output`), worktree.new / app.snapshot (`.path`) |

## Events

`events.<name>` in settings and `ab.on(name)` in scripts. The fields listed are in the handler's context
(and a script's payload):

| event | when | fields |
| --- | --- | --- |
| `app.start` | the window opened | |
| `app.focus` / `app.blur` | the window came to the front / went behind | |
| `app.updated` | auto-update pulled a new build | `.what`, `.restarting` |
| `app.snapshot_done` | `app.snapshot` finished | `.path`, `.ok` |
| `settings.reloaded` | settings saved and applied | |
| `model` | a host sent its new state | `.host` |
| `host.connected` / `host.disconnected` | a daemon connection came up / dropped | `.host`, `.why` |
| `hook` | an agent hook fired on any host | `.hook {host, seq, at, event, session_id}` |
| `chat.added` / `chat.removed` | a chat appeared on / left a host | `.chat`, `.host` |
| `chat.state` | any chat state change | `.chat`, `.from` |
| `chat.waiting` / `chat.finished` / `chat.stopped` | by `events_options.transitions` | `.chat` (`.chat.asks`: its last reply ends in a question), `.from` |
| `chat.background_finished` | a chat.finished for a turn the agent started on its own (a subagent or background task reporting back) that ended without a reply or with a quiet one (`chat.quiet_reply`); nothing handles it by default | `.chat`, `.from` |
| `chat.message` | a new message arrived in an open chat | `.chat`, `.message {kind, text}` |
| `worktree.added` / `worktree.removed` | a worktree appeared / went | `.worktree`, `.host` |
| `worktree.mr` | a worktree's merge request or its checks changed | `.worktree`, `.from` (the old one) |
| `select` | the selected worktree or chat changed | `.worktree` (path), `.chat` (id) |
| `notification.click` | a banner without its own `do` was clicked | `.chat`, `.id` |
| `type_to_focus` | typing with nothing focused | `.text` |
| `usage.level` | the usage gauge's level changed (ok, warning, danger) | `.level`, `.from` |
| `designer.finished` | the designer replied | |
| any `emit` / `ab.emit` name | raised by settings or a script | `.payload` |
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
    for (t, d) in all_node_types() {
        md.push_str(&format!("| `{t}` | {d} |\n"));
    }
    md.push_str("\n## Style keys\n\n| key | value |\n| --- | --- |\n");
    for (k, d) in STYLE_KEYS {
        md.push_str(&format!("| `{k}` | {d} |\n"));
    }
    md.push_str("\n## Actions\n\nUsed by `on`, `menu`, `keys`, and `commands`. An action is a name (`\"palette.open\"`), an object\n`{\"do\": \"name\", ...args}`, or a list of actions run in order.\n\n| action | arguments |\n| --- | --- |\n");
    for (a, d) in all_actions() {
        md.push_str(&format!("| `{a}` | {d} |\n"));
    }
    md.push_str("\n## Script calls\n\n`ab.call(name, args, cb)` from a script.\n\n| call | does |\n| --- | --- |\n");
    for (c, d) in crate::script::CALLS {
        md.push_str(&format!("| `{c}` | {d} |\n"));
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
    md.push_str(&settings::defaults_text());
    md.push_str("\n```\n");
    let _ = std::fs::write(dir.join("SETTINGS.md"), md);
    let _ = std::fs::write(dir.join("settings.schema.json"), serde_json::to_string_pretty(&schema()).unwrap_or_default());
    let p = settings::path();
    if !p.exists() {
        let _ = std::fs::write(
            &p,
            "// a.box settings: everything here overrides the built-in defaults.\n// Reference: SETTINGS.md in this folder (for you and for agents).\n{\n  \"$schema\": \"./settings.schema.json\"\n}\n",
        );
    }
}

fn all_node_types() -> impl Iterator<Item = &'static (&'static str, &'static str)> {
    NODE_TYPES.iter().chain(crate::ui::widgets::NODE_TYPES)
}

fn all_actions() -> impl Iterator<Item = &'static (&'static str, &'static str)> {
    ACTIONS.iter().chain(crate::ui::trace_view::ACTIONS).chain(crate::ui::quote_view::ACTIONS).chain(crate::ui::diff_view::ACTIONS).chain(crate::terminal::ACTIONS).chain(crate::ui::drive::ACTIONS)
}

fn schema() -> Value {
    let types: Vec<&str> = all_node_types().map(|(t, _)| *t).filter(|t| !t.starts_with('<') && !t.starts_with('(')).collect();
    let actions: Vec<String> = all_actions().flat_map(|(a, _)| a.split(" / ").map(str::to_string)).collect();
    let styles: serde_json::Map<String, Value> = STYLE_KEYS
        .iter()
        .flat_map(|(k, d)| k.split(" / ").map(move |k| (k.to_string(), json!({"description": d}))))
        .collect();
    json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "title": "a.box settings",
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
                    "on": {"type": "object", "properties": {"key": {"type": ["object", "array"]}}, "additionalProperties": {"$ref": "#/definitions/action"}},
                    "menu": {"type": "array"}, "menu_on": {"enum": ["right_click", "click"]}, "tooltip": {"type": "string"},
                    "focusable": {"type": ["boolean", "string"]}, "layer": {"type": ["string", "number"]},
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
