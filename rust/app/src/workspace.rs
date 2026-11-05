/*! The window's state: the daemon connection, the model, selection, UI state, transcripts, composers,
and terminals. What it looks like and does comes entirely from settings (see `ui`). */

use crate::remote::{Incoming, Remote, RemoteGuard};
use crate::settings::{self, parse_color, Settings};
use crate::terminal::TerminalView;
use crate::transcript::{Item, Transcript};
use crate::ui::expr::{to_val, Exprs};
use crate::ui::render::{value_text, Transition};
use crate::ui::style;
use agent_box_protocol::{Call, Chat, Event, Model, Worktree};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use jaq_json::Val;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::time::Duration;

actions!(agent_box, [Quit, OpenSettings, Palette]);

/** A settings `menu_bar` (or `dock.menu`) item, by its id (its `id`, else its place). */
#[derive(Action, Clone, PartialEq, serde::Deserialize)]
#[action(namespace = agent_box, no_json)]
pub struct MenuBarItem {
    pub id: String,
}

thread_local! {
    static MENU_ACTIONS: std::cell::RefCell<HashMap<String, Value>> = std::cell::RefCell::new(HashMap::new());
    /** Shortcuts already bound to menu items (bindings can't be taken back one by one). */
    static MENU_KEYS: std::cell::RefCell<HashSet<(String, String)>> = std::cell::RefCell::new(HashSet::new());
}

/** The a.box menu when settings give no `menu_bar.app_menu`: Settings, Command Palette, `menu_bar.app`, Quit. */
fn default_app_menu() -> Value {
    json!([
        {"id": "settings", "label": "{{ .settings.strings.menu_settings // \"Settings…\" }}", "do": "settings.open"},
        {"id": "palette", "label": "{{ .settings.strings.menu_palette // \"Command Palette\" }}", "do": "palette.open"},
        {"splice": "app"},
        {"separator": true},
        {"id": "quit", "label": "{{ .settings.strings.menu_quit // \"Quit {app}\" }}", "do": "app.quit"}
    ])
}

/** Settings `menu_bar` with each item's `if`, `label`, `checked`, `disabled` and `shortcut` worked out
against the context: `[{name, items}]`, the a.box menu first. `do` stays as written (it runs at click time). */
pub(crate) fn resolve_menus(settings: &Settings, exprs: &crate::ui::expr::Exprs, ctx: &Val) -> Value {
    let app_name = settings.text("app_name", "a.box");
    fn items(list: &[Value], settings: &Settings, exprs: &crate::ui::expr::Exprs, ctx: &Val, app_name: &str, path: &str) -> Vec<Value> {
        let mut out = vec![];
        for (i, it) in list.iter().enumerate() {
            if let Some(name) = it.get("splice").and_then(Value::as_str) {
                let extra = settings.get(&format!("menu_bar.{name}")).and_then(Value::as_array).cloned().unwrap_or_default();
                let extra = items(&extra, settings, exprs, ctx, app_name, &format!("{path}{name}."));
                if !extra.is_empty() {
                    out.push(json!({"separator": true}));
                    out.extend(extra);
                }
                continue;
            }
            if it.get("separator").and_then(Value::as_bool) == Some(true) {
                out.push(json!({"separator": true}));
                continue;
            }
            if let Some(cond) = it.get("if").and_then(Value::as_str) {
                if !exprs.truthy(cond, ctx) {
                    continue;
                }
            }
            let flag = |k: &str| match it.get(k) {
                Some(Value::Bool(b)) => *b,
                Some(Value::String(code)) => exprs.truthy(code, ctx),
                _ => false,
            };
            let label = value_text(&exprs.resolve(it.get("label").unwrap_or(&Value::Null), ctx)).replace("{app}", app_name);
            let id = it.get("id").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| format!("{path}{i}"));
            let mut item = json!({"id": id, "label": label, "checked": flag("checked"), "disabled": flag("disabled"), "shortcut": it.get("shortcut"), "do": it.get("do")});
            if let Some(sub) = it.get("items").and_then(Value::as_array) {
                item["items"] = Value::Array(items(sub, settings, exprs, ctx, app_name, &format!("{id}.")));
            }
            out.push(item);
        }
        out
    }
    let app_menu = settings.get("menu_bar.app_menu").and_then(Value::as_array).cloned().unwrap_or_else(|| default_app_menu().as_array().cloned().unwrap_or_default());
    let mut menus = vec![json!({"name": app_name, "items": items(&app_menu, settings, exprs, ctx, &app_name, "app.")})];
    for (m, menu) in settings.get("menu_bar.menus").and_then(Value::as_array).cloned().unwrap_or_default().iter().enumerate() {
        let name = value_text(&exprs.resolve(menu.get("name").unwrap_or(&Value::Null), ctx));
        let list = menu.get("items").and_then(Value::as_array).cloned().unwrap_or_default();
        menus.push(json!({"name": name, "items": items(&list, settings, exprs, ctx, &app_name, &format!("menu{m}."))}));
    }
    Value::Array(menus)
}

/** Menu items for resolved entries; each item's `do` is remembered under its id, and its `shortcut` bound to it. */
fn menu_items(list: &[Value], actions: &mut HashMap<String, Value>, cx: &mut App) -> Vec<MenuItem> {
    list.iter()
        .map(|it| {
            if it.get("separator").and_then(Value::as_bool) == Some(true) {
                return MenuItem::separator();
            }
            let label = it["label"].as_str().unwrap_or("").to_string();
            if let Some(sub) = it.get("items").and_then(Value::as_array) {
                return MenuItem::submenu(Menu { name: label.into(), items: menu_items(sub, actions, cx), disabled: it["disabled"] == true });
            }
            let id = it["id"].as_str().unwrap_or("").to_string();
            let item = match it["do"].as_str() {
                // These keep working while no window is open.
                Some("app.quit") => MenuItem::action(label, Quit),
                Some("settings.open") => MenuItem::action(label, OpenSettings),
                Some("palette.open") => MenuItem::action(label, Palette),
                _ => {
                    actions.insert(id.clone(), it["do"].clone());
                    if let Some(key) = it["shortcut"].as_str().filter(|k| !k.is_empty()) {
                        let fresh = MENU_KEYS.with(|k| k.borrow_mut().insert((key.to_string(), id.clone())));
                        if fresh {
                            cx.bind_keys([KeyBinding::new(key, MenuBarItem { id: id.clone() }, None)]);
                        }
                    }
                    MenuItem::action(label, MenuBarItem { id })
                }
            };
            item.checked(it["checked"] == true).disabled(it["disabled"] == true)
        })
        .collect()
}

fn build_menu_bar(settings: &Settings, spec: &Value, cx: &mut App) {
    let mut actions = HashMap::new();
    let menus: Vec<Menu> = spec
        .as_array()
        .into_iter()
        .flatten()
        .map(|m| Menu { name: m["name"].as_str().unwrap_or("").to_string().into(), items: menu_items(m["items"].as_array().map(Vec::as_slice).unwrap_or(&[]), &mut actions, cx), disabled: false })
        .collect();
    // `dock.menu`: items for the Dock icon's right-click menu, like menu_bar items.
    let dock = settings.dock.get("menu").and_then(Value::as_array).cloned().unwrap_or_default();
    let dock_items = menu_items(&dock.iter().enumerate().map(|(i, it)| {
        let mut it = it.clone();
        if it.get("id").is_none() && it.get("separator").is_none() {
            it["id"] = json!(format!("dock.{i}"));
        }
        it
    }).collect::<Vec<_>>(), &mut actions, cx);
    MENU_ACTIONS.with(|a| *a.borrow_mut() = actions);
    cx.set_menus(menus);
    cx.set_dock_menu(dock_items);
}

/** The menu bar before the window exists (its templates see only `.settings`). */
pub fn set_menu_bar(settings: &Settings, cx: &mut App) {
    let exprs = crate::ui::expr::Exprs::default();
    let ctx = to_val(&json!({"settings": settings.raw}));
    let spec = resolve_menus(settings, &exprs, &ctx);
    build_menu_bar(settings, &spec, cx);
}

/** The component kit's light or dark look from settings `window.appearance` ("dark" | "light" | "system",
which follows the OS). */
pub fn apply_appearance(settings: &Settings, os: WindowAppearance, cx: &mut App) {
    let mode = match settings.get("window.appearance").and_then(Value::as_str) {
        Some("light") => gpui_kit::component::ThemeMode::Light,
        Some("system") => match os {
            WindowAppearance::Light | WindowAppearance::VibrantLight => gpui_kit::component::ThemeMode::Light,
            _ => gpui_kit::component::ThemeMode::Dark,
        },
        _ => gpui_kit::component::ThemeMode::Dark,
    };
    gpui_kit::component::Theme::change(mode, None, cx);
}

pub struct Drag {
    pub key: String,
    pub vertical: bool,
    pub invert: bool,
    pub start: f32,
    pub start_size: f32,
    pub min: f32,
    pub max: f32,
}

/** A connection the app keeps: what it was started with, and what closes it. */
pub(crate) struct HostConn {
    spec: crate::behavior::HostSpec,
    path: String,
    _guard: RemoteGuard,
    _listen: Task<()>,
}

/** A process a script spawned (`ab.spawn`): its stdin and how to end it. */
struct Proc {
    stdin: Option<std::process::ChildStdin>,
    pid: u32,
}

/** What scripts added at runtime (`ab.patch`, `ab.key`, `ab.command`), applied over the settings until
they reload. */
#[derive(Default)]
struct ScriptOverlay {
    patches: Vec<Value>,
    keys: Vec<Value>,
    commands: Vec<Value>,
}

/** The palette while it's drawn from settings (`views.palette`): the highlighted entry (what's typed is
state `palette_query`). */
#[derive(Default, Clone)]
pub(crate) struct PaletteView {
    pub(crate) selected: usize,
}

pub struct Workspace {
    /** One daemon connection per host (settings `hosts.<name>.connection`, `connection`, `local`). */
    conns: HashMap<String, HostConn>,
    pub(crate) remotes: HashMap<String, Remote>,
    /** Each host's latest model; `model` is them merged (this Mac's first). */
    pub(crate) models: HashMap<String, Model>,
    pub(crate) host_up: HashMap<String, bool>,
    /** Since when each host that isn't up has been down (a host shows offline after `connection.offline_after_ms`). */
    down_since: HashMap<String, std::time::Instant>,
    /** When each host's model last arrived (epoch seconds), from the cache until it connects. */
    last_seen: HashMap<String, f64>,
    /** When each host's model was last written to the cache. */
    cache_saved: HashMap<String, std::time::Instant>,
    /** Where each merged project and section came from, by index and id. */
    pub(crate) project_hosts: Vec<Vec<String>>,
    pub(crate) section_hosts: HashMap<String, String>,
    /** Past chats from every host's `History`, for the history panel. */
    pub(crate) history: Vec<Value>,
    pub(crate) history_loading: bool,
    pub(crate) inputs: HashMap<String, Entity<gpui_kit::component::input::InputState>>,
    /** Each `input` node's `on.submit` action and the context it was drawn in. */
    pub(crate) input_submits: HashMap<String, (Value, jaq_json::Val)>,
    /** Chats whose older lines are being fetched. */
    /** Chats loading older lines; true: filling in after a quick first page, with no chip. */
    loading_earlier: HashMap<String, bool>,
    /** When each chat was last the one shown, for letting go of transcripts not looked at lately. */
    chat_seen: HashMap<String, std::time::Instant>,
    worktree_seen: HashMap<String, std::time::Instant>,
    /** What a dropped transcript still tells the sidebar: its newest reply and the model it came from. */
    dropped: HashMap<String, (Option<String>, Option<String>)>,
    /** The open completion list under a composer: (chat, trigger, entries, selected). */
    pub(crate) completion: Option<(String, String, Vec<crate::completions::Entry>, usize)>,
    /** Messages typed while a chat was working, sent one per turn as it finishes (`chat.queue`). */
    pub(crate) queued: HashMap<String, Vec<String>>,
    /** Text quoted from transcripts (`chat.add_selection`), going out at the top of each chat's next message. */
    pub(crate) quotes: HashMap<String, Vec<String>>,
    /** A waiting chat's picks so far on a card of several questions: which questions (`questions_view::stamp`), and an option per question. */
    pub(crate) picks: HashMap<String, (String, Vec<Option<usize>>)>,
    /** The sent times of the transcript whose row is drawing (its own are lent here while it's out). */
    pub(crate) row_times: HashMap<usize, f64>,
    pub(crate) select_tip: Option<crate::ui::quote_view::SelectTip>,
    /** Subagent conversations opened as tabs (read-only): (worktree path, a chat with id `sub:PARENT:AGENT`). */
    pub(crate) sub_chats: Vec<(String, Chat)>,
    queue_saved: String,
    pub(crate) usage: crate::usage::Usage,
    /** Drafts read at startup, waiting for their chat's message box; and what drafts.json holds. */
    drafts_loaded: HashMap<String, String>,
    drafts_saved: String,
    /** Chats sent a message in the last few seconds whose hook hasn't said they're working yet. */
    just_sent: HashMap<String, std::time::Instant>,
    /** Where you've been, browser-style: (worktree, chat) visits and the one showing now. */
    nav: Vec<(String, Option<String>)>,
    /** Recent notices of every kind (in-window, banners, sounds): {at, kind, text, title?}. */
    pub(crate) notices: Vec<Value>,
    /** Recent hook events from every daemon (for inspect.json). */
    pub(crate) hooks: Vec<Value>,
    /** The open text dialog (rename, new worktree, project settings, prompt), for agents. */
    pub(crate) dialog: Option<OpenDialog>,
    pub(crate) palette_open: bool,
    /** Where typing goes this frame (`focus_area`), as `.app.focus_area`. */
    focus_now: &'static str,
    nav_at: usize,
    nav_jumping: bool,
    /** This view, for click handlers built without a `cx` (message actions). */
    pub(crate) view_handle: Option<WeakEntity<Workspace>>,
    placeholders: HashMap<String, String>,
    /** When a queued message last went out, so it isn't sent twice before the chat turns busy. */
    queue_sent: HashMap<String, std::time::Instant>,
    pub(crate) settings: std::rc::Rc<Settings>,
    pub(crate) settings_error: Option<String>,
    pub(crate) remote: Remote,
    pub(crate) connected: bool,
    pub(crate) status: String,
    pub(crate) model: Model,
    pub(crate) selected: Option<String>,
    pub(crate) active_chat: HashMap<String, String>,
    /** Each worktree's tabs in the order they were last shown, newest last: closing one shows the one before. */
    tab_history: HashMap<String, Vec<String>>,
    pub(crate) state: Map<String, Value>,
    /** What the user changed (persisted); `state` is the settings' starting values with this on top. */
    user_state: Map<String, Value>,
    pub(crate) exprs: Exprs,
    pub(crate) transitions: HashMap<String, Transition>,
    pub(crate) entered: HashMap<String, (std::time::Instant, u64, Value)>,
    pub(crate) motion: std::rc::Rc<std::cell::RefCell<crate::ui::motion::MotionStore>>,
    pub(crate) pending_motion: bool,
    pub(crate) designer: crate::designer::Designer,
    /** The latest press: the innermost node with a handler under it, when, and where; and the node the
    latest single press (the first half of any double) landed on. */
    pub(crate) press: Option<(String, std::time::Instant, Point<Pixels>)>,
    /** While rendering inside a "motion": false node: its subtree doesn't animate. */
    pub(crate) motion_off: usize,
    pub(crate) first_press: Option<String>,
    /** The designer's conversations, as the history list last read them. */
    pub(crate) designer_sessions: Vec<Value>,
    designer_tx: flume::Sender<crate::designer::Out>,
    problems_written: String,
    pub(crate) render_generation: u64,
    pub(crate) scroll_handles: HashMap<String, ScrollHandle>,
    pub(crate) drag: Option<Drag>,
    pub(crate) pending_actions: Vec<Value>,
    pub(crate) pending_select: Option<String>,
    /** Chats opened as tabs this session (their process may still be starting). */
    pub(crate) opened: HashSet<String>,
    /** Chats closed this session: no tab even while their process winds down. */
    pub(crate) closed: HashSet<String>,
    /** The open `confirm` dialog: {title, text, confirm, cancel, do}. */
    pub(crate) confirm: Option<Value>,
    /** Worktree a `chat.new` is starting in, for a placeholder tab until the chat shows up. */
    pub(crate) starting: Option<String>,
    pub(crate) transcripts: HashMap<String, Transcript>,
    watching: HashSet<String>,
    pub(crate) composers: HashMap<String, Entity<TextareaState>>,
    /** Each composer's formatting turned back into plain text, and where its last typing ended (live_markdown). */
    live_md: HashMap<String, (HashSet<usize>, Option<usize>, Vec<(std::ops::Range<usize>, String)>)>,
    /** Each composer's misspelled words (`chat.spellcheck`), a count that lets only the latest check land, and
    the text the words' offsets are in. */
    spelling: HashMap<String, (Vec<std::ops::Range<usize>>, u64, String)>,
    /** Words "Ignore spelling" took out of the marks until a.box quits. */
    spell_ignored: HashSet<String>,
    pub(crate) terminals: HashMap<String, Entity<TerminalView>>,
    term_by_id: HashMap<(String, u64), Entity<TerminalView>>,
    pub(crate) expanded_tools: HashSet<String>,
    pub(crate) thumbs: crate::ui::thumbs::Thumbs,
    /** The image shown full size over the window (`image.view`): what it is (a path or chat:index) and its source. */
    pub(crate) image_view: Option<(String, ImageSource)>,
    pub(crate) diffs: HashMap<String, crate::ui::diff_view::DiffData>,
    pub(crate) attachments: HashMap<String, Vec<crate::ui::attach_view::Pending>>,
    pub(crate) send_when_ready: HashSet<String>,
    pub(crate) next_attachment: u64,
    pub(crate) dragging: bool,
    pub(crate) drop_hint: Option<crate::ui::render::DropHint>,
    /** What's being dragged (kind, value), set when a drag starts; its row is left out until the drop. */
    pub(crate) drag_item: std::rc::Rc<std::cell::RefCell<Option<(String, Value)>>>,
    /** The drag preview's second line. */
    pub(crate) drag_note: std::rc::Rc<std::cell::RefCell<String>>,
    /** The drag state the context was last built with. */
    drag_sig: String,
    pub(crate) drop_preview: bool,
    pub(crate) inspect_at: Option<std::time::Instant>,
    /** When the app's files last went to the other machines, and what each held. */
    pub(crate) mirror_at: Option<std::time::Instant>,
    /** Worktree moves not yet in their machine's model (keep_pending_moves). */
    pub(crate) pending_moves: Vec<PendingMove>,
    /** When each other machine last relayed an action here: the app's files go only to those using it. */
    pub(crate) relayed_at: HashMap<String, std::time::Instant>,
    /** A chat being moved and where to: selected there once it shows up. */
    pub(crate) pending_move: Option<(String, String)>,
    /** Moves this app started that haven't finished: their rows and tabs show a spinner. */
    pub(crate) moves: Vec<ChatMove>,
    move_serial: std::cell::Cell<u64>,
    pub(crate) perf: std::rc::Rc<std::cell::RefCell<crate::ui::inspect::Perf>>,
    /** The redraw a looping animation (spinner, pulse) asked for, at `motion.animation_fps`. */
    /** Each drawer's (region, or "" for the window) pending looping-animation redraw. */
    pub(crate) anim_ticks: HashMap<String, Task<()>>,
    /** Regions on screen: (view, what it was drawn from), what they registered, and their views by id. */
    pub(crate) regions: HashMap<String, (Entity<crate::ui::region::Region>, crate::ui::region::Fingerprint)>,
    pub(crate) kept: HashMap<String, crate::ui::region::Kept>,
    pub(crate) region_views: HashMap<String, WeakEntity<crate::ui::region::Region>>,
    pub(crate) region_deps: HashMap<String, (u64, crate::ui::region::Deps)>,
    pub(crate) regions_placed: HashSet<String>,
    pub(crate) live_regions: HashSet<String>,
    /** Draw every region this frame: an input event's handlers may have changed what no fingerprint covers. */
    pub(crate) refresh_regions: bool,
    pub(crate) refresh_now: bool,
    pub(crate) settings_rev: u64,
    /** Components with what their uses set on top (style, id, on, ...), by component and use, as of
    `settings_rev`. */
    pub(crate) component_uses: HashMap<(String, u64), (u64, std::rc::Rc<Value>)>,
    pub(crate) ctx_generation: std::cell::Cell<u64>,
    pub(crate) hash_memo: std::cell::RefCell<crate::ui::region::HashMemo>,
    /** Which region (or "" for the window) drew each enter animation and transition. */
    pub(crate) anim_owner: HashMap<String, String>,
    /** ABX_REGION_DEBUG: each region's per-path hashes, to report which path made it draw. */
    pub(crate) region_debug: HashMap<String, Vec<(String, u64)>>,
    /** Whether the node being drawn sits in a row (else a column): which way a region takes its content's size. */
    pub(crate) in_row: bool,
    /** The context each virtual list was last measured against. */
    pub(crate) list_generations: HashMap<String, (u64, u64)>,
    pub(crate) anim_epoch: std::time::Instant,
    /** Each chat's transcript list: only the rows on screen are built and laid out. */
    pub(crate) lists: HashMap<String, TranscriptList>,
    /** Virtual `scroll` nodes by id: list state, row keys, and each row's (node, context, path). */
    #[allow(clippy::type_complexity)]
    pub(crate) virtual_lists: HashMap<String, (ListState, Vec<String>, std::rc::Rc<Vec<(Value, jaq_json::Val, String)>>)>,
    pub(crate) activity: Vec<String>,
    /** What's clickable this frame: (node id, tooltip, action), for `ui.click` and inspect.json. */
    pub(crate) clickables: Vec<(String, String, Value)>,
    /** Right-click menus drawn this frame: (node id, tooltip, menu spec, its context), for `ui.menu`. */
    pub(crate) menus: Vec<(String, String, std::rc::Rc<Vec<Value>>, jaq_json::Val)>,
    /** Last frame's, complete (virtual-list rows included), for inspect.json and agent clicks. */
    pub(crate) clickables_last: Vec<(String, String, Value)>,
    pub(crate) menus_last: Vec<(String, String, std::rc::Rc<Vec<Value>>, jaq_json::Val)>,
    pub(crate) scripts: Option<crate::script::Scripts>,
    pub(crate) script_data: Map<String, Value>,
    script_timers: HashMap<u64, Task<()>>,
    pub(crate) ctx_rev: u64,
    ctx_cache: Option<(u64, std::time::Instant, std::rc::Rc<str>)>,
    /** context_data, and what it was built for: (ctx_rev, model seq, transcript revisions, frame). */
    ctx_data: std::cell::RefCell<Option<((u64, u64, u64, u64), std::time::Instant, Val)>>,
    chat_json_memo: std::cell::RefCell<Option<HashMap<String, Value>>>,
    /** The sidebar's order as last written to ~/.agent-box/sidebar-order.json, for the phone app. */
    sidebar_order_written: std::cell::RefCell<String>,
    /** While the context is built: the selected chat, found once for every chat it shows. */
    building_current: std::cell::RefCell<Option<Option<String>>>,
    /** Where each chat sits in `model.chats`, and each worktree in the model (project or section, then
    item), checked on every use and rebuilt when found out of date. */
    chat_index: std::cell::RefCell<HashMap<String, usize>>,
    worktree_index: std::cell::RefCell<HashMap<String, (bool, usize, usize)>>,
    orchestrator_memo: std::cell::RefCell<HashMap<String, (String, bool)>>,
    pub(crate) focused: bool,
    /** Events waiting for a window to run in: (name, extra context fields). */
    pending_events: Vec<(String, Map<String, Value>)>,
    /** inbox.read calls for a host that was down, sent when it connects. */
    pending_inbox_reads: HashMap<String, Vec<Call>>,
    /** Agents' requests (run.json, run.d, relayed): run in order, each with its result written. */
    pending_requests: Vec<(crate::behavior::Request, Option<String>)>,
    pub(crate) pending_script: Vec<Value>,
    /** Which load of the scripts is current (a build for an older one is dropped). */
    pub(crate) script_generation: u64,
    /** Banners' own click actions (`notify.system {do}`), by notification id. */
    pub(crate) notify_actions: HashMap<String, Value>,
    overlay: ScriptOverlay,
    /** The settings (and their warnings) before the overlay, while there is one. */
    settings_base: Option<(Value, Vec<String>)>,
    procs: HashMap<u64, Proc>,
    fs_watchers: HashMap<u64, notify::RecommendedWatcher>,
    pub(crate) window_size: (f32, f32),
    /** Each composer's text as of the last change, for `.composers`. */
    pub(crate) composer_texts: HashMap<String, String>,
    /** Where `composer.recall` is in each chat's sent messages (0: the newest). */
    recall: HashMap<String, usize>,
    pub(crate) palette_view: Option<PaletteView>,
    /** `views.settings` is drawing the settings panel. */
    pub(crate) settings_sheet: bool,
    /** Things whose change fires an event, as last seen. */
    seen_focus: Option<bool>,
    seen_select: Option<(String, Option<String>)>,
    seen_usage_level: Option<String>,
    /** The menu bar and badge as last built, so they're rebuilt only when what they show changes. */
    menu_sig: u64,
    shown_rev: u64,
    /** Settings to apply to the window at the next chance (a reload has no window at hand). */
    window_dirty: bool,
    hosts_dirty: bool,
    /** `.settings` and `.app.layout`, rebuilt only when settings or a frame changes them. */
    settings_val: Val,
    layout_val: std::cell::RefCell<Option<(u64, Val)>>,
    pub(crate) notify_count: u64,
    badge: Option<String>,
    dock_icon: Option<Vec<u8>>,
    pub(crate) diff_collapsed: HashSet<String>,
    /** Per diff (`PATH|BASE`): rows drawn, row height, each file's header row (inspect.json). */
    pub(crate) diff_geom: std::rc::Rc<std::cell::RefCell<Value>>,
    pub(crate) diff_scroll: HashMap<String, crate::ui::diff_view::DiffScroll>,
    focus: FocusHandle,
    _tasks: Vec<Task<()>>,
    pub(crate) _subscriptions: Vec<Subscription>,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // The window keeps the workspace as drawn until it changes (see main.rs `Shell`): every update
        // of it draws it again, notified or not.
        let me = cx.entity_id();
        cx.redraw_on_update(me);
        let fresh = !settings::path().exists();
        let (settings, settings_error) = match settings::load() {
            Ok(s) => (s, None),
            Err(e) => (Settings::default(), Some(format!("{e:#}"))),
        };
        crate::ui::docs::write_reference();
        crate::transcript::set_options(&settings.chat);
        set_upload_options(&settings);
        crate::designer::configure(&settings.designer, &crate::behavior::path_env(&settings.strings("app.path", DEFAULT_PATH), &std::env::var("HOME").unwrap_or_default()));
        let watch = Self::watch_settings(window, cx);
        // Usage and plan limits: refreshed every `usage.refresh_secs`.
        let usage_loop = cx.spawn(async move |this, cx| loop {
            let every = this.update(cx, |ws, cx| {
                ws.refresh_usage(false, None, cx);
                ws.settings.num("usage.refresh_secs", 120.).max(ws.settings.num("usage.min_refresh_secs", 15.))
            });
            let Ok(every) = every else { break };
            cx.background_executor().timer(Duration::from_secs_f64(every)).await;
        });
        // `app.auto_update`: pull a.box's main from its remote and rebuild what changed (the daemon, the app);
        // a new app build restarts the app. So a change pushed from the box ships to the Mac on its own.
        let update_loop = cx.spawn(async move |this, cx| loop {
            let plan = this.update(cx, |ws, _| {
                let on = ws.settings.get("app.auto_update").and_then(Value::as_bool) != Some(false);
                let every = if on { ws.settings.num("app.update_check_secs", 60.).max(ws.settings.num("app.min_update_check_secs", 15.)) } else { ws.settings.num("app.update_off_check_secs", 300.) };
                (on, every, ws.settings.get("app.update_command").and_then(Value::as_str).unwrap_or(UPDATE_SCRIPT).to_string(), ws.path_env(), settings::source_dir().to_string_lossy().into_owned())
            });
            let Ok((on, every, script, path, source)) = plan else { break };
            cx.background_executor().timer(Duration::from_secs_f64(every.max(1.))).await;
            if !on {
                continue;
            }
            let out = cx.background_spawn(async move { std::process::Command::new("/bin/sh").arg("-c").arg(script).env("PATH", path).env("AGENT_BOX_SOURCE", source).output() }).await;
            let text = out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
            if !text.starts_with("updated") {
                continue;
            }
            let app = text.split_whitespace().any(|w| w == "app");
            let restart = this.update(cx, |ws, cx| {
                ws.log_action(&json!({"do": "app.updated", "what": text}));
                let mut extra = Map::new();
                extra.insert("what".into(), Value::String(text.clone()));
                extra.insert("restarting".into(), Value::Bool(app));
                ws.pending_events.push(("app.updated".into(), extra));
                cx.notify();
                if app {
                    ws.save_drafts(cx);
                }
                ws.settings.get("app.restart_command").and_then(Value::as_str).unwrap_or(RESTART_COMMAND).to_string()
            });
            let Ok(restart) = restart else { break };
            if app {
                let _ = std::process::Command::new("/bin/sh").arg("-c").arg(restart).spawn();
                let _ = this.update(cx, |_, cx| cx.quit());
                break;
            }
        });
        let env_push = cx.spawn(async move |this, cx| loop {
            let plan = this.update(cx, |ws, _| (ws.settings.connection.env_push_seconds, ws.env_push_command(), ws.path_env()));
            let Ok((every, command, path)) = plan else { break };
            if every == 0 {
                cx.background_executor().timer(Duration::from_secs(60)).await;
                continue;
            }
            let _ = cx.background_spawn(async move { std::process::Command::new("/bin/sh").arg("-c").arg(command).env("PATH", path).output() }).await;
            cx.background_executor().timer(Duration::from_secs(every)).await;
        });
        let (designer_tx, designer_rx) = flume::unbounded();
        let designer_listen = cx.spawn(async move |this, cx| {
            while let Ok(out) = designer_rx.recv_async().await {
                if this.update(cx, |ws, cx| ws.on_designer(out, cx)).is_err() {
                    break;
                }
            }
        });
        #[cfg(target_os = "macos")]
        let clicks = crate::macos::init().map(|rx| {
            cx.spawn_in(window, async move |this, cx| {
                while let Ok(id) = rx.recv_async().await {
                    if this.update_in(cx, |ws, window, cx| ws.on_notification_click(&id, window, cx)).is_err() {
                        break;
                    }
                }
            })
        });
        #[cfg(not(target_os = "macos"))]
        let clicks: Option<Task<()>> = None;
        let mut transcripts: HashMap<String, Transcript> = HashMap::new();
        transcripts.entry(crate::designer::CHAT_ID.into()).or_default().append(0, &crate::designer::past_entries());
        let mut user_state = load_state();
        // A fresh install opens the setup guide (the Setup component); the palette's "Set up a.box" reopens it.
        if fresh && !user_state.contains_key("setup_step") {
            user_state.insert("setup_step".into(), json!("welcome"));
            let _ = std::fs::write(settings::dir().join("ui-state.json"), serde_json::to_string_pretty(&user_state).unwrap_or_default());
        }
        let mut state = settings.state.clone();
        state.extend(user_state.clone());
        let connecting = settings.text("connecting", "Connecting…");
        let ws = Self {
            conns: HashMap::new(),
            remotes: HashMap::new(),
            models: HashMap::new(),
            host_up: HashMap::new(),
            down_since: HashMap::new(),
            last_seen: HashMap::new(),
            cache_saved: HashMap::new(),
            project_hosts: vec![],
            section_hosts: HashMap::new(),
            history: vec![],
            history_loading: false,
            inputs: HashMap::new(),
            input_submits: HashMap::new(),
            loading_earlier: Default::default(),
            chat_seen: HashMap::new(),
            worktree_seen: HashMap::new(),
            dropped: HashMap::new(),
            completion: None,
            queued: std::fs::read_to_string(settings::dir().join("queued.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default(),
            queue_saved: String::new(),
            usage: Default::default(),
            drafts_loaded: std::fs::read_to_string(settings::dir().join("drafts.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default(),
            drafts_saved: String::new(),
            just_sent: HashMap::new(),
            nav: vec![],
            notices: vec![],
            hooks: vec![],
            dialog: None,
            palette_open: false,
            focus_now: "",
            nav_at: 0,
            nav_jumping: false,
            sub_chats: vec![],
            quotes: HashMap::new(),
            picks: HashMap::new(),
            row_times: HashMap::new(),
            select_tip: None,
            view_handle: None,
            placeholders: HashMap::new(),
            queue_sent: HashMap::new(),
            settings_val: Val::Null,
            layout_val: Default::default(),
            settings: std::rc::Rc::new(settings),
            settings_error,
            remote: Remote::detached(),
            connected: false,
            status: connecting,
            model: Model::default(),
            selected: None,
            active_chat: HashMap::new(),
            tab_history: HashMap::new(),
            state,
            user_state,
            exprs: Exprs::default(),
            transitions: HashMap::new(),
            entered: HashMap::new(),
            motion: Default::default(),
            pending_motion: false,
            designer: Default::default(),
            designer_sessions: vec![],
            press: None,
            motion_off: 0,
            first_press: None,
            designer_tx,
            problems_written: String::new(),
            render_generation: 0,
            scroll_handles: HashMap::new(),
            drag: None,
            pending_actions: vec![],
            pending_select: None,
            opened: HashSet::new(),
            closed: HashSet::new(),
            confirm: None,
            starting: None,
            transcripts,
            watching: HashSet::new(),
            composers: HashMap::new(),
            terminals: HashMap::new(),
            term_by_id: HashMap::new(),
            expanded_tools: HashSet::new(),
            thumbs: Default::default(),
            image_view: None,
            diffs: HashMap::new(),
            attachments: HashMap::new(),
            send_when_ready: HashSet::new(),
            next_attachment: 0,
            dragging: false,
            drop_hint: None,
            drag_item: Default::default(),
            drag_note: Default::default(),
            drag_sig: String::new(),
            drop_preview: false,
            inspect_at: None,
            mirror_at: None,
            pending_moves: vec![],
            relayed_at: HashMap::new(),
            pending_move: None,
            moves: vec![],
            move_serial: Default::default(),
            perf: Default::default(),
            anim_ticks: HashMap::new(),
            regions: HashMap::new(),
            kept: HashMap::new(),
            region_views: HashMap::new(),
            region_deps: HashMap::new(),
            regions_placed: HashSet::new(),
            live_regions: HashSet::new(),
            refresh_regions: true,
            refresh_now: true,
            settings_rev: 0,
            component_uses: HashMap::new(),
            ctx_generation: Default::default(),
            hash_memo: Default::default(),
            anim_owner: HashMap::new(),
            region_debug: HashMap::new(),
            in_row: false,
            list_generations: HashMap::new(),
            anim_epoch: std::time::Instant::now(),
            lists: HashMap::new(),
            virtual_lists: HashMap::new(),
            // The log carries over a restart, so what happened just before one can still be read.
            activity: std::fs::read_to_string(settings::dir().join("activity.log")).map(|t| t.lines().map(str::to_string).collect()).unwrap_or_default(),
            clickables: vec![],
            menus: vec![],
            clickables_last: vec![],
            menus_last: vec![],
            scripts: None,
            script_data: Map::new(),
            script_timers: HashMap::new(),
            ctx_rev: 0,
            ctx_cache: None,
            ctx_data: Default::default(),
            chat_json_memo: Default::default(),
            sidebar_order_written: Default::default(),
            building_current: Default::default(),
            chat_index: Default::default(),
            worktree_index: Default::default(),
            orchestrator_memo: Default::default(),
            focused: true,
            pending_events: vec![("app.start".into(), Map::new())],
            pending_inbox_reads: HashMap::new(),
            pending_requests: vec![],
            pending_script: vec![],
            script_generation: 0,
            notify_actions: HashMap::new(),
            overlay: Default::default(),
            settings_base: None,
            procs: HashMap::new(),
            fs_watchers: HashMap::new(),
            window_size: (0., 0.),
            composer_texts: HashMap::new(),
            recall: HashMap::new(),
            palette_view: None,
            settings_sheet: false,
            seen_focus: None,
            seen_select: None,
            seen_usage_level: None,
            menu_sig: 0,
            shown_rev: u64::MAX,
            window_dirty: false,
            hosts_dirty: false,
            notify_count: 0,
            badge: None,
            dock_icon: None,
            diff_collapsed: HashSet::new(),
            diff_geom: Default::default(),
            diff_scroll: HashMap::new(),
            focus: cx.focus_handle(),
            _tasks: [watch, env_push, designer_listen, usage_loop, update_loop].into_iter().chain(clicks).collect(),
            _subscriptions: vec![],
            live_md: HashMap::new(),
            spelling: HashMap::new(),
            spell_ignored: HashSet::new(),
        };
        let mut ws = ws;
        let me = cx.entity().downgrade();
        ws._subscriptions.push(cx.intercept_keystrokes(move |e, window, cx| {
            let k = &e.keystroke;
            let key = match (k.key.as_str(), k.modifiers.modified(), k.modifiers.shift && !k.modifiers.platform && !k.modifiers.control && !k.modifiers.alt) {
                ("backspace" | "left" | "right", false, _) => k.key.as_str(),
                ("enter", _, true) => "newline",
                ("tab", false, _) => "tab",
                ("tab", _, true) => "outdent",
                _ => return,
            };
            if me.update(cx, |ws, cx| ws.composer_key(key, window, cx)).unwrap_or(false) {
                cx.stop_propagation();
            }
        }));
        ws.settings_val = ws.settings_context();
        ws.sync_hosts(window, cx);
        ws.configure_motion();
        ws.load_scripts();
        ws.update_dock_icon();
        ws.watch_window(window, cx);
        ws
    }

    /** Connects to every host settings name, and drops connections they no longer do (settings
    `hosts.<name>.connection`, `connection`, `local`). Changing a host's connection reconnects it. */
    pub(crate) fn sync_hosts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.path_env();
        let specs = crate::behavior::host_specs(&self.settings.raw);
        let wanted: HashSet<String> = specs.iter().map(|s| s.name.clone()).collect();
        let gone: Vec<String> = self.conns.keys().filter(|h| !wanted.contains(*h)).cloned().collect();
        for h in gone {
            self.conns.remove(&h);
            self.remotes.remove(&h);
            self.host_up.remove(&h);
            self.down_since.remove(&h);
            self.models.remove(&h);
        }
        for spec in specs {
            if self.conns.get(&spec.name).is_some_and(|c| c.spec == spec && c.path == path) {
                continue;
            }
            let host = spec.name.clone();
            let (r, incoming, guard) = Remote::start(spec.clone(), path.clone());
            let name = host.clone();
            let listen = cx.spawn_in(window, async move |this, cx| {
                while let Ok(msg) = incoming.recv_async().await {
                    // Terminal output goes straight to its terminal: the workspace only reads which one
                    // (an update of it would draw it again).
                    if let Incoming::Event(Event::TermOutput { term_id, data }) = &msg {
                        let Ok(t) = this.read_with(cx, |ws, _| ws.term_by_id.get(&(name.clone(), *term_id)).cloned()) else { break };
                        if let Some(t) = t {
                            let _ = cx.update(|_, cx| t.update(cx, |t, cx| t.output(data, cx)));
                        }
                        continue;
                    }
                    if this.update_in(cx, |ws, window, cx| ws.on_incoming(&name, msg, window, cx)).is_err() {
                        break;
                    }
                }
            });
            if self.host_up.insert(host.clone(), false) == Some(true) {
                self.went_down(&host, cx);
            }
            if !self.models.contains_key(&host) {
                if let Some((m, at)) = load_cached_model(&host) {
                    // Last seen before this run: offline at once, not after the grace.
                    self.down_since.insert(host.clone(), std::time::Instant::now() - Duration::from_secs(3600));
                    self.last_seen.insert(host.clone(), at);
                    self.models.insert(host.clone(), m);
                }
            }
            self.down_since.entry(host.clone()).or_insert_with(std::time::Instant::now);
            self.remotes.insert(host.clone(), r);
            self.conns.insert(host, HostConn { spec, path: path.clone(), _guard: guard, _listen: listen });
        }
        let default = self.default_host();
        self.remote = self.remotes.get(&default).or_else(|| self.remotes.values().next()).cloned().unwrap_or_else(Remote::detached);
        self.connected = self.host_up.values().any(|u| *u);
        self.merge_models();
    }

    /** Window-level behavior from settings `window`: focus and blur events, the remembered frame,
    following the system's appearance, and what closing the window does. */
    fn watch_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let activation = cx.observe_window_activation(window, |ws, window, cx| {
            let now = window.is_window_active();
            if ws.seen_focus != Some(now) {
                ws.seen_focus = Some(now);
                ws.focused = now;
                ws.ctx_rev += 1;
                ws.pending_events.push((if now { "app.focus" } else { "app.blur" }.into(), Map::new()));
                cx.notify();
            }
        });
        let bounds = cx.observe_window_bounds(window, |ws, window, _| {
            if ws.settings.get("window.remember_frame").and_then(Value::as_bool) == Some(false) {
                return;
            }
            let b = window.bounds();
            let frame = json!({"x": f32::from(b.origin.x), "y": f32::from(b.origin.y), "width": f32::from(b.size.width), "height": f32::from(b.size.height), "fullscreen": window.is_fullscreen()});
            let _ = std::fs::write(settings::dir().join("window.json"), frame.to_string());
        });
        let appearance = cx.observe_window_appearance(window, |ws, window, cx| {
            if ws.settings.get("window.appearance").and_then(Value::as_str) == Some("system") {
                apply_appearance(&ws.settings, window.appearance(), cx);
                ws.ctx_rev += 1;
                cx.notify();
            }
        });
        self._subscriptions.extend([activation, bounds, appearance]);
        let view = cx.entity().downgrade();
        window.on_window_should_close(cx, move |_, cx| {
            let what = view.upgrade().map(|v| v.read(cx).settings.text_at("window.on_close", "close")).unwrap_or_else(|| "close".into());
            match what.as_str() {
                "hide" => {
                    cx.hide();
                    false
                }
                "quit" => {
                    cx.quit();
                    true
                }
                "keep" => false,
                _ => true,
            }
        });
    }

    /** Settings `window` again after a reload: the title and the component kit's appearance. */
    fn apply_window_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let title = self.settings.text_at("window.title", "a.box");
        window.set_window_title(&title);
        apply_appearance(&self.settings, window.appearance(), cx);
    }

    /** (Re)starts the scripts: fresh instances, so edits take effect at once and old timers stop; scripts that
    changed are compiled in the background (`flush` starts them) and start when built. */
    pub(crate) fn load_scripts(&mut self) {
        self.script_timers.clear();
        self.procs.drain().for_each(|(_, p)| kill_pid(p.pid));
        self.fs_watchers.clear();
        // `scripts_options.keep_data`: what scripts published (`.data`) stays until they publish it again.
        if self.settings.get("scripts_options.keep_data").and_then(Value::as_bool) == Some(false) {
            self.script_data.clear();
        }
        let had_overlay = !self.overlay.patches.is_empty() || !self.overlay.keys.is_empty() || !self.overlay.commands.is_empty();
        self.overlay = ScriptOverlay::default();
        if had_overlay {
            self.apply_overlay();
        }
        self.script_generation += 1;
        self.scripts = crate::script::Scripts::load(&self.settings.scripts, self.script_options(), self.script_generation);
        self.pending_script.push(json!({"type": "event", "name": "start", "payload": null}));
    }

    /** The settings with what scripts added at runtime on top (`ab.patch`, `ab.key`, `ab.command`). */
    fn apply_overlay(&mut self) {
        let base = self.settings_base.get_or_insert_with(|| (self.settings.raw.clone(), self.settings.warnings.clone())).clone();
        let mut raw = base.0;
        let mut warnings = crate::ui::patch::apply(&mut raw, &self.overlay.patches);
        if let Some(Value::Array(k)) = raw.get_mut("keys") {
            k.extend(self.overlay.keys.iter().cloned());
        }
        if let Some(Value::Array(c)) = raw.get_mut("commands") {
            c.extend(self.overlay.commands.iter().cloned());
        }
        match settings::from_raw(raw) {
            Ok(mut s) => {
                let mut w = base.1;
                w.extend(warnings.drain(..).map(|x| format!("ab.patch: {x}")));
                s.warnings = w;
                self.settings = std::rc::Rc::new(s);
                self.settings_rev += 1;
                self.settings_val = self.settings_context();
                self.ctx_rev += 1;
                self.pending_motion = true;
            }
            Err(e) => self.exprs.note(format!("ab.patch: {e:#}")),
        }
    }

    /** Runs queued script messages, events, requests, and actions now, so the app keeps working while its
    window is hidden and not rendering (agents drive it then too). */
    pub(crate) fn flush(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.spawn_script_builds(window, cx);
        if std::mem::take(&mut self.hosts_dirty) {
            self.sync_hosts(window, cx);
        }
        if std::mem::take(&mut self.window_dirty) {
            self.apply_window_settings(window, cx);
        }
        let idle = |ws: &Self| ws.pending_script.is_empty() && ws.pending_events.is_empty() && ws.pending_actions.is_empty() && ws.pending_requests.is_empty();
        if idle(self) {
            return;
        }
        let mut asked = false;
        for _ in 0..8 {
            if idle(self) {
                break;
            }
            asked |= self.pending_requests.iter().any(|(r, _)| r.action.get("do").and_then(Value::as_str) == Some("inspect.refresh"));
            for m in std::mem::take(&mut self.pending_script) {
                self.script_deliver(m, window, cx);
            }
            for (e, extra) in std::mem::take(&mut self.pending_events) {
                let extra = extra.into_iter().map(|(k, v)| (k, to_val(&v))).collect();
                self.fire_event(&e, extra, window, cx);
            }
            for (req, from) in std::mem::take(&mut self.pending_requests) {
                self.run_request(req, from, window, cx);
            }
            for a in std::mem::take(&mut self.pending_actions) {
                self.run_action(&a, window, cx);
            }
        }
        // `abx state` reads inspect.json right after asking for it: written now, not on its schedule.
        if asked {
            let ctx = self.context();
            self.refresh_shown(&ctx, cx);
            self.inspect_at = None;
            self.write_inspect(&ctx, window, cx);
        }
    }

    /** The context as JSON for scripts, reused until something changes it (`ctx_rev`) or 250ms pass. */
    fn script_context(&mut self) -> std::rc::Rc<str> {
        if let Some((rev, at, text)) = &self.ctx_cache {
            if *rev == self.ctx_rev && (at.elapsed().as_millis() as f64) < self.settings.num("scripts_options.context_cache_ms", 250.) {
                return text.clone();
            }
        }
        let mut ctx = crate::ui::expr::to_json(&self.context());
        // Where every node is drawn is big and changes every frame; scripts get it from ab.inspect.
        if let Some(Value::Object(app)) = ctx.get_mut("app") {
            app.remove("layout");
        }
        let text: std::rc::Rc<str> = std::rc::Rc::from(ctx.to_string());
        self.ctx_cache = Some((self.ctx_rev, std::time::Instant::now(), text.clone()));
        text
    }

    pub(crate) fn script_deliver(&mut self, message: Value, window: &mut Window, cx: &mut Context<Self>) {
        if !self.scripts.as_ref().is_some_and(|s| s.any()) {
            return;
        }
        let ctx = self.script_context();
        let Some(scripts) = self.scripts.as_mut() else { return };
        let outs = scripts.deliver(message, ctx);
        for m in outs {
            self.script_out(m, window, cx);
        }
        cx.notify();
    }

    fn script_out(&mut self, m: Value, window: &mut Window, cx: &mut Context<Self>) {
        let id = m["id"].as_u64().unwrap_or(0);
        match m["type"].as_str().unwrap_or("") {
            "do" => self.run_action(&m["action"], window, cx),
            "data" => {
                self.ctx_rev += 1;
                if let Some(n) = m["name"].as_str() {
                    self.script_data.insert(n.to_string(), m["value"].clone());
                }
            }
            "timer" => {
                let ms = m["ms"].as_u64().unwrap_or(1000).max(self.settings.num("scripts_options.min_timer_ms", 16.) as u64);
                let repeat = m["repeat"].as_bool().unwrap_or(false);
                let task = cx.spawn_in(window, async move |this, cx| loop {
                    cx.background_executor().timer(Duration::from_millis(ms)).await;
                    let alive = this.update_in(cx, |ws, window, cx| ws.script_deliver(json!({"type": "timer", "id": id, "repeat": repeat}), window, cx));
                    if alive.is_err() || !repeat {
                        break;
                    }
                });
                self.script_timers.insert(id, task);
            }
            "cancel" => {
                self.script_timers.remove(&id);
            }
            "shell" => {
                let env: Vec<(String, String)> = m["env"].as_object().map(|e| e.iter().map(|(k, v)| (k.clone(), crate::ui::render::value_text(v))).collect()).unwrap_or_default();
                let job = ShellJob {
                    run: m["cmd"].as_str().unwrap_or("").to_string(),
                    cwd: m["cwd"].as_str().map(str::to_string),
                    host: m["host"].as_str().map(str::to_string),
                    timeout: m["timeout"].as_f64(),
                    env,
                };
                self.start_shell(job, window, cx, move |ws, result, window, cx| ws.script_deliver(json!({"type": "reply", "id": id, "result": result}), window, cx));
            }
            "box" => {
                let call = serde_json::from_value::<Call>(json!({"method": m["method"], "params": m["params"]}));
                let remote = match (m["host"].as_str(), &call) {
                    (Some(h), _) => self.remote_for_host(h),
                    (None, Ok(c)) => self.route(c),
                    _ => self.remote.clone(),
                };
                cx.spawn_in(window, async move |this, cx| {
                    let (result, error) = match call {
                        Ok(call) => match remote.call(call).await {
                            Ok(v) => (v, Value::Null),
                            Err(e) => (Value::Null, Value::String(e.to_string())),
                        },
                        Err(e) => (Value::Null, Value::String(format!("unknown box call: {e}"))),
                    };
                    this.update_in(cx, |ws, window, cx| ws.script_deliver(json!({"type": "reply", "id": id, "result": result, "error": error}), window, cx)).ok();
                })
                .detach();
            }
            "write" => {
                let path = crate::script::expand(m["path"].as_str().unwrap_or(""));
                if let Some(parent) = std::path::Path::new(&path).parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let error = std::fs::write(&path, m["text"].as_str().unwrap_or("")).err().map(|e| format!("{path}: {e}"));
                if let Some(e) = &error {
                    self.exprs.note(format!("ab.write: {e}"));
                }
                self.pending_script.push(json!({"type": "reply", "id": id, "result": error.is_none(), "error": error}));
            }
            "emit" => {
                let name = m["event"].as_str().unwrap_or("").to_string();
                let payload = to_val(&m["payload"]);
                self.fire_event_as(&name, vec![("payload".into(), payload)], Some(m["payload"].clone()), window, cx);
            }
            "call" => {
                let name = m["name"].as_str().unwrap_or("").to_string();
                self.script_call(&name, m["args"].clone(), id, window, cx);
            }
            "patch" => {
                self.overlay.patches.extend(m["ops"].as_array().cloned().unwrap_or_default());
                self.apply_overlay();
            }
            "key" | "command" => {
                let item = m[m["type"].as_str().unwrap_or("")].clone();
                if m["type"] == "key" { self.overlay.keys.push(item) } else { self.overlay.commands.push(item) }
                self.apply_overlay();
            }
            "log" | "error" => {
                use std::io::Write as _;
                let text = m["text"].as_str().unwrap_or("").to_string();
                if m["type"] == "error" {
                    if let Some(s) = self.scripts.as_mut() {
                        s.errors.push(text.clone());
                    }
                }
                if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(settings::dir().join("scripts.log")) {
                    let _ = writeln!(f, "{text}");
                }
            }
            other => self.exprs.note(format!("script: unknown message type `{other}`")),
        }
    }

    pub(crate) fn script_timer_ids(&self) -> Vec<u64> {
        let mut ids: Vec<u64> = self.script_timers.keys().copied().collect();
        ids.sort();
        ids
    }

    pub(crate) fn script_proc_ids(&self) -> Vec<Value> {
        self.procs.iter().map(|(id, p)| json!({"id": id, "pid": p.pid})).collect()
    }

    /** `ab.call(name, args)`: the abilities scripts reach by name (see `script::CALLS`); the answer goes
    back as a reply to `id`. */
    fn script_call(&mut self, name: &str, args: Value, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let reply = |ws: &mut Workspace, result: Value, error: Option<String>| ws.pending_script.push(json!({"type": "reply", "id": id, "result": result, "error": error}));
        match name {
            "http" => {
                let path = self.path_env();
                cx.spawn_in(window, async move |this, cx| {
                    let result = cx.background_spawn(async move { crate::script::http(&args, &path) }).await;
                    let error = result["error"].as_str().map(str::to_string);
                    this.update_in(cx, |ws, window, cx| ws.script_deliver(json!({"type": "reply", "id": id, "result": result, "error": error}), window, cx)).ok();
                })
                .detach();
            }
            "clipboard.read" => {
                let text = cx.read_from_clipboard().and_then(|c| c.text());
                reply(self, json!({"text": text}), None);
            }
            "clipboard.write" => {
                cx.write_to_clipboard(ClipboardItem::new_string(crate::ui::render::value_text(&args["text"])));
                reply(self, Value::Bool(true), None);
            }
            "inspect" => {
                let v = self.inspect_doc(window, cx);
                reply(self, v, None);
            }
            "fs.watch" => {
                use notify::{RecursiveMode, Watcher};
                let watch_id = args["watch"].as_u64().unwrap_or(id);
                let path = crate::script::expand(args["path"].as_str().unwrap_or(""));
                let (tx, rx) = flume::unbounded::<Vec<String>>();
                let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                    if let Ok(ev) = res {
                        let _ = tx.send(ev.paths.iter().map(|p| p.to_string_lossy().into_owned()).collect());
                    }
                });
                match watcher.and_then(|mut w| w.watch(std::path::Path::new(&path), RecursiveMode::Recursive).map(|_| w)) {
                    Ok(w) => {
                        self.fs_watchers.insert(watch_id, w);
                        cx.spawn_in(window, async move |this, cx| {
                            while let Ok(paths) = rx.recv_async().await {
                                let msg = json!({"type": "proc", "proc": watch_id, "stream": "change", "data": paths});
                                if this.update_in(cx, |ws, window, cx| ws.script_deliver(msg, window, cx)).is_err() {
                                    break;
                                }
                            }
                        })
                        .detach();
                        reply(self, Value::Bool(true), None);
                    }
                    Err(e) => reply(self, Value::Null, Some(format!("fs.watch {path}: {e}"))),
                }
            }
            "proc.spawn" => self.spawn_proc(args, id, window, cx),
            "proc.write" => {
                let proc_id = args["proc"].as_u64().unwrap_or(0);
                let data = crate::ui::render::value_text(&args["data"]);
                let ok = self.procs.get_mut(&proc_id).and_then(|p| p.stdin.as_mut()).map(|s| { use std::io::Write as _; s.write_all(data.as_bytes()).and_then(|_| s.flush()) });
                match ok {
                    Some(Ok(())) => reply(self, Value::Bool(true), None),
                    Some(Err(e)) => reply(self, Value::Null, Some(e.to_string())),
                    None => reply(self, Value::Null, Some(format!("no process {proc_id}"))),
                }
            }
            "proc.kill" => {
                let proc_id = args["proc"].as_u64().unwrap_or(0);
                match self.procs.get(&proc_id).map(|p| p.pid) {
                    Some(pid) => {
                        kill_pid(pid);
                        reply(self, Value::Bool(true), None)
                    }
                    None => reply(self, Value::Null, Some(format!("no process {proc_id}"))),
                }
            }
            other => match crate::script::pure_call(other, &args) {
                Some(Ok(v)) => reply(self, v, None),
                Some(Err(e)) => reply(self, Value::Null, Some(e)),
                None => {
                    self.exprs.note(format!("ab.call: unknown call `{other}`"));
                    reply(self, Value::Null, Some(format!("unknown call `{other}`")));
                }
            },
        }
    }

    /** `ab.spawn`: a local process whose stdout and stderr lines (and exit code) go to the script as
    they come. A list `cmd` runs directly; a string runs in /bin/sh. */
    fn spawn_proc(&mut self, args: Value, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        use std::io::BufRead as _;
        use std::process::Stdio;
        let proc_id = args["proc"].as_u64().unwrap_or(id);
        let mut cmd = match &args["cmd"] {
            Value::Array(a) if !a.is_empty() => {
                let parts: Vec<String> = a.iter().map(crate::ui::render::value_text).collect();
                let mut c = std::process::Command::new(&parts[0]);
                c.args(&parts[1..]);
                c
            }
            other => {
                let mut c = std::process::Command::new("/bin/sh");
                c.arg("-c").arg(crate::ui::render::value_text(other));
                c
            }
        };
        cmd.env("PATH", self.path_env());
        if let Some(cwd) = args["cwd"].as_str() {
            cmd.current_dir(crate::script::expand(cwd));
        }
        if let Some(env) = args["env"].as_object() {
            cmd.envs(env.iter().map(|(k, v)| (k.clone(), crate::ui::render::value_text(v))));
        }
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                self.pending_script.push(json!({"type": "reply", "id": id, "result": null, "error": e.to_string()}));
                self.pending_script.push(json!({"type": "proc", "proc": proc_id, "exit": -1}));
                return;
            }
        };
        let mut stdin = child.stdin.take();
        if let (Some(text), Some(s)) = (args["stdin"].as_str(), stdin.as_mut()) {
            use std::io::Write as _;
            let _ = s.write_all(text.as_bytes());
        }
        let (tx, rx) = flume::unbounded::<Value>();
        let mut readers = vec![];
        for (stream, pipe) in [("stdout", child.stdout.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>)), ("stderr", child.stderr.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>))] {
            let (Some(pipe), tx) = (pipe, tx.clone()) else { continue };
            readers.push(std::thread::spawn(move || {
                for line in std::io::BufReader::new(pipe).lines().map_while(Result::ok) {
                    let _ = tx.send(json!({"type": "proc", "proc": proc_id, "stream": stream, "data": line}));
                }
            }));
        }
        let pid = child.id();
        std::thread::spawn(move || {
            let code = child.wait().ok().and_then(|s| s.code()).unwrap_or(-1);
            // `exit` comes after the last line.
            for r in readers {
                let _ = r.join();
            }
            let _ = tx.send(json!({"type": "proc", "proc": proc_id, "exit": code}));
        });
        self.procs.insert(proc_id, Proc { stdin, pid });
        self.pending_script.push(json!({"type": "reply", "id": id, "result": {"pid": pid}, "error": null}));
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(msg) = rx.recv_async().await {
                let done = msg.get("exit").is_some();
                let alive = this.update_in(cx, |ws, window, cx| {
                    // Scripts that reloaded since don't hear from their old processes.
                    let known = if done { ws.procs.remove(&proc_id).is_some() } else { ws.procs.contains_key(&proc_id) };
                    if known {
                        ws.script_deliver(msg, window, cx)
                    }
                });
                if alive.is_err() || done {
                    break;
                }
            }
        })
        .detach();
    }

    /** A shell command on the Mac, or with `host` set to another machine, on its daemon (bash -lc there);
    `done` gets `{code, stdout, stderr}`. */
    pub(crate) fn start_shell(&mut self, job: ShellJob, window: &mut Window, cx: &mut Context<Self>, done: impl FnOnce(&mut Workspace, Value, &mut Window, &mut Context<Workspace>) + 'static) {
        let remote_host = job.host.clone().filter(|h| !self.is_local_host(h));
        if let Some(host) = remote_host {
            let exports: String = job.env.iter().map(|(k, v)| format!("export {k}={}; ", crate::behavior::sh_quote(v))).collect();
            let call = Call::Exec { cmd: format!("{exports}{}", job.run), cwd: job.cwd.clone(), timeout_secs: job.timeout.map(|t| t.ceil() as u64) };
            let remote = self.remote_for_host(&host);
            cx.spawn_in(window, async move |this, cx| {
                let result = match remote.call(call).await {
                    Ok(v) => json!({"code": v["code"], "stdout": v["stdout"], "stderr": v["stderr"]}),
                    Err(e) => json!({"code": -1, "stdout": "", "stderr": e.to_string()}),
                };
                this.update_in(cx, |ws, window, cx| done(ws, result, window, cx)).ok();
            })
            .detach();
            return;
        }
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.arg("-c").arg(&job.run).envs(job.env.clone()).env("PATH", self.path_env());
        if let Some(cwd) = &job.cwd {
            cmd.current_dir(crate::script::expand(cwd));
        }
        let timeout = job.timeout.map(Duration::from_secs_f64);
        cx.spawn_in(window, async move |this, cx| {
            let out = cx.background_spawn(async move { crate::script::run_output(cmd, None, timeout) }).await;
            let result = match out {
                Ok(o) => json!({"code": o.status.code(), "stdout": String::from_utf8_lossy(&o.stdout), "stderr": String::from_utf8_lossy(&o.stderr)}),
                Err(e) => json!({"code": -1, "stdout": "", "stderr": e.to_string()}),
            };
            this.update_in(cx, |ws, window, cx| done(ws, result, window, cx)).ok();
        })
        .detach();
    }

    fn configure_motion(&self) {
        let m = &self.settings.motion;
        let mut store = self.motion.borrow_mut();
        store.enabled = m.get("enabled").and_then(Value::as_bool) != Some(false) && self.settings.theme.get("animations").and_then(Value::as_bool) != Some(false);
        store.duration_ms = m.get("move").and_then(|x| x.get("duration")).and_then(Value::as_f64).unwrap_or(280.) as f32;
        store.easing = crate::ui::motion::Easing::parse(m.get("move").and_then(|x| x.get("easing")).and_then(Value::as_str)).with_spring(m.get("move").and_then(|x| x.get("spring")));
    }

    fn on_designer(&mut self, out: crate::designer::Out, cx: &mut Context<Self>) {
        use crate::designer::{Out, CHAT_ID};
        let t = self.transcripts.entry(CHAT_ID.into()).or_default();
        match out {
            Out::Entry(e) => {
                let from = t.lines;
                t.append(from, &[e]);
            }
            Out::Done { error } => {
                self.designer.working = false;
                self.pending_events.push(("designer.finished".into(), Map::new()));
                if let Some(text) = error {
                    let from = t.lines;
                    t.append(from, &[json!({"type": "system", "level": "error", "content": text})]);
                }
            }
            Out::Exited => self.designer.reset_process(),
        }
        cx.notify();
    }

    pub(crate) fn designer_send(&mut self, text: String, cx: &mut Context<Self>) {
        use crate::designer::CHAT_ID;
        crate::designer::snapshot();
        let t = self.transcripts.entry(CHAT_ID.into()).or_default();
        let from = t.lines;
        t.append(from, &[json!({"type": "user", "message": {"role": "user", "content": text}})]);
        if let Err(e) = self.designer.send(&text, self.designer_tx.clone()) {
            let t = self.transcripts.entry(CHAT_ID.into()).or_default();
            let from = t.lines;
            t.append(from, &[json!({"type": "system", "level": "error", "content": e})]);
        }
        self.jump_to_latest(CHAT_ID);
        cx.notify();
    }

    pub(crate) fn designer_reset(&mut self, cx: &mut Context<Self>) {
        self.designer.reset();
        self.transcripts.remove(crate::designer::CHAT_ID);
        self.lists.remove(crate::designer::CHAT_ID);
        self.designer_sessions = crate::designer::sessions();
        self.ctx_rev += 1;
        cx.notify();
    }

    /** Switch the designer panel to an earlier conversation (`designer.open`). */
    pub(crate) fn designer_open(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.designer.working || crate::designer::current().as_deref() == Some(id) {
            return;
        }
        self.designer.switch_to(id);
        let mut t = Transcript::default();
        t.append(0, &crate::designer::past_entries());
        self.transcripts.insert(crate::designer::CHAT_ID.into(), t);
        // The panel's rows were laid out for the other conversation; a new list lays these out from scratch.
        self.lists.remove(crate::designer::CHAT_ID);
        self.designer_sessions = crate::designer::sessions();
        self.ctx_rev += 1;
        cx.notify();
    }

    fn watch_settings(window: &mut Window, cx: &mut Context<Self>) -> Task<()> {
        use notify::{RecursiveMode, Watcher};
        let (tx, rx) = flume::unbounded();
        let name = settings::path().file_name().map(|n| n.to_owned());
        let dir = settings::dir();
        let defaults = settings::live_defaults_path();
        let defaults_name = defaults.as_ref().and_then(|p| p.file_name()).map(|n| n.to_owned());
        let defaults_dir = defaults.as_ref().and_then(|p| p.parent()).map(|p| p.to_path_buf());
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res {
                for p in &ev.paths {
                    if p.file_name().map(|n| n.to_owned()) == name || (defaults_name.is_some() && p.file_name().map(|n| n.to_owned()) == defaults_name) {
                        let _ = tx.send(false);
                    } else if p.extension().is_some_and(|e| e == "svg") {
                        let _ = tx.send(false);
                    } else if p.extension().is_some_and(|e| e == "rs") && p.parent().is_some_and(|d| d.ends_with("scripts")) {
                        let _ = tx.send(false);
                    } else if p.file_name().and_then(|n| n.to_str()) == Some("run.json") || (p.parent().is_some_and(|d| d.ends_with("run.d")) && p.extension().is_some_and(|e| e == "json")) {
                        let _ = tx.send(true);
                    }
                }
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(mut watcher) = watcher else { return };
            let _ = std::fs::create_dir_all(&dir);
            if watcher.watch(&dir, RecursiveMode::NonRecursive).is_err() {
                return;
            }
            if let Some(d) = &defaults_dir {
                let _ = watcher.watch(d, RecursiveMode::NonRecursive);
            }
            let scripts = crate::script::scripts_dir();
            let _ = std::fs::create_dir_all(&scripts);
            let _ = watcher.watch(&scripts, RecursiveMode::NonRecursive);
            let inbox = dir.join("run.d");
            let _ = std::fs::create_dir_all(&inbox);
            let _ = watcher.watch(&inbox, RecursiveMode::NonRecursive);
            // Requests written while the app wasn't running.
            let _ = this.update_in(cx, |ws, window, cx| {
                ws.run_inbox(cx);
                ws.flush(window, cx);
            });
            while let Ok(first) = rx.recv_async().await {
                let wait = this.update(cx, |ws, _| ws.settings.num("settings_options.reload_debounce_ms", 80.)).unwrap_or(80.);
                cx.background_executor().timer(Duration::from_millis(wait as u64)).await;
                let (mut reload, mut run) = (!first, first);
                while let Ok(r) = rx.try_recv() {
                    reload |= !r;
                    run |= r;
                }
                let ok = this.update_in(cx, |ws, window, cx| {
                    if reload {
                        ws.reload_settings(cx);
                    }
                    if run {
                        ws.run_inbox(cx);
                    }
                    ws.flush(window, cx);
                });
                if ok.is_err() {
                    break;
                }
            }
            drop(watcher);
        })
    }

    /** Agents' requests in the settings folder: `run.json`, and each `run.d/<id>.json` (oldest first).
    Each runs once and its file is removed; a request with an id gets `results/<id>.json`. */
    fn run_inbox(&mut self, cx: &mut Context<Self>) {
        let dir = settings::dir();
        let mut files: Vec<(std::path::PathBuf, Option<String>)> = vec![];
        let p = dir.join("run.json");
        if p.exists() {
            files.push((p, None));
        }
        let mut queued: Vec<(std::time::SystemTime, std::path::PathBuf)> = std::fs::read_dir(dir.join("run.d"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "json") && !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')))
            .map(|p| (std::fs::metadata(&p).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH), p))
            .collect();
        queued.sort();
        for (_, p) in queued {
            let id = p.file_stem().map(|s| s.to_string_lossy().into_owned());
            files.push((p, id));
        }
        for (p, id) in files {
            let Ok(text) = std::fs::read_to_string(&p) else { continue };
            let _ = std::fs::remove_file(&p);
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            match serde_json_lenient::from_str::<Value>(&text) {
                Ok(v) => self.pending_requests.push((crate::behavior::parse_request(id.as_deref(), v), None)),
                Err(e) => {
                    self.exprs.note(format!("{name}: {e}"));
                    if let Some(id) = id {
                        write_result(&id, &json!({"ok": false, "error": format!("{name}: {e}"), "errors": [], "value": null}));
                    }
                }
            }
        }
        cx.notify();
    }

    /** Runs an agent's request; with an id, writes what happened to `results/<id>.json` (on the machine
    it came from): `{ok, error, errors, value}` (value: its `read` expression after the action). */
    fn run_request(&mut self, req: crate::behavior::Request, from: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let prior = self.exprs.capture.borrow_mut().replace(vec![]);
        self.run_action(&req.action, window, cx);
        let errors = std::mem::replace(&mut *self.exprs.capture.borrow_mut(), prior).unwrap_or_default();
        let Some(id) = req.id.as_deref().and_then(crate::behavior::safe_id) else { return };
        let value = req.read.as_deref().map(|code| crate::ui::expr::to_json(&self.exprs.first(code, &self.context()))).unwrap_or(Value::Null);
        let result = json!({"id": id, "ok": errors.is_empty(), "error": errors.first(), "errors": errors, "value": value});
        self.deliver_result(&id, &result, from);
    }

    fn deliver_result(&self, id: &str, result: &Value, from: Option<String>) {
        match from.filter(|h| !self.is_local_host(h)) {
            None => write_result(id, result),
            Some(host) => {
                // Back on the machine whose agent asked, where its `abx do --wait` reads it.
                use base64::Engine as _;
                let data = base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec_pretty(result).unwrap_or_default());
                let cmd = format!("d=\"$HOME/.agent-box/state/settings/results\"; mkdir -p \"$d\" && printf %s {} | base64 -d > \"$d/.{id}.json\" && mv \"$d/.{id}.json\" \"$d/{id}.json\"", crate::behavior::sh_quote(&data));
                self.remote_for_host(&host).send(Call::Exec { cmd, cwd: None, timeout_secs: Some(15) });
            }
        }
    }

    pub(crate) fn reload_settings(&mut self, cx: &mut Context<Self>) {
        crate::ui::docs::write_reference();
        self.settings_rev += 1;
        match settings::load() {
            Ok(s) => {
                self.state = s.state.clone();
                self.state.extend(self.user_state.clone());
                self.settings = std::rc::Rc::new(s);
                self.settings_base = None;
                self.settings_val = self.settings_context();
                crate::transcript::set_options(&self.settings.chat);
                set_upload_options(&self.settings);
                crate::designer::configure(&self.settings.designer, &self.path_env());
                self.configure_usage();
                self.menu_sig = 0;
                self.shown_rev = u64::MAX;
                self.hosts_dirty = true;
                self.window_dirty = true;
                self.pending_events.push(("settings.reloaded".into(), Map::new()));
                self.settings_error = None;
                self.exprs = Exprs::default();
                self.configure_motion();
                self.load_scripts();
                for l in self.lists.values() {
                    l.state.remeasure();
                }
                self.pending_motion = true;
                crate::logo::clear_cache();
                self.update_dock_icon();
                let ts = self.settings.terminal.clone();
                for t in self.terminals.values() {
                    t.update(cx, |t, _| t.apply_settings(&ts));
                }
                self.pending_script.push(json!({"type": "event", "name": "model", "payload": null}));
                let up: Vec<String> = self.host_up.iter().filter(|(_, u)| **u).map(|(h, _)| h.clone()).collect();
                for h in up {
                    self.configure_host(&h);
                }
            }
            Err(e) => self.settings_error = Some(format!("{e:#}")),
        }
        cx.notify();
    }

    /** Runs `events` from settings for chat state changes, named by `events_options.transitions`
    (chat.waiting, chat.finished, chat.stopped by default) plus chat.state for any change. The chat is
    `.chat`, its old state `.from`. A turn the agent started on its own finishes as chat.background_finished,
    which notifies and sounds nothing by default. */
    fn fire_chat_events(&mut self, before: &HashMap<String, String>, window: &mut Window, cx: &mut Context<Self>) {
        let rules = self.settings.get("events_options.transitions").cloned();
        let mut fired = vec![];
        for c in &self.model.chats {
            let Some(old) = before.get(&c.id) else { continue };
            if old != &c.state {
                fired.push(("chat.state".to_string(), c.clone(), old.clone()));
            }
            for event in crate::behavior::transition_events(rules.as_ref(), old, &c.state) {
                let woke = self.transcripts.get(&c.id).is_some_and(|t| t.woke_quietly());
                let event = if event == "chat.finished" && woke { "chat.background_finished".to_string() } else { event };
                fired.push((event, c.clone(), old.clone()));
            }
        }
        for (event, chat, from) in fired {
            self.fire_event(&event, vec![("chat".into(), to_val(&self.chat_json(&chat))), ("from".into(), to_val(&Value::String(from)))], window, cx);
        }
    }

    /** What appeared, went, or changed in a host's new model: chat.added / chat.removed (`.chat`),
    worktree.added / worktree.removed (`.worktree`), worktree.mr (its merge request or checks changed;
    `.worktree`, `.from`). */
    fn model_events(&mut self, old: &Model, host: &str) {
        let new = self.models.get(host).cloned().unwrap_or_default();
        let ext = |pairs: Vec<(&str, Value)>| -> Map<String, Value> { pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect() };
        let mut events = vec![];
        for c in new.chats.iter().filter(|c| !old.chats.iter().any(|o| o.id == c.id)) {
            events.push(("chat.added", ext(vec![("chat", self.chat_json(c)), ("host", json!(host))])));
        }
        for c in old.chats.iter().filter(|o| !new.chats.iter().any(|c| c.id == o.id)) {
            events.push(("chat.removed", ext(vec![("chat", serde_json::to_value(c).unwrap_or_default()), ("host", json!(host))])));
        }
        let wts = |m: &Model| -> Vec<Worktree> { m.projects.iter().flat_map(|p| p.items.iter()).chain(m.sections.iter().flat_map(|s| s.items.iter())).cloned().collect() };
        let (before, after) = (wts(old), wts(&new));
        for w in &after {
            match before.iter().find(|o| o.path == w.path) {
                None => events.push(("worktree.added", ext(vec![("worktree", serde_json::to_value(w).unwrap_or_default()), ("host", json!(host))]))),
                Some(o) if o.mr != w.mr => events.push(("worktree.mr", ext(vec![("worktree", serde_json::to_value(w).unwrap_or_default()), ("from", serde_json::to_value(&o.mr).unwrap_or_default()), ("host", json!(host))]))),
                _ => {}
            }
        }
        for o in before.iter().filter(|o| !after.iter().any(|w| w.path == o.path)) {
            events.push(("worktree.removed", ext(vec![("worktree", serde_json::to_value(o).unwrap_or_default()), ("host", json!(host))])));
        }
        // New inbox items, and ones an update made unread again.
        for i in new.inbox.iter().filter(|i| !old.inbox.iter().any(|o| o.id == i.id && o.rev == i.rev)) {
            events.push(("inbox.added", ext(vec![("item", inbox_item_json(i, host, &self.age_format()))])));
        }
        self.pending_events.extend(events.into_iter().map(|(e, x)| (e.to_string(), x)));
    }

    /** `.inbox`: every host's unread items as one list, newest first (an item posted under the same key on two
    machines shows once, the newer), and their count. */
    fn inbox_json(&self) -> Value {
        let mut items: Vec<(f64, Value, Option<String>)> = vec![];
        let fmt = self.age_format();
        for (host, m) in &self.models {
            for i in &m.inbox {
                items.push((i.updated_at, inbox_item_json(i, host, &fmt), i.key.clone()));
            }
        }
        items.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut seen = HashSet::new();
        let items: Vec<Value> = items.into_iter().filter(|(_, _, k)| k.as_ref().map_or(true, |k| seen.insert(k.clone()))).map(|(_, v, _)| v).collect();
        json!({"unread": items.len(), "items": items})
    }

    /** `inbox.read`: mark items read on every host that has them (each host keeps its own; a host that is down
    gets it when it reconnects). */
    pub(crate) fn inbox_read(&mut self, uids: Vec<String>, chat: Option<String>, all: bool, cx: &mut Context<Self>) {
        let mut hosts: HashSet<String> = self.models.keys().cloned().collect();
        hosts.extend(self.remotes.keys().cloned());
        for host in hosts {
            let ids: Vec<i64> = uids.iter().filter_map(|u| u.split_once(':')).filter(|(h, _)| *h == host).filter_map(|(_, id)| id.parse().ok()).collect();
            if ids.is_empty() && chat.is_none() && !all {
                continue;
            }
            // Shown read at once; the host's next model confirms it.
            if let Some(m) = self.models.get_mut(&host) {
                m.inbox.retain(|i| !(all || ids.contains(&i.id) || (chat.is_some() && i.chat == chat)));
            }
            let call = Call::InboxRead { ids, chat: chat.clone(), all };
            if self.host_up.get(&host).copied().unwrap_or(false) {
                let remote = self.remote_for_host(&host);
                cx.spawn(async move |_, _| {
                    let _ = remote.call(call).await;
                })
                .detach();
            } else {
                self.pending_inbox_reads.entry(host).or_default().push(call);
            }
        }
        self.ctx_rev += 1;
        let ctx = self.context();
        self.update_badge(&ctx);
        cx.notify();
    }

    /** `inbox.post`: to the inbox host (`daemon.inbox_host`, default the box), else this machine when it is down. */
    pub(crate) fn inbox_post(&mut self, call: Call, cx: &mut Context<Self>) {
        let owner = self.settings.raw.pointer("/daemon/inbox_host").and_then(|v| v.as_str()).unwrap_or("box").to_string();
        let host = if self.host_up.get(&owner).copied().unwrap_or(false) { owner } else { self.local_host() };
        let remote = self.remote_for_host(&host);
        cx.spawn(async move |_, _| {
            let _ = remote.call(call).await;
        })
        .detach();
    }

    /** A banner was clicked: its own `on_click` (notify.system), else the `notification.click` event with the
    chat it was about as `.chat` and the banner's id as `.id`. */
    fn on_notification_click(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let chat_id = id.split('#').next().unwrap_or("").to_string();
        let chat = self.chat(&chat_id).map(|c| self.chat_json(c)).unwrap_or_else(|| json!({"id": chat_id}));
        let extra = vec![("chat".into(), to_val(&chat)), ("id".into(), to_val(&json!(id)))];
        match self.notify_actions.remove(id) {
            Some(action) => {
                let ctx = crate::ui::expr::with(&self.context(), extra);
                let action = self.exprs.resolve_action(&action, &ctx);
                self.run_action(&action, window, cx);
            }
            None => self.fire_event("notification.click", extra, window, cx),
        }
        cx.notify();
    }

    /** Shows a chat wherever it is: selects its worktree with it as the active tab (the designer opens its panel). */
    pub(crate) fn show_chat(&mut self, chat: &str, window: &mut Window, cx: &mut Context<Self>) {
        if chat == crate::designer::CHAT_ID {
            self.set_state("designer_open".into(), Value::Bool(true));
        } else if let Some(path) = self.all_worktrees().iter().find(|w| w.chat_ids.iter().any(|c| c == chat)).map(|w| w.path.clone()) {
            self.active_chat.insert(path.clone(), chat.to_string());
            self.select(path, window, cx);
        }
        cx.notify();
    }

    /** The Dock shows the settings' logo while the app runs (`logo.dock: false` keeps the bundle's icon). */
    pub(crate) fn update_dock_icon(&mut self) {
        let png = crate::logo::dock_spec(&self.settings.logo).and_then(|s| crate::logo::png(&s, &self.settings.theme, 512));
        if png != self.dock_icon {
            #[cfg(target_os = "macos")]
            crate::macos::set_app_icon(png.as_deref());
            self.dock_icon = png;
        }
    }

    fn update_badge(&mut self, ctx: &Val) {
        let label = self.settings.dock.get("badge").map(|t| self.exprs.resolve(t, ctx)).map(|v| value_text(&v)).filter(|s| !s.is_empty() && s != "0");
        if label != self.badge {
            #[cfg(target_os = "macos")]
            crate::macos::set_badge(label.as_deref());
            self.badge = label;
        }
    }

    pub(crate) fn fire_event(&mut self, event: &str, extra: Vec<(String, Val)>, window: &mut Window, cx: &mut Context<Self>) {
        self.fire_event_as(event, extra, None, window, cx)
    }

    /** Runs an event: scripts' `ab.on` handlers get `script_payload` (else the extra fields as an object),
    its sound plays, and every handler in settings `events.<event>` (one `{if, do}` or a list) whose `if`
    holds runs with the extra fields in its context. */
    pub(crate) fn fire_event_as(&mut self, event: &str, extra: Vec<(String, Val)>, script_payload: Option<Value>, window: &mut Window, cx: &mut Context<Self>) {
        if self.scripts.is_some() {
            let payload = script_payload.unwrap_or_else(|| Value::Object(extra.iter().map(|(k, v)| (k.clone(), crate::ui::expr::to_json(v))).collect()));
            self.pending_script.push(json!({"type": "event", "name": event, "payload": payload}));
        }
        self.play_event_sound(event, &extra);
        let Some(spec) = self.settings.events.get(event).cloned() else { return };
        let ctx = crate::ui::expr::with(&self.context(), extra);
        for h in crate::behavior::handlers(&spec) {
            let (cond, action) = crate::behavior::handler_parts(&h);
            if cond.is_some_and(|c| !self.exprs.truthy(c, &ctx)) {
                continue;
            }
            let action = self.exprs.resolve_action(action, &ctx);
            self.run_action(&action, window, cx);
        }
    }

    /** `sounds` from settings: a sound per event (a turn ending in a question plays `chat.question`
    instead, for the events in `sounds.question_event`), focused or not unless `when` is "unfocused". */
    fn play_event_sound(&mut self, event: &str, extra: &[(String, Val)]) {
        let sounds = &self.settings.sounds;
        if sounds.get("enabled") == Some(&Value::Bool(false)) || (sounds.get("when").and_then(Value::as_str) == Some("unfocused") && self.focused) {
            return;
        }
        let swaps = match sounds.get("question_event") {
            None => event == "chat.finished",
            Some(Value::String(e)) => e == event,
            Some(Value::Array(a)) => a.iter().any(|e| e.as_str() == Some(event)),
            Some(_) => false,
        };
        let asks = swaps && extra.iter().any(|(k, v)| k == "chat" && crate::ui::expr::to_json(v).get("asks") == Some(&Value::Bool(true)));
        let key = if asks { "chat.question" } else { event };
        if let Some(name) = sounds.get(key).and_then(Value::as_str).map(str::to_string) {
            let volume = sounds.get("volume").and_then(Value::as_f64).unwrap_or(1.) as f32;
            self.record_notice("sound", &name, None);
            #[cfg(target_os = "macos")]
            crate::macos::play_sound(&name, volume);
            let _ = volume;
        }
    }

    /** problems.txt lets an agent that just edited settings see what broke. */
    fn write_problems(&mut self) {
        let mut lines: Vec<String> = self.settings_error.iter().cloned().collect();
        lines.extend(self.settings.warnings.iter().cloned());
        lines.extend(self.scripts.iter().flat_map(|s| s.errors.iter().cloned()));
        lines.extend(self.exprs.errors.borrow().iter().cloned());
        let mut unbacked: Vec<String> = self.models.iter().filter_map(|(h, m)| Some(format!("{h}: {}", no_backup(m)?))).collect();
        unbacked.sort();
        lines.extend(unbacked);
        let text = if lines.is_empty() { "No problems.\n".to_string() } else { lines.join("\n") + "\n" };
        if text != self.problems_written {
            let _ = std::fs::write(settings::dir().join("problems.txt"), &text);
            self.problems_written = text;
        }
    }

    pub(crate) fn set_state(&mut self, key: String, value: Value) {
        self.ctx_rev += 1;
        let regroup = key == "sidebar_projects" || key == "moving";
        // null forgets the key: back to the settings' starting value, if any.
        if value.is_null() {
            match self.settings.state.get(&key) {
                Some(d) => self.state.insert(key.clone(), d.clone()),
                None => self.state.remove(&key),
            };
            self.user_state.remove(&key);
        } else {
            self.state.insert(key.clone(), value.clone());
            self.user_state.insert(key, value);
        }
        self.save_state();
        if regroup {
            self.merge_models();
        }
    }

    fn save_state(&self) {
        let _ = std::fs::write(settings::dir().join("ui-state.json"), serde_json::to_string_pretty(&self.user_state).unwrap_or_default());
    }

    fn on_incoming(&mut self, host: &str, msg: Incoming, window: &mut Window, cx: &mut Context<Self>) {
        // Terminal output isn't in the context, transcripts count by their own revision, and hook events
        // (many a second while agents work) show in it only for inspecting: none rebuild it.
        if !matches!(msg, Incoming::Event(Event::TermOutput { .. } | Event::Transcript { .. } | Event::Hook(_))) {
            self.ctx_rev += 1;
        }
        match msg {
            Incoming::Connected => {
                self.host_up.insert(host.to_string(), true);
                self.down_since.remove(host);
                self.connected = self.host_up.values().any(|u| *u);
                self.status = self.tr("connected", "Connected", &[]);
                self.watching.clear();
                self.configure_host(host);
                self.pending_events.push(("host.connected".into(), [("host".to_string(), json!(host))].into_iter().collect()));
                let remote = self.remote_for_host(host);
                let reads = self.pending_inbox_reads.remove(host).unwrap_or_default();
                cx.spawn(async move |_, _| {
                    let _ = remote.call(Call::Hello { client: "agent-box-app".into() }).await;
                    for r in reads {
                        let _ = remote.call(r).await;
                    }
                    let _ = remote.call(Call::Subscribe { from_seq: None }).await;
                })
                .detach();
            }
            Incoming::Disconnected(why) => {
                let was_up = self.host_up.insert(host.to_string(), false) == Some(true);
                if was_up {
                    self.went_down(host, cx);
                }
                self.connected = self.host_up.values().any(|u| *u);
                self.status = self.tr("reconnecting", "Reconnecting to {host}… ({why})", &[("host", host), ("why", &why)]);
                self.terminals.retain(|_, t| t.read(cx).host != host);
                self.term_by_id.retain(|(h, _), _| h != host);
                // Once per drop, not on every retry while it stays down (`.app.host_errors` says why).
                if was_up {
                    self.pending_events.push(("host.disconnected".into(), [("host".to_string(), json!(host)), ("why".to_string(), json!(why))].into_iter().collect()));
                }
            }
            Incoming::Event(Event::Model(m)) => {
                let before: HashMap<String, String> = self.model.chats.iter().map(|c| (c.id.clone(), c.state.clone())).collect();
                let unbacked = no_backup(&m).map(str::to_string);
                let was_unbacked = self.models.get(host).and_then(no_backup).is_some();
                let old = self.models.insert(host.to_string(), m);
                self.last_seen.insert(host.to_string(), now_secs());
                self.cache_model(host, false);
                if let (Some(e), false) = (unbacked, was_unbacked) {
                    self.toast(format!("{host}: {e}"), window, cx);
                }
                self.merge_models();
                self.drop_screens(window, cx);
                if let Some(old) = old {
                    // A chat that left this host (moved to another machine) is read from where it went now.
                    let here: HashSet<&String> = self.models.get(host).map(|m| m.chats.iter().map(|c| &c.id).collect()).unwrap_or_default();
                    let gone: Vec<String> = old.chats.iter().filter(|c| !here.contains(&c.id)).map(|c| c.id.clone()).collect();
                    for id in gone {
                        self.watching.remove(&id);
                    }
                    self.model_events(&old, host);
                }
                if !before.is_empty() {
                    self.fire_chat_events(&before, window, cx);
                    self.flush_queue(window, cx);
                }
                self.pending_events.push(("model".into(), [("host".to_string(), json!(host))].into_iter().collect()));
                let ctx = self.context();
                self.update_badge(&ctx);
                self.pending_motion = true;
                self.mark_diffs_stale();
                // A moved chat: follow it to its worktree on the machine it went to, tab open.
                if let Some((id, to)) = self.pending_move.clone() {
                    if let Some(path) = self.all_worktrees().iter().find(|w| w.host == to && w.chat_ids.contains(&id)).map(|w| w.path.clone()) {
                        self.opened.insert(id.clone());
                        self.closed.remove(&id);
                        self.tab_to_end(&id);
                        self.active_chat.insert(path.clone(), id);
                        self.selected = Some(path);
                        self.pending_move = None;
                    }
                }
                if let Some(id) = self.pending_select.clone() {
                    if let Some(path) = self.all_worktrees().iter().find(|w| w.chat_ids.contains(&id)).map(|w| w.path.clone()) {
                        self.opened.insert(id.clone());
                        self.tab_to_end(&id);
                        self.active_chat.insert(path.clone(), id);
                        self.selected = Some(path);
                        self.pending_select = None;
                        self.starting = None;
                    }
                }
                if self.selected.as_ref().map_or(true, |p| self.worktree(p).is_none()) {
                    self.selected = self
                        .state
                        .get("selected_worktree")
                        .and_then(Value::as_str)
                        .filter(|p| self.worktree(p).is_some())
                        .map(str::to_string)
                        .or_else(|| self.sidebar_order().into_iter().next())
                        .or_else(|| self.all_worktrees().first().map(|w| w.path.clone()));
                }
                self.sync_selection(window, cx);
            }
            Incoming::Event(Event::Transcript { chat_id, from, entries }) => {
                let t = self.transcripts.entry(chat_id.clone()).or_insert_with(|| crate::transcript::Transcript::for_chat(&chat_id));
                let had = t.items.len();
                t.append(from, &entries);
                // New messages as they arrive (not loads of older ones): `.chat`, `.message {kind, text}`.
                let new: Vec<Value> = t.items.iter().skip(had).filter_map(|i| match i {
                    Item::Assistant { text } => Some(json!({"kind": "assistant", "text": text})),
                    Item::User { text } => Some(json!({"kind": "user", "text": text})),
                    _ => None,
                }).collect();
                let chat = if new.is_empty() { Value::Null } else { self.chat(&chat_id).map(|c| self.chat_json(c)).unwrap_or_else(|| json!({"id": chat_id})) };
                for message in new {
                    self.pending_events.push(("chat.message".into(), [("chat".to_string(), chat.clone()), ("message".to_string(), message)].into_iter().collect()));
                }
            }
            Incoming::Event(Event::TermOutput { term_id, data }) => {
                if let Some(t) = self.term_by_id.get(&(host.to_string(), term_id)).cloned() {
                    t.update(cx, |t, cx| t.output(&data, cx));
                }
                return;
            }
            Incoming::Event(Event::AppAction { action }) => {
                self.log_action(&json!({"from": host, "action": "relayed"}));
                let req = crate::behavior::parse_request(None, action);
                match crate::behavior::remote_allowed(self.settings.get("inspect.remote_actions"), &req.action) {
                    Ok(()) => {
                        // A machine that starts using the app again gets every file, changed or not.
                        let active = self.settings.num("inspect.mirror_active_secs", 300.);
                        if self.relayed_at.insert(host.to_string(), std::time::Instant::now()).is_none_or(|t| t.elapsed().as_secs_f64() >= active) {
                            crate::ui::inspect::forget_mirrored();
                        }
                        self.mirror_at = None;
                        self.pending_requests.push((req, Some(host.to_string())))
                    }
                    Err(why) => {
                        self.exprs.note(format!("relayed action from {host} refused: {why}"));
                        if let Some(id) = req.id.as_deref().and_then(crate::behavior::safe_id) {
                            let result = json!({"id": id, "ok": false, "error": format!("refused: {why}"), "errors": [], "value": null});
                            self.deliver_result(&id, &result, Some(host.to_string()));
                        }
                    }
                }
            }
            Incoming::Event(Event::Hook(e)) => {
                let keep = self.settings.num("inspect.hook_events", 50.) as usize;
                let hook = json!({"host": host, "seq": e.seq, "at": e.at, "event": e.event, "session_id": e.session_id});
                self.hooks.push(hook.clone());
                if self.hooks.len() > keep {
                    let extra = self.hooks.len() - keep;
                    self.hooks.drain(..extra);
                }
                self.pending_events.push(("hook".into(), [("hook".to_string(), hook)].into_iter().collect()));
            }
            Incoming::Event(_) => {}
        }
        self.flush(window, cx);
        cx.notify();
    }

    /** Every host's model in one: this Mac's projects and sections first, then the box's. */
    /** A host that was up dropped: cache what it showed, and redraw once it counts as offline. */
    fn went_down(&mut self, host: &str, cx: &mut Context<Self>) {
        self.down_since.insert(host.to_string(), std::time::Instant::now());
        // It answered until now, however long ago its model last changed.
        self.last_seen.insert(host.to_string(), now_secs());
        self.cache_model(host, true);
        let after = Duration::from_millis(self.offline_after_ms() + 50);
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(after).await;
            let _ = this.update(cx, |ws, cx| {
                ws.ctx_rev += 1;
                cx.notify();
            });
        })
        .detach();
    }

    fn offline_after_ms(&self) -> u64 {
        self.settings.num("connection.offline_after_ms", 8000.) as u64
    }

    /** Whether a host's daemon has been out of reach long enough to show its worktrees and chats as offline
    (from the cache, or as they were when it dropped). */
    pub(crate) fn host_offline(&self, host: &str) -> bool {
        self.host_up.get(host) != Some(&true)
            && self.models.contains_key(host)
            && self.down_since.get(host).is_none_or(|t| t.elapsed() >= Duration::from_millis(self.offline_after_ms()))
    }

    /** Writes a host's model to the cache every `connection.cache_seconds` (and when it drops), so a
    restart while it's out of reach still shows its worktrees. */
    fn cache_model(&mut self, host: &str, now: bool) {
        let every = Duration::from_secs_f64(self.settings.num("connection.cache_seconds", 30.));
        if !now && self.cache_saved.get(host).is_some_and(|t| t.elapsed() < every) {
            return;
        }
        let Some(m) = self.models.get(host) else { return };
        self.cache_saved.insert(host.to_string(), std::time::Instant::now());
        let text = serde_json::to_string(&json!({"at": self.last_seen.get(host).copied().unwrap_or_else(now_secs), "model": m})).unwrap_or_default();
        let path = model_cache_path(host);
        std::thread::spawn(move || {
            let _ = std::fs::create_dir_all(path.parent().unwrap_or(std::path::Path::new(".")));
            let tmp = path.with_extension("tmp");
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        });
    }

    fn merge_models(&mut self) {
        let mut hosts: Vec<&String> = self.models.keys().collect();
        // `sidebar.host_order`: whose worktrees come first in a merged project.
        let order: Vec<String> = self.settings.get("sidebar.host_order").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_else(|| vec![self.local_host(), self.default_host()]);
        let default_host = self.default_host();
        hosts.sort_by_key(|h| order.iter().position(|x| x == *h).unwrap_or(order.len()));
        let mut merged = Model { host: "all".into(), ..Default::default() };
        let (mut project_hosts, mut section_hosts): (Vec<Vec<String>>, HashMap<String, String>) = (vec![], HashMap::new());
        // state.sidebar_projects: "merged" (default) lists each repo once with every machine's worktrees;
        // "by_host" keeps a project per machine.
        let by_host = self.state.get("sidebar_projects").and_then(Value::as_str) == Some("by_host");
        for h in hosts {
            let m = &self.models[h];
            if *h == default_host {
                merged.ssh_host = m.ssh_host.clone();
            }
            merged.last_seq = merged.last_seq.max(m.last_seq);
            for p in &m.projects {
                let mut p = p.clone();
                for w in &mut p.items {
                    w.host = h.clone();
                }
                match merged.projects.iter().position(|x| x.repo == p.repo).filter(|_| !by_host) {
                    Some(i) => {
                        merged.projects[i].items.extend(p.items);
                        if !project_hosts[i].contains(h) {
                            project_hosts[i].push(h.clone());
                        }
                    }
                    None => {
                        merged.projects.push(p);
                        project_hosts.push(vec![h.clone()]);
                    }
                }
            }
            for s in &m.sections {
                let mut s = s.clone();
                for w in &mut s.items {
                    w.host = h.clone();
                }
                // A section a move is taking to another machine exists on both under one id for a moment:
                // one section, its rows leaving one machine and arriving on the other.
                if let Some(same) = merged.sections.iter_mut().find(|x| x.id == s.id) {
                    same.items.extend(s.items);
                    continue;
                }
                section_hosts.insert(s.id.clone(), h.clone());
                merged.sections.push(s);
            }
            merged.chats.extend(m.chats.iter().cloned());
        }
        for (p, hosts) in merged.projects.iter_mut().zip(&project_hosts) {
            if let Some(saved) = self.state.get(&format!("worktree_order:{}", p.repo)).and_then(Value::as_array).filter(|_| hosts.len() > 1) {
                let saved: Vec<&str> = saved.iter().filter_map(Value::as_str).collect();
                apply_order(&mut p.items, &saved, |w| (w.path.clone(), w.host.clone()));
            }
        }
        // state.section_order: the sidebar's order of sections from several machines (each keeps its own).
        let section_machines: HashSet<&String> = section_hosts.values().collect();
        if let Some(saved) = self.state.get("section_order").and_then(Value::as_array).filter(|_| section_machines.len() > 1) {
            let saved: Vec<&str> = saved.iter().filter_map(Value::as_str).collect();
            apply_order(&mut merged.sections, &saved, |s| (s.id.clone(), section_hosts.get(&s.id).cloned().unwrap_or_default()));
        }
        // state.moving {to, branches, at}: a move under way (bin/chat sets it). Once a branch's worktree is on
        // `to`, it takes the place of its copy on the machine it's leaving, so the row switches machines.
        // One left behind by a move that never finished counts for 10 minutes at most.
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.);
        if let Some(mv) = self.state.get("moving").filter(|v| v["at"].as_f64().is_some_and(|at| now - at < 600.)) {
            let to = mv["to"].as_str().unwrap_or_default();
            let branches: HashSet<&str> = mv["branches"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
            let arrived: HashSet<(String, String)> = merged.projects.iter().flat_map(|p| &p.items).chain(merged.sections.iter().flat_map(|s| &s.items))
                .filter(|w| w.host == to).filter_map(|w| w.branch.clone().filter(|b| branches.contains(b.as_str())).map(|b| (w.repo.clone(), b))).collect();
            for list in merged.projects.iter_mut().map(|p| &mut p.items).chain(merged.sections.iter_mut().map(|s| &mut s.items)) {
                switch_rows(list, to, &branches, &arrived);
            }
        }
        self.model = merged;
        self.chat_index.borrow_mut().clear();
        self.worktree_index.borrow_mut().clear();
        self.apply_just_sent();
        self.project_hosts = project_hosts;
        self.section_hosts = section_hosts;
        self.keep_pending_moves();
    }

    /** Worktrees dragged into place stay there until their machine's model shows them there (or
    `sidebar.move_hold_secs` pass): a model it was already building when the move landed still has the
    old order, and would put them back for a moment. */
    fn keep_pending_moves(&mut self) {
        let hold = self.settings.num("sidebar.move_hold_secs", 15.);
        let moves = std::mem::take(&mut self.pending_moves);
        for mv in moves {
            let shown = self.models.get(&mv.host).is_some_and(|m| {
                let mut lists = m.projects.iter().map(|p| (None, &p.items)).chain(m.sections.iter().map(|s| (Some(&s.id), &s.items)));
                lists.any(|(section, items)| {
                    let at = |p: &str| items.iter().position(|w| w.path == p);
                    section == mv.section.as_ref() && at(&mv.path).is_some_and(|i| match &mv.host_before {
                        Some(b) => at(b) == Some(i + 1),
                        None => i + 1 == items.len(),
                    })
                })
            });
            if !shown && mv.at.elapsed().as_secs_f64() < hold {
                crate::ui::actions::move_worktree_in(&mut self.model, &mv.path, mv.section.as_deref(), mv.before.as_deref());
                self.pending_moves.push(mv);
            }
        }
    }

    /** Asks every connected host (or those in `hosts`) for its past chats; `.history` holds them newest first. */
    pub(crate) fn load_history(&mut self, hosts: Option<Vec<String>>, limit: Option<usize>, cx: &mut Context<Self>) {
        self.history_loading = true;
        let remotes: Vec<(String, Remote)> = self
            .remotes
            .iter()
            .filter(|(h, _)| self.host_up.get(*h) == Some(&true) && hosts.as_ref().is_none_or(|l| l.contains(h)))
            .map(|(h, r)| (h.clone(), r.clone()))
            .collect();
        let limit = limit.unwrap_or(self.settings.num("history.limit", 300.) as usize);
        let age_fmt = self.age_format();
        cx.spawn(async move |this, cx| {
            let mut all: Vec<Value> = vec![];
            for (host, r) in remotes {
                if let Ok(v) = r.call(Call::History { limit: Some(limit) }).await {
                    for mut c in v["chats"].as_array().cloned().unwrap_or_default() {
                        if let Value::Object(o) = &mut c {
                            o.entry("host").or_insert(Value::String(host.clone()));
                            let age = o.get("last_activity").and_then(Value::as_f64);
                            o.insert("age_text".into(), Value::String(age_text(age, &age_fmt)));
                            let folder = o.get("dir").and_then(Value::as_str).and_then(|d| d.rsplit('/').next()).unwrap_or("").to_string();
                            o.insert("folder".into(), Value::String(folder));
                        }
                        all.push(c);
                    }
                }
            }
            all.sort_by(|a, b| b["last_activity"].as_f64().unwrap_or(0.).total_cmp(&a["last_activity"].as_f64().unwrap_or(0.)));
            this.update(cx, |ws, cx| {
                ws.history = all;
                ws.history_loading = false;
                ws.ctx_rev += 1;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn remote_for_host(&self, host: &str) -> Remote {
        self.remotes.get(host).cloned().unwrap_or_else(|| self.remote.clone())
    }

    /** The machine the app runs on (`app.local_host`): its daemon runs here, not over ssh. */
    pub(crate) fn local_host(&self) -> String {
        crate::behavior::host_name(&self.settings.raw, "local_host", "mac")
    }

    /** Where things go when nothing says (`app.default_host`): new worktrees, unknown chats, calls. */
    pub(crate) fn default_host(&self) -> String {
        crate::behavior::host_name(&self.settings.raw, "default_host", "box")
    }

    pub(crate) fn is_local_host(&self, host: &str) -> bool {
        host == self.local_host() || self.conns.get(host).is_some_and(|c| c.spec.ssh_host.is_empty())
    }

    /** The ssh host a machine is reached at ("" for this one). */
    pub(crate) fn ssh_host_of(&self, host: &str) -> String {
        match self.conns.get(host) {
            Some(c) => c.spec.ssh_host.clone(),
            None if self.is_local_host(host) => String::new(),
            None => self.settings.connection.ssh_host.clone(),
        }
    }

    /** `PATH` for everything the app runs (settings `app.path`). */
    /** Settings `scripts_options` for building and running scripts. */
    fn script_options(&self) -> crate::script::Options {
        let build_dir = match self.settings.get("scripts_options.build_dir").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            Some(d) => crate::script::expand(d).into(),
            None => std::path::PathBuf::from(crate::script::expand("~/.agent-box/script-build")),
        };
        crate::script::Options {
            build_dir,
            sdk_dir: settings::source_dir().join("rust/script-sdk"),
            dependencies: self.settings.get("scripts_options.dependencies").and_then(Value::as_object).cloned().unwrap_or_default(),
            time_limit_ms: self.settings.num("scripts_options.time_limit_ms", 1000.).max(10.) as u64,
            path: self.path_env(),
        }
    }

    /** Compiles waiting scripts off the UI thread; each starts (and gets `start`) when its build lands. */
    fn spawn_script_builds(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(scripts) = self.scripts.as_mut() else { return };
        let generation = scripts.generation;
        for (build, options) in scripts.take_builds() {
            cx.spawn_in(window, async move |this, cx| {
                let name = build.name.clone();
                let result = cx.background_spawn(async move { crate::script::build(&build, &options) }).await;
                this.update_in(cx, |ws, window, cx| {
                    let ctx = ws.script_context();
                    let Some(s) = ws.scripts.as_mut().filter(|s| s.generation == generation) else { return };
                    let mut outs = s.built(&name, result, ctx.clone());
                    outs.extend(s.deliver_named(&name, json!({"type": "event", "name": "start", "payload": null}), ctx));
                    for m in outs {
                        ws.script_out(m, window, cx);
                    }
                    ws.ctx_rev += 1;
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
    }

    pub(crate) fn path_env(&self) -> String {
        let home = std::env::var("HOME").unwrap_or_default();
        let list = self.settings.strings("app.path", DEFAULT_PATH);
        crate::behavior::path_env(&list, &home)
    }

    /** The folder of a.box's scripts (`app.bin_dir`, else `bin` in `app.source_dir`). */
    pub(crate) fn bin_dir(&self) -> std::path::PathBuf {
        match self.settings.get("app.bin_dir").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            Some(d) => crate::script::expand(d).into(),
            None => settings::source_dir().join("bin"),
        }
    }

    fn env_push_command(&self) -> String {
        match self.settings.get("connection.env_push_command").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            Some(c) => c.replace("{bin}", &self.bin_dir().to_string_lossy()),
            None => crate::behavior::sh_quote(&self.bin_dir().join("env-push").to_string_lossy()),
        }
    }

    pub(crate) fn host_of_path(&self, path: &str) -> String {
        self.worktree(path).map(|w| w.host.clone()).unwrap_or_else(|| self.default_host())
    }

    /** A chat's daemon: the host whose worktree holds it, else the chat's own host if connected. */
    pub(crate) fn host_of_chat(&self, id: &str) -> String {
        if let Some(w) = self.all_worktrees().into_iter().find(|w| w.chat_ids.iter().any(|c| c == id)) {
            return w.host.clone();
        }
        self.chat(id).map(|c| c.host.clone()).filter(|h| self.remotes.contains_key(h)).unwrap_or_else(|| self.default_host())
    }

    /** The daemon a call belongs to, from the chat, worktree, or section it names. */
    pub(crate) fn route(&self, call: &Call) -> Remote {
        let host = match call {
            Call::ChatFork { chat_id } => self.host_of_chat(chat_id),
            Call::TranscriptRead { chat_id, .. }
            | Call::TranscriptUnwatch { chat_id }
            | Call::ChatStart { chat_id }
            | Call::ChatStop { chat_id }
            | Call::ChatArchive { chat_id }
            | Call::ChatRename { chat_id, .. }
            | Call::ChatSend { chat_id, .. }
            | Call::ChatKeys { chat_id, .. } => self.host_of_chat(chat_id),
            Call::ChatNew { dir: Some(d), .. } => self.host_of_path(d),
            Call::SectionRename { id, .. } | Call::SectionDelete { id } | Call::SectionMove { id, .. } => {
                self.section_hosts.get(id).cloned().unwrap_or_else(|| self.default_host())
            }
            Call::SectionAssign { path, .. } | Call::WorktreeDelete { path } | Call::WorktreeMove { path, .. } | Call::WorktreeLabel { path, .. } | Call::WorktreeRenameBranch { path, .. } | Call::WorktreeDiff { path, .. } => {
                self.host_of_path(path)
            }
            Call::TermOpen { cwd: Some(c), .. } => self.host_of_path(c),
            _ => self.default_host(),
        };
        self.remote_for_host(&host)
    }

    pub(crate) fn all_worktrees(&self) -> Vec<&Worktree> {
        self.model.projects.iter().flat_map(|p| p.items.iter()).chain(self.model.sections.iter().flat_map(|s| s.items.iter())).collect()
    }

    pub(crate) fn worktree(&self, path: &str) -> Option<&Worktree> {
        let at = |(in_section, i, j): (bool, usize, usize)| {
            let items = if in_section { self.model.sections.get(i).map(|s| &s.items) } else { self.model.projects.get(i).map(|p| &p.items) };
            items.and_then(|items| items.get(j)).filter(|w| w.path == path)
        };
        if let Some(w) = self.worktree_index.borrow().get(path).copied().and_then(at) {
            return Some(w);
        }
        let found = self.all_worktrees().into_iter().find(|w| w.path == path)?;
        let mut index = self.worktree_index.borrow_mut();
        index.clear();
        for (i, p) in self.model.projects.iter().enumerate() {
            index.extend(p.items.iter().enumerate().map(|(j, w)| (w.path.clone(), (false, i, j))));
        }
        for (i, s) in self.model.sections.iter().enumerate() {
            index.extend(s.items.iter().enumerate().map(|(j, w)| (w.path.clone(), (true, i, j))));
        }
        Some(found)
    }

    pub(crate) fn chat(&self, id: &str) -> Option<&Chat> {
        let at = |i: usize| self.model.chats.get(i).filter(|c| c.id == id);
        if let Some(c) = self.chat_index.borrow().get(id).copied().and_then(at) {
            return Some(c);
        }
        let stale = self.chat_index.borrow().len() != self.model.chats.len() || self.chat_index.borrow().contains_key(id);
        if stale {
            *self.chat_index.borrow_mut() = self.model.chats.iter().enumerate().map(|(i, c)| (c.id.clone(), i)).collect();
            if let Some(c) = self.chat_index.borrow().get(id).copied().and_then(at) {
                return Some(c);
            }
        }
        self.sub_chats.iter().find(|(_, c)| c.id == id).map(|(_, c)| c)
    }

    /** A worktree's terminal tabs (`state.terms:PATH`): [{id, label, command?}]; the first shell has id "1". */
    pub(crate) fn term_tabs(&self, path: &str) -> Vec<Value> {
        match self.state.get(&format!("terms:{path}")).and_then(Value::as_array) {
            Some(t) if !t.is_empty() => t.clone(),
            _ => vec![json!({"id": "1", "label": self.tr("terminal", "Terminal", &[])})],
        }
    }

    pub(crate) fn term_active(&self, path: &str) -> String {
        let tabs = self.term_tabs(path);
        let want = self.state.get(&format!("term_active:{path}")).and_then(Value::as_str).unwrap_or("1");
        let has = |id: &str| tabs.iter().any(|t| t["id"].as_str() == Some(id));
        if has(want) { want.to_string() } else { tabs[0]["id"].as_str().unwrap_or("1").to_string() }
    }

    /** Where a terminal tab's view is kept: the first shell under the worktree's path, others under PATH#ID. */
    pub(crate) fn term_key(path: &str, id: &str) -> String {
        if id == "1" { path.to_string() } else { format!("{path}#{id}") }
    }

    /** The view for a worktree's active terminal tab, made on first show. */
    pub(crate) fn active_terminal(&mut self, path: &str, cx: &mut Context<Self>) -> Option<Entity<TerminalView>> {
        let id = self.term_active(path);
        let key = Self::term_key(path, &id);
        if let Some(t) = self.terminals.get(&key) {
            return Some(t.clone());
        }
        let w = self.worktree(path)?.clone();
        if !self.connected || id == "1" {
            return None;
        }
        let tab = self.term_tabs(path).into_iter().find(|t| t["id"].as_str() == Some(id.as_str()))?;
        let command = tab.get("command").and_then(Value::as_str).map(str::to_string);
        // A tab may name its own folder and machine (`terminal.new {cwd, host}`).
        let host = tab.get("host").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| w.host.clone());
        let cwd = tab.get("cwd").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| w.path.clone());
        let (remote, ts) = (self.remote_for_host(&host), self.settings.terminal.clone());
        let view = cx.new(|cx| TerminalView::shell(remote, host, None, Some(cwd), Some(id.clone()), command, &ts, cx));
        self.terminals.insert(key, view.clone());
        Some(view)
    }

    /** The live terminal on a chat's own session while its agent shows a screen of its own (`screen`), made
    (and given the typing) when it first shows. */
    pub(crate) fn chat_screen(&mut self, chat: &str, window: &mut Window, cx: &mut Context<Self>) -> Option<Entity<TerminalView>> {
        let key = format!("screen:{chat}");
        if let Some(t) = self.terminals.get(&key) {
            return Some(t.clone());
        }
        let c = self.chat(chat).filter(|c| c.screen && c.alive && !c.tmux.is_empty())?.clone();
        if !self.host_up.get(&c.host).copied().unwrap_or(false) {
            return None;
        }
        let (remote, ts) = (self.remote_for_host(&c.host), self.settings.terminal.clone());
        let view = cx.new(|cx| TerminalView::shell(remote, c.host.clone(), Some(c.tmux.clone()), None, None, None, &ts, cx));
        view.read(cx).focus_handle(cx).focus(window, cx);
        self.terminals.insert(key, view.clone());
        Some(view)
    }

    /** Detaches the screens of chats back at their input box; one that had the typing hands it to the composer. */
    fn drop_screens(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let done: Vec<String> = self.terminals.keys()
            .filter(|k| k.strip_prefix("screen:").is_some_and(|id| !self.chat(id).is_some_and(|c| c.screen && c.alive)))
            .cloned()
            .collect();
        for k in done {
            let Some(t) = self.terminals.remove(&k) else { continue };
            let focused = t.read(cx).is_focused(window);
            t.read(cx).kill();
            if focused {
                self.focus_area_action("composer", k.strip_prefix("screen:").map(str::to_string), window, cx);
            }
        }
    }

    /** The phone app lists the box's worktrees and chats in the sidebar's order: projects then sections, each
    group's rows as shown, each row's chats in tab order. env_push copies the file to the box. */
    fn write_sidebar_order(&self, groups: Vec<Value>) {
        let text = json!({"groups": groups}).to_string();
        if *self.sidebar_order_written.borrow() == text {
            return;
        }
        let path = std::path::PathBuf::from(crate::script::expand("~/.agent-box/sidebar-order.json"));
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, &text).and_then(|_| std::fs::rename(&tmp, &path)).is_ok() {
            *self.sidebar_order_written.borrow_mut() = text;
        }
    }

    /** A worktree's chats in tab order (`state.tab_order[path]`, set by dragging tabs), the rest after. */
    fn chats_of(&self, w: &Worktree) -> Vec<&Chat> {
        let mut chats: Vec<&Chat> = w.chat_ids.iter().filter_map(|id| self.chat(id)).collect();
        chats.extend(self.sub_chats.iter().filter(|(p, _)| *p == w.path).map(|(_, c)| c));
        // Tabs dragged or opened here keep their place in `state.tab_order`; the rest follow, oldest first.
        let order = self.state.get("tab_order").and_then(|o| o.get(&w.path)).and_then(Value::as_array);
        let ranks: HashMap<&str, usize> = order.map(|o| o.iter().enumerate().filter_map(|(i, x)| Some((x.as_str()?, i))).rev().collect()).unwrap_or_default();
        let rank = |id: &str| ranks.get(id).copied().unwrap_or(usize::MAX);
        chats.sort_by(|a, b| rank(&a.id).cmp(&rank(&b.id)).then_with(|| a.created_at.cmp(&b.created_at)));
        chats
    }

    /** Puts a chat's tab last in its worktree's tab bar (`state.tab_order`), where a tab opened just now goes. */
    pub(crate) fn tab_to_end(&mut self, id: &str) {
        let Some(path) = self.all_worktrees().iter().find(|w| w.chat_ids.iter().any(|c| c == id)).map(|w| w.path.clone())
            .or_else(|| self.sub_chats.iter().find(|(_, c)| c.id == id).map(|(p, _)| p.clone()))
        else {
            return;
        };
        let mut ids = self.chats_of_path(&path);
        ids.retain(|x| x != id);
        ids.push(id.to_string());
        let mut order = self.state.get("tab_order").and_then(Value::as_object).cloned().unwrap_or_default();
        order.insert(path, json!(ids));
        self.set_state("tab_order".into(), Value::Object(order));
    }

    pub(crate) fn chats_of_path(&self, path: &str) -> Vec<String> {
        self.worktree(path).map(|w| self.chats_of(w).into_iter().map(|c| c.id.clone()).collect()).unwrap_or_default()
    }

    /** Whether a chat shows as a tab: its process is running (or starting, or was just opened), and it
    wasn't closed. Worktrees open with no tabs unless a chat there is live. */
    pub(crate) fn chat_open(&self, c: &Chat) -> bool {
        if self.closed.contains(&c.id) {
            return false;
        }
        // `chat.open_when`: what makes a chat show as a tab (besides being opened by hand).
        let when = self.settings.get("chat.open_when").and_then(Value::as_array);
        let has = |k: &str| when.map_or(true, |w| w.iter().any(|x| x.as_str() == Some(k)));
        self.opened.contains(&c.id)
            || (has("alive") && c.alive)
            || (has("working") && c.state == "working")
            || (has("waiting") && c.state == "waiting")
            || (has("running_elsewhere") && c.status == "running_elsewhere")
            || self.chat_moving_to(c).is_some()
    }

    pub(crate) fn current_worktree(&self) -> Option<&Worktree> {
        self.selected.as_deref().and_then(|p| self.worktree(p))
    }

    /** The selected worktree's active chat: the one chosen there (this run, else as saved in
    `state.active_tabs` from an earlier one), else the open tab shown most recently before it, else its
    first open tab, else by `chat.open_on_select`:
    "recent" (the one with the newest activity), "first" (the first tab), or none. */
    pub(crate) fn current_chat(&self) -> Option<&Chat> {
        let w = self.current_worktree()?;
        let chats = self.chats_of(w);
        let saved = self.state.get("active_tabs").and_then(|t| t.get(&w.path)).and_then(Value::as_str);
        let active = self.active_chat.get(&w.path).map(String::as_str).or(saved);
        let fallback = || match self.settings.chat.get("open_on_select").and_then(Value::as_str) {
            Some("recent") => chats.iter().max_by(|a, b| a.last_activity.unwrap_or(0.).total_cmp(&b.last_activity.unwrap_or(0.))).copied(),
            Some("first") => chats.first().copied(),
            _ => None,
        };
        active
            .and_then(|id| chats.iter().find(|c| c.id == id && !self.closed.contains(&c.id)).copied())
            .or_else(|| {
                let seen = self.tab_history.get(&w.path)?;
                seen.iter().rev().find_map(|id| chats.iter().find(|c| &c.id == id && self.chat_open(c)).copied())
            })
            .or_else(|| chats.iter().find(|c| self.chat_open(c)).copied())
            .or_else(fallback)
    }

    /** The worktrees in the order the sidebar lists them (`sidebar.select_order`, a jq expression giving
    paths), for `select {offset}` and picking a worktree when the selected one goes. */
    pub(crate) fn sidebar_order(&self) -> Vec<String> {
        let code = self.settings.text_at("sidebar.select_order", "[.projects[].items[].path] + [.sections[] | select(.collapsed | not) | .items[].path]");
        let v = crate::ui::expr::to_json(&self.exprs.first(&code, &self.context()));
        v.as_array().map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default()
    }

    /** Everything expressions can read: model, state, theme, selection, app. The bulk is rebuilt only when
    something it shows changed (`ctx_rev`, a model push, a transcript) or `perf.context_max_age_ms` passed,
    so frames that only animate (spinners, motion) reuse it; layout is fresh every frame. */
    pub(crate) fn context(&self) -> Val {
        let transcripts: u64 = self.transcripts.values().map(|t| t.revision).fold(0, u64::wrapping_add);
        let key = (self.ctx_rev, self.model.last_seq as u64, transcripts, self.render_generation);
        let max_age = self.settings.num("perf.context_max_age_ms", 500.);
        let fresh = |k: &(u64, u64, u64, u64), at: &std::time::Instant| {
            (k.0, k.1, k.2) == (key.0, key.1, key.2) && (at.elapsed().as_secs_f64() * 1000.) < max_age
        };
        let data = match self.ctx_data.borrow().as_ref() {
            // The same frame asks more than once (render, flush): reuse it then too.
            Some((k, at, v)) if fresh(k, at) || *k == key => Some(v.clone()),
            _ => None,
        };
        let data = data.unwrap_or_else(|| {
            let t = std::time::Instant::now();
            let v = self.context_data();
            self.ctx_generation.set(self.ctx_generation.get() + 1);
            self.perf.borrow_mut().spans.insert("context_build", t.elapsed().as_secs_f32() * 1000.);
            *self.ctx_data.borrow_mut() = Some((key, std::time::Instant::now(), v.clone()));
            v
        });
        let layout = self.layout_context();
        match &data {
            Val::Obj(_) => {
                let app = crate::ui::expr::with(&crate::ui::expr::first_field(&data, "app"), vec![("layout".into(), layout)]);
                crate::ui::expr::with(&data, vec![("app".into(), app)])
            }
            _ => data,
        }
    }

    fn context_data(&self) -> Val {
        *self.chat_json_memo.borrow_mut() = Some(HashMap::new());
        *self.building_current.borrow_mut() = Some(self.current_chat().map(|c| c.id.clone()));
        // Each chat and worktree is built once and shared (by reference) by every list that shows it.
        let chat_vals: HashMap<&str, Val> = self.model.chats.iter().chain(self.sub_chats.iter().map(|(_, c)| c)).map(|c| (c.id.as_str(), to_val(&self.chat_json(c)))).collect();
        let worktrees: Vec<(*const Worktree, Val)> = self
            .model
            .projects
            .iter()
            .flat_map(|p| p.items.iter().map(move |w| (w, Value::Null)))
            .chain(self.model.sections.iter().flat_map(|s| s.items.iter().map(move |w| (w, json!({"id": s.id, "name": s.name})))))
            .map(|(w, section)| (w as *const Worktree, self.worktree_val(w, section, &chat_vals)))
            .collect();
        let worktree_val: HashMap<*const Worktree, Val> = worktrees.iter().cloned().collect();
        let rows = |shown: &[&Worktree], gaps: &[String]| -> Val {
            Val::Arr(std::rc::Rc::new(shown.iter().filter_map(|w| Some(self.row_val(w, worktree_val.get(&(*w as *const Worktree))?, gaps))).collect()))
        };
        let project_ids: Vec<String> = self.model.projects.iter().filter(|p| !self.project_removed(&p.repo)).map(|p| p.repo.clone()).collect();
        let project_gap = self.card_gap("project", "projcard", &project_ids);
        let section_ids: Vec<String> = self.model.sections.iter().map(|s| s.id.clone()).collect();
        let section_gap = self.card_gap("section", "seccard", &section_ids);
        let order = std::cell::RefCell::new(Vec::<Value>::new());
        let note_order = |group: String, shown: &[&Worktree]| {
            let items: Vec<Value> = shown.iter().map(|w| json!({"path": w.path, "chats": self.chats_of(w).iter().map(|c| &c.id).collect::<Vec<_>>()})).collect();
            order.borrow_mut().push(json!({"group": group, "items": items}));
        };
        let projects: Vec<Val> = self
            .model
            .projects
            .iter()
            .enumerate()
            .filter(|(_, p)| !self.project_removed(&p.repo))
            .map(|(i, p)| {
                let (gap, gap_end) = &project_gap;
                let (name, letter, hue) = self.project_look(&p.repo, &p.name, p.hue);
                let (show_external, show_primary) = (!self.flag(&format!("hide_external:{}", p.repo)), !self.flag(&format!("hide_primary:{}", p.repo)));
                let shown = self.visible_items(&p.items, |w| (show_external || !w.external) && (show_primary || !w.is_primary));
                note_order(format!("p:{}", p.repo), &shown);
                let gaps = self.drop_gaps(&shown);
                let primary = p.items.iter().find(|w| w.is_primary).or(p.items.first());
                let head = json!({"name": name, "repo": p.repo, "hue": hue, "letter": letter,
                       "hosts": self.project_hosts.get(i).cloned().unwrap_or_else(|| vec![self.default_host()]),
                       "host": self.project_hosts.get(i).and_then(|h| h.first().cloned()).unwrap_or_else(|| self.default_host()),
                       "show_external": show_external, "show_primary": show_primary,
                       "external_count": p.items.iter().filter(|w| w.external).count(),
                       "hidden_count": p.items.iter().chain(self.model.sections.iter().flat_map(|s| s.items.iter())).filter(|w| w.repo == p.repo && self.flag(&format!("hidden:{}", w.path))).count(),
                       "primary_path": primary.map(|w| w.path.clone()), "primary_host": primary.map(|w| w.host.clone()),
                       "drop_gap": gap.as_deref() == Some(p.repo.as_str()), "drop_gap_end": gap_end.as_deref() == Some(p.repo.as_str())});
                crate::ui::expr::with(&to_val(&head), vec![("items".into(), rows(&shown, &gaps))])
            })
            .collect();
        let sections: Vec<Val> = self
            .model
            .sections
            .iter()
            .map(|s| {
                // A section's orchestrator leads it; its workers follow.
                let mut shown = self.visible_items(&s.items, |_| true);
                if self.settings.get("sidebar.orchestrators_first").and_then(Value::as_bool) != Some(false) {
                    shown.sort_by_cached_key(|w| !self.chats_of(w).iter().any(|c| self.is_orchestrator(c)));
                }
                note_order(format!("s:{}", s.id), &shown);
                let gaps = self.drop_gaps(&shown);
                let (gap, gap_end) = &section_gap;
                let head = json!({"id": s.id, "name": s.name, "collapsed": s.collapsed,
                       "drop_gap": gap.as_deref() == Some(s.id.as_str()), "drop_gap_end": gap_end.as_deref() == Some(s.id.as_str()),
                       "host": self.section_hosts.get(&s.id).cloned().unwrap_or_else(|| self.default_host())});
                crate::ui::expr::with(&to_val(&head), vec![("items".into(), rows(&shown, &gaps))])
            })
            .collect();
        let into = self.drop_into();
        self.write_sidebar_order(std::mem::take(&mut *order.borrow_mut()));
        let removed: Vec<Value> = self.model.projects.iter().filter(|p| self.project_removed(&p.repo)).map(|p| json!({"repo": p.repo, "name": self.project_look(&p.repo, &p.name, p.hue).0})).collect();
        let selected_w = self.current_worktree().and_then(|w| worktree_val.get(&(w as *const Worktree))).map(|v| crate::ui::expr::with(v, vec![("section".into(), Val::Null)])).unwrap_or(Val::Null);
        let selected_diff = self.selected.as_deref().map(|p| self.diff_summary(p)).unwrap_or(Value::Null);
        let selected_c = self.current_chat().and_then(|c| chat_vals.get(c.id.as_str()).cloned()).unwrap_or(Val::Null);
        let terminals: Vec<Value> = self.selected.as_deref().map(|p| {
            let active = self.term_active(p);
            self.term_tabs(p).into_iter().map(|mut t| {
                let on = t["id"].as_str() == Some(active.as_str());
                if let Value::Object(o) = &mut t {
                    o.insert("active".into(), Value::Bool(on));
                }
                t
            }).collect()
        }).unwrap_or_default();
        let background: Vec<Value> = self
            .current_chat()
            .filter(|c| c.alive || c.state == "working" || c.state == "waiting")
            .and_then(|c| self.transcripts.get(&c.id).map(|t| (c, t)))
            .map(|(c, t)| {
                t.background().into_iter().map(|b| json!({
                    "kind": b.kind, "label": b.label, "tool_id": b.tool_id, "task_id": b.task_id, "output": b.output,
                    "chat": c.id, "agent": b.task_id.clone().unwrap_or_else(|| b.tool_id.clone()),
                })).collect()
            })
            .unwrap_or_default();
        let chats_here: Vec<Val> = self.current_worktree().map(|w| self.chats_of(w).into_iter().filter_map(|c| chat_vals.get(c.id.as_str()).cloned()).collect()).unwrap_or_default();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.);
        // Usage is filed under the agent's own session id (Codex picks its own; Claude's is the chat id).
        let usage_id = |c: &Chat| c.session.clone().unwrap_or_else(|| c.id.clone());
        let tabs: Vec<(String, String)> = self.current_worktree().map(|w| self.chats_of(w).into_iter().filter(|c| self.chat_open(c)).map(|c| (usage_id(c), c.name.clone())).collect()).unwrap_or_default();
        let current_usage_id = self.current_chat().map(usage_id);
        let usage = self.usage.context(current_usage_id.as_deref(), self.current_worktree().map(|w| (w.host.as_str(), w.path.as_str())), &tabs, &self.usage_providers(), &self.meter_options());
        let transcript = self.current_chat().and_then(|c| self.transcripts.get(&c.id)).map(|t| self.transcript_json(t)).unwrap_or_default();
        // Hosts out of reach, with when each was last heard from.
        let offline: Map<String, Value> = self.models.keys().filter(|h| self.host_offline(h))
            .map(|h| (h.clone(), json!({"last_seen": self.last_seen.get(h), "ago": age_text(self.last_seen.get(h).copied(), &self.age_format())})))
            .collect();
        let host_errors: Map<String, Value> = self.remotes.iter().map(|(h, r)| (h.clone(), json!(r.stderr()))).filter(|(_, v)| v.as_array().is_some_and(|a| !a.is_empty())).collect();
        let arr = |v: Vec<Val>| Val::Arr(std::rc::Rc::new(v));
        let all_chats: Vec<Val> = self.model.chats.iter().chain(self.sub_chats.iter().map(|(_, c)| c)).filter_map(|c| chat_vals.get(c.id.as_str()).cloned()).collect();
        let selected = crate::ui::expr::with(
            &to_val(&json!({"diff": selected_diff, "terminals": terminals, "background": background, "move": self.move_plan(), "transcript": transcript})),
            vec![("worktree".into(), selected_w), ("chat".into(), selected_c), ("chats".into(), arr(chats_here))],
        );
        let shared = vec![
            ("projects".to_string(), arr(projects)),
            ("sections".to_string(), arr(sections)),
            ("worktrees".to_string(), arr(worktrees.into_iter().map(|(_, v)| v).collect())),
            ("chats".to_string(), arr(all_chats)),
            ("selected".to_string(), selected),
        ];
        let ctx = to_val(&json!({
            "inbox": self.inbox_json(),
            "composers": self.composer_texts,
            "state": self.state,
            "theme": self.settings.theme,
            "hosts": self.settings.hosts,
            "agents": self.settings.get("daemon.agents").cloned().unwrap_or(Value::Null),
            "history": self.history,
            "app": {
                "confirm": self.confirm.as_ref().map(|c| { let mut c = c.clone(); if let Value::Object(o) = &mut c { o.remove("then"); } c }),
                "starting": self.starting,
                "hosts_up": self.host_up,
                "offline": offline,
                "completion": self.completion.as_ref().map(|(chat, trigger, items, selected)| json!({
                    "chat": chat, "trigger": trigger, "items": crate::completions::to_json(items), "selected": selected
                })),
                "history_loading": self.history_loading,
                "connected": self.connected,
                "removed_projects": removed,
                "notifications": self.notices,
                "window": {"width": self.window_size.0, "height": self.window_size.1},
                "hooks": self.hooks,
                "host_errors": host_errors,
                "local_host": self.local_host(),
                "default_host": self.default_host(),
                "script_actions": self.scripts.as_ref().map(|s| s.actions.clone()).unwrap_or_default(),
                "palette": self.palette_view.as_ref().map(|p| self.palette_json(p)),
                "image_view": self.image_view.as_ref().map(|(k, _)| k.clone()),
                "settings_sheet": self.settings_sheet.then(|| json!({
                    "path": settings::path(), "docs": settings::dir().join("SETTINGS.md"),
                    "text": std::fs::read_to_string(settings::path()).unwrap_or_default(),
                    "error": self.settings_error,
                })),
                "dialog": self.dialog.as_ref().map(|d| json!({"title": d.title, "fields": d.labels, "view": d.view,
                    "values": d.view.then(|| (0..d.labels.len()).map(|i| self.state.get(&format!("dialog_field:{i}")).map(crate::ui::render::value_text).unwrap_or_default()).collect::<Vec<_>>())})),
                "palette_open": self.palette_open,
                "focus_area": self.focus_now,
                "nav": {"back": self.can_navigate(-1), "forward": self.can_navigate(1), "at": self.nav_at,
                        "visits": self.nav.iter().map(|(w, c)| json!({"worktree": w, "chat": c})).collect::<Vec<_>>()},
                "dragging": self.dragging,
                "drop": self.drop_hint.as_ref().map(|h| json!({"key": h.key, "zone": h.zone, "kind": h.kind})),
                "drag": self.drag_item.borrow().as_ref().map(|(k, v)| json!({"kind": k, "value": v, "into": into.as_ref().map(|x| &x.1), "into_key": into.as_ref().map(|x| &x.0)})),
                "focused": self.focused,
                "status": self.status,
                "ssh_host": self.model.ssh_host,
                "settings_error": self.settings_error,
                "errors": self.exprs.errors.borrow().iter().chain(self.settings.warnings.iter()).cloned().collect::<Vec<_>>(),
            },
            "now": now,
            "usage": usage,
            "data": self.script_data,
            "designer": {
                "working": self.designer.working,
                "attachments": self.attachments_json(crate::designer::CHAT_ID),
                "activity": if self.designer.working { self.transcripts.get(crate::designer::CHAT_ID).map(|t| t.activity()).unwrap_or_default() } else { String::new() },
                "has_messages": self.transcripts.get(crate::designer::CHAT_ID).map(|t| !t.items.is_empty()).unwrap_or(false),
                "sessions": self.designer_sessions.iter().map(|v| {
                    let mut v = v.clone();
                    v["age_text"] = json!(age_text(v["last"].as_f64(), &self.age_format()));
                    v
                }).collect::<Vec<_>>(),
            },
        }));
        *self.chat_json_memo.borrow_mut() = None;
        *self.building_current.borrow_mut() = None;
        let mut extra = shared;
        extra.push(("settings".into(), self.settings_val.clone()));
        crate::ui::expr::with(&ctx, extra)
    }

    /** `.settings`: the merged settings without the bulky parts the rest of the context already shows or
    doesn't need (views, components, theme, patch). */
    fn settings_context(&self) -> Val {
        let mut v = self.settings.raw.clone();
        if let Value::Object(o) = &mut v {
            for k in ["views", "components", "theme", "patch"] {
                o.remove(k);
            }
        }
        to_val(&v)
    }

    /** `.app.layout`: where each node with an id was drawn last frame, `{id: {x, y, w, h}}`. */
    fn layout_context(&self) -> Val {
        if let Some((gen, v)) = self.layout_val.borrow().as_ref() {
            if *gen == self.render_generation {
                return v.clone();
            }
        }
        let bounds: Map<String, Value> = self.motion.borrow().bounds.iter().map(|(k, b)| (k.clone(), json!({"x": b[0], "y": b[1], "w": b[2], "h": b[3]}))).collect();
        let v = to_val(&Value::Object(bounds));
        *self.layout_val.borrow_mut() = Some((self.render_generation, v.clone()));
        v
    }

    /** A chat's newest transcript items for `.selected.transcript` (`inspect.transcript_items` of them,
    text cut at `inspect.transcript_chars`): `{kind, text, tool, summary, is_error}`. */
    fn transcript_json(&self, t: &Transcript) -> Vec<Value> {
        let n = self.settings.num("inspect.transcript_items", 20.) as usize;
        let chars = self.settings.num("inspect.transcript_chars", 2000.) as usize;
        let cut = |s: &str| s.chars().take(chars).collect::<String>();
        let start = t.items.len().saturating_sub(n);
        t.items[start..]
            .iter()
            .map(|i| match i {
                Item::User { text } => json!({"kind": "user", "text": cut(text)}),
                Item::Assistant { text } => json!({"kind": "assistant", "text": cut(text)}),
                Item::Thinking { text } => json!({"kind": "thinking", "text": cut(text)}),
                Item::Notice { text } => json!({"kind": "notice", "text": cut(text)}),
                Item::Image { .. } => json!({"kind": "image"}),
                Item::Wake => json!({"kind": "wake"}),
                Item::Tool { name, summary, result, is_error, .. } => json!({"kind": "tool", "tool": name, "summary": summary, "text": result.as_deref().map(cut), "is_error": is_error, "done": result.is_some()}),
            })
            .collect()
    }

    fn flag(&self, key: &str) -> bool {
        self.state.get(key).and_then(Value::as_bool).unwrap_or(false)
    }

    fn project_removed(&self, repo: &str) -> bool {
        self.flag(&format!("removed_project:{repo}"))
    }

    /** A project's name, badge letter, and hue, with Project settings' overrides (`project_name:REPO` and so on in state). */
    pub(crate) fn project_look(&self, repo: &str, name: &str, hue: f32) -> (String, String, f32) {
        let text = |k: &str| self.state.get(&format!("{k}:{repo}")).map(crate::ui::render::value_text).map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
        let name = text("project_name").unwrap_or_else(|| name.to_string());
        let letter = text("project_letter").map(|l| l.chars().take(2).collect()).unwrap_or_else(|| letter(&name));
        let hue = text("project_hue").and_then(|h| h.parse::<f32>().ok()).unwrap_or(hue);
        (name, letter, hue)
    }

    /** What the sidebar lists: not hidden, its project not removed, passing `keep`; pinned ones first, then
    by `state.sidebar_order` ("manual": as arranged; "recent": newest activity first). */
    fn visible_items<'a>(&self, items: &'a [Worktree], keep: impl Fn(&Worktree) -> bool) -> Vec<&'a Worktree> {
        let mut out: Vec<&Worktree> = items.iter().filter(|w| !self.flag(&format!("hidden:{}", w.path)) && !self.project_removed(&w.repo) && keep(w)).collect();
        if self.state.get("sidebar_order").and_then(Value::as_str) == Some("recent") {
            out.sort_by(|a, b| b.age.unwrap_or(0.).total_cmp(&a.age.unwrap_or(0.)));
        }
        out.sort_by_key(|w| !self.flag(&format!("pinned:{}", w.path)));
        out
    }

    /** While a worktree is dragged over a row's edge, the row it would land above opens a gap (`drop_gap`),
    so the list makes room live. The gap is inside that row, so the pointer stays over the same target. */
    fn drop_gaps(&self, shown: &[&Worktree]) -> Vec<String> {
        let Some(h) = self.drop_hint.as_ref().filter(|h| h.kind == "worktree") else { return vec![] };
        let Some(target) = h.key.strip_prefix("wt:") else { return vec![] };
        let Some(i) = shown.iter().position(|w| w.path == target) else { return vec![] };
        match h.zone.as_str() {
            "before" => vec![target.to_string()],
            "after" => shown[i + 1..].iter().find(|w| Some(w.path.as_str()) != h.value.as_str()).map(|w| vec![w.path.clone(), target.to_string()]).unwrap_or_default(),
            _ => vec![],
        }
    }

    /** While a section or project (`kind`) is dragged over a card (drop key `prefix:ID`): the card that opens
    room above it for the drop (`drop_gap`), or the last card when it would land after it (`drop_gap_end`). */
    fn card_gap(&self, kind: &str, prefix: &str, ids: &[String]) -> (Option<String>, Option<String>) {
        let Some(h) = self.drop_hint.as_ref().filter(|h| h.kind == kind) else { return (None, None) };
        let Some(target) = h.key.strip_prefix(prefix).and_then(|k| k.strip_prefix(':')) else { return (None, None) };
        let shown: Vec<&String> = ids.iter().filter(|i| Some(i.as_str()) != h.value.as_str()).collect();
        let Some(i) = shown.iter().position(|x| x.as_str() == target) else { return (None, None) };
        match h.zone.as_str() {
            "before" => (Some(target.to_string()), None),
            "after" => match shown.get(i + 1) {
                Some(next) => (Some(next.to_string()), None),
                None => (None, Some(target.to_string())),
            },
            _ => (None, None),
        }
    }

    /** The group a worktree is in: `sec:ID` or `proj:REPO`, and its name. */
    fn group_of(&self, path: &str) -> Option<(String, String)> {
        if let Some(s) = self.model.sections.iter().find(|s| s.items.iter().any(|w| w.path == path)) {
            return Some((format!("sec:{}", s.id), s.name.clone()));
        }
        let w = self.model.projects.iter().flat_map(|p| p.items.iter()).find(|w| w.path == path)?;
        let p = self.model.projects.iter().find(|p| p.repo == w.repo)?;
        Some((format!("proj:{}", p.repo), self.project_look(&p.repo, &p.name, p.hue).0))
    }

    /** While a worktree is dragged over another section or project: that group (`sec:ID`/`proj:REPO`) and its name. */
    pub(crate) fn drop_into(&self) -> Option<(String, String)> {
        let h = self.drop_hint.as_ref().filter(|h| h.kind == "worktree")?;
        let from = self.group_of(h.value.as_str()?)?.0;
        let to = if let Some(p) = h.key.strip_prefix("wt:") {
            self.group_of(p)?
        } else if let Some(id) = h.key.strip_prefix("sec:") {
            (h.key.clone(), self.model.sections.iter().find(|s| s.id == id)?.name.clone())
        } else if let Some(repo) = h.key.strip_prefix("proj:") {
            let p = self.model.projects.iter().find(|p| p.repo == repo)?;
            (h.key.clone(), self.project_look(&p.repo, &p.name, p.hue).0)
        } else {
            return None;
        };
        (to.0 != from).then_some(to)
    }

    /** A worktree's context value with its chats (shared with the chat list). */
    fn worktree_val(&self, w: &Worktree, section: Value, chat_vals: &HashMap<&str, Val>) -> Val {
        let chats: Vec<Val> = self.chats_of(w).iter().filter_map(|c| chat_vals.get(c.id.as_str()).cloned()).collect();
        crate::ui::expr::with(&to_val(&self.worktree_json_head(w, section)), vec![("chats".into(), Val::Arr(std::rc::Rc::new(chats)))])
    }

    /** A sidebar row: its worktree's value with where a dragged row would land. */
    fn row_val(&self, w: &Worktree, base: &Val, gaps: &[String]) -> Val {
        // gaps[0] opens the room; a target whose room opens on the row below (gaps[1]) hides its drop line.
        let dragged = self.drag_item.borrow().as_ref().is_some_and(|(k, v)| k == "worktree" && v.as_str() == Some(&w.path));
        crate::ui::expr::with(base, vec![
            ("drop_gap".into(), Val::Bool(gaps.first() == Some(&w.path))),
            ("drop_room".into(), Val::Bool(gaps.contains(&w.path))),
            ("drag_source".into(), Val::Bool(dragged)),
        ])
    }

    fn worktree_json(&self, w: &Worktree, section: Value) -> Value {
        let mut v = self.worktree_json_head(w, section);
        v["chats"] = Value::Array(self.chats_of(w).iter().map(|c| self.chat_json(c)).collect());
        v
    }

    /** A worktree's context value without its chats. */
    fn worktree_json_head(&self, w: &Worktree, section: Value) -> Value {
        let chats = self.chats_of(w);
        // Which status wins when several apply: `sidebar.status_order`.
        let offline = self.host_offline(&w.host);
        let moving = self.moving_to(w);
        let applies = |s: &str| match s {
            "offline" => offline,
            "moving" => moving.is_some(),
            "working" => chats.iter().any(|c| c.state == "working"),
            "waiting" => chats.iter().any(|c| c.state == "waiting"),
            "merged" => w.mr.as_ref().is_some_and(|m| m.state == "merged"),
            "failing" => w.mr.as_ref().and_then(|m| m.checks.as_deref()) == Some("failing"),
            "pending" => w.mr.as_ref().and_then(|m| m.checks.as_deref()) == Some("pending"),
            "dirty" => w.uncommitted > 0 || w.unpushed > 0,
            "behind" => w.behind,
            _ => false,
        };
        let order: Vec<String> = self
            .settings
            .get("sidebar.status_order")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
            .unwrap_or_else(|| ["moving", "offline", "working", "waiting", "merged", "failing", "pending", "dirty", "behind"].map(String::from).to_vec());
        let status = order.iter().find(|s| applies(s)).map(String::as_str).unwrap_or("idle");
        let (emoji, label) = split_emoji(&w.title);
        let (_, project_letter, hue) = self.project_look(&w.repo, &w.project, w.hue);
        json!({
            "id": w.id, "path": w.path, "branch": w.branch, "title": w.title, "emoji": emoji, "label": label,
            "project": w.project, "repo": w.repo, "hue": hue, "letter": project_letter, "is_primary": w.is_primary,
            "pinned": self.flag(&format!("pinned:{}", w.path)), "hidden": self.flag(&format!("hidden:{}", w.path)), "external": w.external,
            "uncommitted": w.uncommitted, "unpushed": w.unpushed, "behind": w.behind, "mr": w.mr,
            "age": w.age, "age_text": age_text(w.age, &self.age_format()), "status": status, "section": section,
            "selected": self.selected.as_deref() == Some(&w.path),
            "host": w.host, "offline": offline, "moving": moving,
        })
    }

    fn chat_json(&self, c: &Chat) -> Value {
        // One build of the context shows a chat several times (the chat list, its worktree, its row); the
        // memo lives only while one is being built.
        if let Some(v) = self.chat_json_memo.borrow().as_ref().and_then(|m| m.get(&c.id)) {
            return v.clone();
        }
        let v = self.chat_json_fresh(c);
        if let Some(m) = self.chat_json_memo.borrow_mut().as_mut() {
            m.insert(c.id.clone(), v.clone());
        }
        v
    }

    fn chat_json_fresh(&self, c: &Chat) -> Value {
        let mut v = serde_json::to_value(c).unwrap_or(Value::Null);
        if let Value::Object(o) = &mut v {
            let active = match self.building_current.borrow().as_ref() {
                Some(cur) => cur.as_deref() == Some(c.id.as_str()),
                None => self.current_chat().is_some_and(|x| x.id == c.id),
            };
            o.insert("active".into(), Value::Bool(active));
            o.insert("open".into(), Value::Bool(self.chat_open(c)));
            o.insert("moving".into(), json!(self.chat_moving_to(c)));
            o.insert("queued".into(), json!(self.queued.get(&c.id).cloned().unwrap_or_default()));
            o.insert("age_text".into(), Value::String(age_text(c.last_activity, &self.age_format())));
            o.insert("attachments".into(), self.attachments_json(&c.id));
            let last = match self.transcripts.get(&c.id) {
                Some(t) => Self::last_reply(t),
                None => self.dropped.get(&c.id).and_then(|d| d.0.clone()),
            };
            o.insert("asks".into(), Value::Bool(last.as_deref().is_some_and(|t| self.asks(t))));
            o.insert("last_message".into(), last.map(|t| Value::String(t.chars().take(self.settings.num("chat.last_message_chars", 280.) as usize).collect())).unwrap_or(Value::Null));
            let activity = self.transcripts.get(&c.id).filter(|_| c.state == "working").map(|t| t.activity()).unwrap_or_default();
            o.insert("activity".into(), Value::String(activity));
            let fg = c.state == "working" && self.transcripts.get(&c.id).is_some_and(|t| t.foreground_shell());
            o.insert("foreground_shell".into(), Value::Bool(fg));
            o.insert("orchestrator".into(), Value::Bool(self.is_orchestrator(c)));
            // Its machine is out of reach: what it was doing then isn't known now.
            if self.host_offline(&c.host) {
                o.insert("offline".into(), Value::Bool(true));
                o.insert("state".into(), json!("offline"));
            }
            // The model its newest reply came from, else the one it was started with (null: the agent's default).
            let used = self.transcripts.get(&c.id).and_then(|t| t.model.clone()).or_else(|| self.dropped.get(&c.id).and_then(|d| d.1.clone())).or_else(|| c.model.clone());
            o.insert("model_used".into(), json!(used));
        }
        v
    }

    /** Whether a chat leads its section (the sidebar draws the others under it): `chat.orchestrator`, a jq
    test of the chat, default its kind being "orchestrator". */
    pub(crate) fn is_orchestrator(&self, c: &Chat) -> bool {
        let Some(code) = self.settings.chat.get("orchestrator").and_then(Value::as_str) else { return c.kind == "orchestrator" };
        // What the test usually reads; a changed setting or chat tests again.
        let key = format!("{code}\u{0}{}\u{0}{}\u{0}{}", c.kind, c.name, c.dir.as_deref().unwrap_or(""));
        if let Some((k, yes)) = self.orchestrator_memo.borrow().get(&c.id) {
            if *k == key {
                return *yes;
            }
        }
        let yes = self.exprs.truthy(code, &crate::ui::expr::to_val(&serde_json::to_value(c).unwrap_or(Value::Null)));
        self.orchestrator_memo.borrow_mut().insert(c.id.clone(), (key, yes));
        yes
    }

    pub(crate) fn select(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        self.set_state("selected_worktree".into(), Value::String(path.clone()));
        self.selected = Some(path);
        self.sync_selection(window, cx);
        cx.notify();
    }

    pub(crate) fn sync_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(w) = self.current_worktree().cloned() else { return };
        if let Some(chat) = self.current_chat().cloned() {
            self.save_active_tab(&w.path, &chat.id);
            let seen = self.tab_history.entry(w.path.clone()).or_default();
            if seen.last() != Some(&chat.id) {
                seen.retain(|c| c != &chat.id);
                seen.push(chat.id.clone());
            }
            if !self.watching.contains(&chat.id) {
                self.watching.insert(chat.id.clone());
                let from = self.transcripts.get(&chat.id).map(|t| t.lines).unwrap_or(0);
                let remote = self.route(&Call::TranscriptUnwatch { chat_id: chat.id.clone() });
                let id = chat.id.clone();
                // Only the newest `chat.transcript_tail` entries on first load (dispatcher transcripts run to 80MB),
                // the last `transcript_first` of them first, to show sooner; the rest come right after.
                let n = |k: &str| self.settings.chat.get(k).and_then(Value::as_u64).map(|n| n as usize);
                let (all, first) = (n("transcript_tail"), n("transcript_first"));
                let tail = first.zip(all).map(|(f, a)| f.min(a)).or(all).filter(|_| from == 0);
                let rest = all.zip(tail).map(|(a, t)| a - t).filter(|r| *r > 0);
                let remote_host = !self.is_local_host(&chat.host);
                cx.spawn(async move |this, cx| {
                    let asked = std::time::Instant::now();
                    if let Ok(v) = remote.call(Call::TranscriptRead { chat_id: id.clone(), from, tail, before: None }).await {
                        let fetched = asked.elapsed();
                        let entries = v["entries"].as_array().cloned().unwrap_or_default();
                        let start = v["from"].as_u64().unwrap_or(from as u64) as usize;
                        this.update(cx, |ws, cx| {
                            let t = std::time::Instant::now();
                            ws.transcripts.entry(id.clone()).or_insert_with(|| crate::transcript::Transcript::for_chat(&id)).append(start, &entries);
                            ws.perf.borrow_mut().fetched(&id, fetched, t.elapsed(), entries.len());
                            if let Some(page) = rest.filter(|_| start > 0) {
                                // Over a slow link the rest waits a moment, and only if the chat's still shown,
                                // so switching through chats doesn't queue big pages ahead of the next one's first.
                                let wait = if remote_host { ws.settings.num("chat.transcript_rest_delay_ms", 1500.) } else { 0. };
                                if wait <= 0. {
                                    ws.load_earlier(&id, Some(page), cx);
                                } else {
                                    let id = id.clone();
                                    cx.spawn(async move |this, cx| {
                                        cx.background_executor().timer(Duration::from_millis(wait as u64)).await;
                                        let _ = this.update(cx, |ws, cx| {
                                            if ws.current_chat().is_some_and(|c| c.id == id) {
                                                ws.load_earlier(&id, Some(page), cx);
                                            }
                                        });
                                    })
                                    .detach();
                                }
                            }
                            cx.notify();
                        })
                        .ok();
                    }
                })
                .detach();
            }
            self.ensure_composer(&chat.id, window, cx);
        }
        // `terminal.open_on_select`: false leaves the terminal until it's shown (a `terminal` node draws it).
        let eager = self.settings.get("terminal.open_on_select").and_then(Value::as_bool) != Some(false);
        if eager && !self.terminals.contains_key(&w.path) && self.connected {
            let remote = self.remote_for_host(&w.host);
            let ts = self.settings.terminal.clone();
            let path = w.path.clone();
            let host = w.host.clone();
            let view = cx.new(|cx| TerminalView::new(remote, host, None, Some(path.clone()), &ts, cx));
            self.terminals.insert(w.path.clone(), view);
        }
    }

    /** Remembers a worktree's open tab across restarts (`state.active_tabs`, {path: chat id}, for the
    worktrees that still exist). */
    fn save_active_tab(&mut self, path: &str, chat: &str) {
        let tabs = self.state.get("active_tabs").cloned().unwrap_or_else(|| json!({}));
        if tabs.get(path).and_then(Value::as_str) == Some(chat) {
            return;
        }
        let live: HashSet<String> = self.all_worktrees().iter().map(|w| w.path.clone()).collect();
        let mut tabs: serde_json::Map<String, Value> = tabs.as_object().cloned().unwrap_or_default().into_iter().filter(|(p, _)| live.contains(p)).collect();
        tabs.insert(path.to_string(), Value::String(chat.to_string()));
        self.set_state("active_tabs".into(), Value::Object(tabs));
    }

    pub(crate) fn ensure_composer(&mut self, chat_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.composers.contains_key(chat_id) {
            return;
        }
        let designer = chat_id == crate::designer::CHAT_ID;
        let key = if designer { "designer_placeholder" } else { "placeholder" };
        let fallback = if designer { "Describe a change to the app…" } else { "Message the agent…" };
        let placeholder = self.settings.chat.get(key).and_then(Value::as_str).unwrap_or(fallback).to_string();
        // enter_sends: Enter sends and Shift+Enter starts a new line; otherwise Enter is a new line.
        // Cmd+Enter always sends.
        let enter_sends = self.settings.chat.get("enter_sends").and_then(Value::as_bool).unwrap_or(true);
        let cmd_enter_sends = self.settings.chat.get("cmd_enter_sends").and_then(Value::as_bool).unwrap_or(true);
        let draft = self.drafts_loaded.remove(chat_id).unwrap_or_default();
        let state = cx.new(|cx| TextareaState::new(window, cx).auto_grow(self.settings.num("chat.composer_lines.min", 1.) as usize, self.settings.num("chat.composer_lines.max", 10.) as usize).placeholder(placeholder).submit_on_enter(enter_sends).default_value(draft));
        let id = chat_id.to_string();
        self._subscriptions.push(cx.subscribe_in(&state, window, move |ws, _, ev: &InputEvent, window, cx| {
            match ev {
                InputEvent::PressEnter { secondary, shift } => {
                    let enter_accepts = ws.settings.get("chat.completion_builtin_keys").and_then(|k| k.get("enter")).map_or(true, |v| v == "accept");
                    if ws.completion.as_ref().is_some_and(|c| c.0 == id) && !*shift && enter_accepts {
                        ws.accept_completion(None, window, cx);
                    } else if (*secondary && cmd_enter_sends) || (enter_sends && !*shift && !*secondary) {
                        ws.send_composer(&id, window, cx);
                    }
                }
                InputEvent::Change => {
                    if let Some(c) = ws.composers.get(&id).cloned() {
                        let text = c.read(cx).value().to_string();
                        let old = ws.composer_texts.get(&id).cloned().unwrap_or_default();
                        let md = ws.live_md.entry(id.clone()).or_default();
                        if old != text {
                            let (start, old_end, new_end) = crate::live_markdown::edit(&old, &text);
                            crate::live_markdown::shift(&mut md.0, start, old_end, new_end);
                            md.1 = (old_end == start && new_end > start).then_some(new_end);
                            ws.sync_markdown(&id, window, cx);
                            ws.ctx_rev += 1;
                        } else {
                            // The same text with a hidden mark gone (undo after it formatted): keep it plain.
                            let now: Vec<_> = c.read(cx).tokens().iter().map(|s| s.range()).collect();
                            let gone: Vec<_> = md.2.iter().filter(|(r, _)| !now.contains(r)).map(|(r, _)| r.clone()).collect();
                            if !gone.is_empty() {
                                for k in crate::live_markdown::parse(&text, &md.0) {
                                    if k.pieces.iter().any(|(r, _)| gone.contains(r)) {
                                        md.0.insert(k.id);
                                    }
                                }
                                ws.sync_markdown(&id, window, cx);
                            }
                        }
                    }
                    ws.update_completion(&id, cx)
                }
                _ => {}
            }
        }));
        let (view, id) = (cx.entity().downgrade(), chat_id.to_string());
        state.update(cx, |s, _| {
            s.on_context_menu(std::rc::Rc::new(move |_, _, at, window, cx| {
                let _ = view.update(cx, |ws, cx| ws.composer_menu(&id, at, window, cx));
            }))
        });
        self.composers.insert(chat_id.to_string(), state);
        self.sync_markdown(chat_id, window, cx);
    }

    /** Opens, narrows, or closes the completion list from the word being typed (settings
    `chat.completion_triggers`, default "/"). */
    pub(crate) fn update_completion(&mut self, chat: &str, cx: &mut Context<Self>) {
        let Some(state) = self.composers.get(chat) else { return };
        let text = state.read(cx).value().to_string();
        let triggers: Vec<String> = self
            .settings
            .chat
            .get("completion_triggers")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
            .unwrap_or_else(|| vec!["/".into()]);
        let found = if chat == crate::designer::CHAT_ID { None } else { crate::completions::token(&text, &triggers) };
        let before = self.completion.as_ref().map(|c| (c.0.clone(), c.2.len()));
        self.completion = found.and_then(|(trigger, query)| {
            let w = self.all_worktrees().into_iter().find(|w| w.chat_ids.iter().any(|c| c == chat)).cloned();
            // Skills of a worktree on another machine come from this Mac's checkout of its repo (`chat.local_repo_path`).
            let template = self.settings.text_at("chat.local_repo_path", "~/dev/{project}");
            let local = w.as_ref().filter(|w| !self.is_local_host(&w.host)).map(|w| template.replace("{project}", &w.project).replace("{repo}", &w.repo));
            let all = crate::completions::entries(&self.settings, &trigger, w.as_ref().map(|w| w.path.as_str()), local.as_deref());
            let items = crate::completions::filter(&all, &query, self.settings.num("chat.completion_limit", 12.) as usize, self.settings.get("chat.completion_rank").and_then(Value::as_str).unwrap_or("default"));
            let keep = self.completion.as_ref().filter(|c| c.0 == chat).map(|c| c.3).unwrap_or(0);
            (!items.is_empty()).then(|| (chat.to_string(), trigger, items.clone(), keep.min(items.len().saturating_sub(1))))
        });
        if before != self.completion.as_ref().map(|c| (c.0.clone(), c.2.len())) || self.completion.is_some() {
            self.ctx_rev += 1;
            cx.notify();
        }
    }

    /** Replaces the word being typed with the chosen entry (`index`, default the highlighted one). */
    pub(crate) fn accept_completion(&mut self, index: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let Some((chat, trigger, items, selected)) = self.completion.take() else { return };
        let Some(entry) = items.get(index.unwrap_or(selected)) else { return };
        let Some(state) = self.composers.get(&chat).cloned() else { return };
        let text = state.read(cx).value().to_string();
        let cut = text.rfind(|c: char| c.is_whitespace()).map(|i| i + 1).unwrap_or(0);
        let next = format!("{}{}{} ", &text[..cut], trigger, entry.name);
        state.update(cx, |s, cx| {
            s.set_value(next, window, cx);
            s.focus(window, cx);
        });
        window.dispatch_action(Box::new(gpui_kit::component::input::MoveToEnd), cx);
        self.completion = None;
        self.ctx_rev += 1;
        cx.notify();
    }

    /** A key the message box binds itself (up, down, escape, tab, enter, shift-enter, cmd-enter), before
    the box acts on it: with the completion list open, `chat.completion_builtin_keys` says what it does
    there (next, prev, accept, close, or null to leave it to the box); otherwise `chat.composer_keys`
    {send, newline, suggestion} can take it. Unclaimed keys do what the box does. */
    fn composer_builtin_key(&mut self, chat: &str, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.completion.as_ref().is_some_and(|x| x.0 == chat) {
            let builtin = self.settings.get("chat.completion_builtin_keys").cloned().unwrap_or_else(|| json!({"up": "prev", "down": "next", "enter": "accept", "tab": "accept", "escape": "close"}));
            let what = builtin.get(key).and_then(Value::as_str).map(str::to_string);
            let handled = match what.as_deref() {
                Some("next") => {
                    self.move_completion(1, cx);
                    true
                }
                Some("prev") => {
                    self.move_completion(-1, cx);
                    true
                }
                Some("accept") => {
                    self.accept_completion(None, window, cx);
                    true
                }
                Some("close") => {
                    self.completion = None;
                    self.ctx_rev += 1;
                    cx.notify();
                    true
                }
                _ => false,
            };
            if handled {
                cx.stop_propagation();
                return;
            }
        }
        let has = |group: &str, default: &[&str]| self.settings.strings(&format!("chat.composer_keys.{group}"), default).iter().any(|k| k == key);
        if has("send", &[]) {
            self.send_composer(chat, window, cx);
            cx.stop_propagation();
        } else if has("newline", &[]) {
            if let Some(c) = self.composers.get(chat).cloned() {
                c.update(cx, |s, cx| s.insert("\n", window, cx));
            }
            cx.stop_propagation();
        } else if has("suggestion", &["tab"]) {
            self.on_tab(chat, window, cx);
        }
    }

    /** Fills in Claude Code's suggestion when the box is empty (Tab by default), or takes the highlighted completion. */
    fn on_tab(&mut self, chat: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.completion.as_ref().is_some_and(|x| x.0 == chat) {
            self.accept_completion(None, window, cx);
            cx.stop_propagation();
            return;
        }
        let suggestion = self.chat(chat).filter(|c| c.state == "idle").and_then(|c| c.suggestion.clone());
        if let (Some(text), Some(state)) = (suggestion, self.composers.get(chat).cloned()) {
            if state.read(cx).value().is_empty() {
                state.update(cx, |s, cx| s.set_value(text, window, cx));
                window.dispatch_action(Box::new(gpui_kit::component::input::MoveToEnd), cx);
                cx.stop_propagation();
            }
        }
    }

    /** Takes Claude Code's suggestion into the composer now, empty or not (`composer.accept_suggestion`). */
    pub(crate) fn on_tab_now(&mut self, chat: &str, window: &mut Window, cx: &mut Context<Self>) {
        let suggestion = self.chat(chat).and_then(|c| c.suggestion.clone());
        if let (Some(text), Some(state)) = (suggestion, self.composers.get(chat).cloned()) {
            state.update(cx, |s, cx| {
                s.set_value(text, window, cx);
                s.focus(window, cx);
            });
            window.dispatch_action(Box::new(gpui_kit::component::input::MoveToEnd), cx);
        }
    }

    /** Shows a composer's markdown as it will look once sent (`chat.live_markdown`): its marks hide
    (`hide_markers`, as atomic tokens the caret steps over) and `styles` paint the rest, by kind: marker,
    quote, heading, list, bold, italic, strike, code, code_block, link, url. */
    pub(crate) fn sync_markdown(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::input::InlineToken;
        let Some(c) = self.composers.get(id).cloned() else { return };
        let cfg = self.settings.chat.get("live_markdown").filter(|c| c.get("enabled").and_then(Value::as_bool) != Some(false)).cloned();
        let text = c.read(cx).value().to_string();
        self.composer_texts.insert(id.to_string(), text.clone());
        let literal = self.live_md.get(id).map(|m| m.0.clone()).unwrap_or_default();
        let found = if cfg.is_some() { crate::live_markdown::parse(&text, &literal) } else { vec![] };
        let hide = cfg.as_ref().is_some_and(|c| c.get("hide_markers").and_then(Value::as_bool) != Some(false));
        let want: Vec<(std::ops::Range<usize>, &str)> = if hide {
            let text = text.as_str();
            found.iter().flat_map(|k| k.pieces.iter().filter_map(move |p| crate::live_markdown::token_tag(k, p, text).map(|t| (p.0.clone(), t)))).collect()
        } else {
            vec![]
        };
        let have: Vec<(std::ops::Range<usize>, String)> = c.read(cx).tokens().iter().filter_map(|s| Some((s.range(), s.token().id().strip_prefix("md:")?.to_string()))).collect();
        // Tokens keep the text as it is, so no change moves another's offsets; the caret stays where it was.
        let sel = c.read(cx).selected_range();
        let changes = have.iter().any(|(r, t)| !want.iter().any(|(w, u)| w == r && u == t)) || want.iter().any(|(w, u)| !have.iter().any(|(r, t)| w == r && u == t));
        for (r, tag) in &have {
            if !want.iter().any(|(w, t)| w == r && t == tag) {
                let utf16 = text[..r.start].encode_utf16().count()..text[..r.end].encode_utf16().count();
                let plain = text[r.clone()].to_string();
                c.update(cx, |s, cx| gpui_kit::EntityInputHandler::replace_text_in_range(s, Some(utf16), &plain, window, cx));
            }
        }
        for (r, tag) in &want {
            if !have.iter().any(|(h, t)| h == r && t == tag) {
                let token = InlineToken::new(format!("md:{tag}"), text[r.clone()].to_string());
                c.update(cx, |s, cx| { let _ = s.replace_range_with_token(r.clone(), token, window, cx); });
            }
        }
        if changes {
            c.update(cx, |s, cx| s.set_selected_range(sel, cx));
        }
        if let Some(m) = self.live_md.get_mut(id) {
            m.2 = want.iter().map(|(r, t)| (r.clone(), t.to_string())).collect();
        }
        self.paint_composer(id, cx);
    }

    /** A composer's live markdown styles and spelling marks, painted over its text. */
    fn paint_composer(&mut self, id: &str, cx: &mut Context<Self>) {
        use gpui_kit::component::input::TextDecoration;
        let Some(c) = self.composers.get(id).cloned() else { return };
        let text = c.read(cx).value().to_string();
        let literal = self.live_md.get(id).map(|m| m.0.clone()).unwrap_or_default();
        let cfg = self.settings.chat.get("live_markdown").filter(|c| c.get("enabled").and_then(Value::as_bool) != Some(false));
        let styles = cfg.and_then(|c| c.get("styles")).and_then(Value::as_object).cloned().unwrap_or_default();
        let look = self.settings.chat.get("spellcheck").and_then(|s| s.get("style")).cloned().unwrap_or_else(|| json!({"underline": true, "wavy": true, "underline_color": "@danger"}));
        self.follow_spelling(id, &text, cx);
        let marks = self.spelling.get(id).map(|(w, _, _)| w.iter().filter(|r| r.end <= text.len()).map(|r| (r.clone(), crate::live_markdown::style(&look, &self.settings.theme))).collect()).unwrap_or_default();
        let decs = crate::live_markdown::decorations(&text, &literal, &styles, &self.settings.theme, marks);
        let decs = decs.into_iter().map(|(r, s)| TextDecoration::new(r, s)).collect();
        c.update(cx, |s, cx| s.set_text_decorations(decs, cx));
    }

    /** After an edit: marks after it move with their words, ones it touched go, and a check is due. */
    pub(crate) fn follow_spelling(&mut self, id: &str, text: &str, cx: &mut Context<Self>) -> bool {
        if self.settings.chat.get("spellcheck").and_then(|s| s.get("enabled")).and_then(Value::as_bool) == Some(false) {
            return self.spelling.remove(id).is_some();
        }
        match self.spelling.get_mut(id) {
            Some((_, _, seen)) if seen == text => return false,
            Some((words, _, seen)) => {
                let (start, old_end, new_end) = crate::live_markdown::edit(seen, text);
                let d = new_end as isize - old_end as isize;
                words.retain(|r| r.end < start || r.start > old_end);
                for r in words.iter_mut().filter(|r| r.start > old_end) {
                    *r = (r.start as isize + d) as usize..(r.end as isize + d) as usize;
                }
                *seen = text.to_string();
            }
            None => {
                self.spelling.insert(id.to_string(), (vec![], 0, text.to_string()));
            }
        }
        self.check_spelling(id, cx);
        true
    }

    /** Checks a composer's spelling once its typing pauses (`chat.spellcheck.delay_ms`), then repaints it. */
    pub(crate) fn check_spelling(&mut self, id: &str, cx: &mut Context<Self>) {
        let cfg = self.settings.chat.get("spellcheck").cloned().unwrap_or(Value::Null);
        let entry = self.spelling.entry(id.to_string()).or_default();
        entry.1 += 1;
        let (run, id) = (entry.1, id.to_string());
        let delay = Duration::from_millis(cfg.get("delay_ms").and_then(Value::as_u64).unwrap_or(400));
        let max = cfg.get("max_chars").and_then(Value::as_u64).unwrap_or(20000) as usize;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |ws, cx| {
                if ws.spelling.get(&id).map(|s| s.1) != Some(run) {
                    return;
                }
                let Some(c) = ws.composers.get(&id).cloned() else { return };
                let (text, sel) = { let s = c.read(cx); (s.value().to_string(), s.selected_range()) };
                let words = if text.len() > max { vec![] } else {
                    let literal = ws.live_md.get(&id).map(|m| m.0.clone()).unwrap_or_default();
                    use crate::live_markdown::Kind;
                    let skip: Vec<_> = crate::live_markdown::parse(&text, &literal).into_iter().flat_map(|k| k.pieces)
                        .filter(|(_, k)| matches!(k, Kind::Code | Kind::CodeBlock | Kind::Url | Kind::Marker)).map(|(r, _)| r).collect();
                    let lang = cfg.get("language").and_then(Value::as_str);
                    let found = crate::spelling::misspelled(&text, lang).into_iter().filter(|r| !ws.spell_ignored.contains(&text[r.clone()])).collect();
                    crate::spelling::shown(&text, found, &skip, sel.is_empty().then_some(sel.start))
                };
                ws.spelling.insert(id.clone(), (words, run, text));
                ws.paint_composer(&id, cx);
            });
        })
        .detach();
    }

    /** Right-click in a composer: `chat.composer_menu` at the pointer, with `.spelling` the misspelled word
    there ({chat, word, from, to, guesses}) if any. */
    pub(crate) fn composer_menu(&mut self, id: &str, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(c) = self.composers.get(id).cloned() else { return };
        let (text, sel) = { let s = c.read(cx); (s.value().to_string(), s.selected_range()) };
        let word = self.spelling.get(id).filter(|s| s.2 == text).and_then(|(w, _, _)| w.iter().find(|r| r.start <= sel.start && sel.end <= r.end && r.end <= text.len()).cloned());
        let spelling = word.map(|r| json!({"chat": id, "word": &text[r.clone()], "from": r.start, "to": r.end, "guesses": crate::spelling::guesses(&text, r.clone(), self.settings.chat.get("spellcheck").and_then(|s| s.get("language")).and_then(Value::as_str)).into_iter().take(6).collect::<Vec<_>>()}));
        let ctx = crate::ui::expr::with(&self.context(), vec![
            ("spelling".into(), crate::ui::expr::to_val(&spelling.unwrap_or(Value::Null))),
            ("composer".into(), crate::ui::expr::to_val(&json!({"chat": id, "selection": !sel.is_empty()}))),
        ]);
        let spec = self.settings.chat.get("composer_menu").and_then(Value::as_array).cloned().unwrap_or_default();
        // It opens over the chat's message box (`composer-box`), which the designer's isn't drawn in.
        if spec.is_empty() || id == crate::designer::CHAT_ID {
            return;
        }
        crate::ui::drive::toggle_menu(self, "composer-box", &spec, &ctx, at, window, cx);
    }

    /** `composer.edit {chat, op}`: cut, copy, paste or select_all in a chat's composer, as its keys would. */
    pub(crate) fn composer_edit(&mut self, chat: &str, op: &str, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::input as input;
        let Some(c) = self.composers.get(chat).cloned() else { return };
        c.update(cx, |s, cx| s.focus(window, cx));
        let action: Box<dyn gpui::Action> = match op {
            "cut" => Box::new(input::Cut),
            "copy" => Box::new(input::Copy),
            "paste" => Box::new(input::Paste),
            "select_all" => Box::new(input::SelectAll),
            _ => return,
        };
        window.dispatch_action(action, cx);
    }

    /** `spelling.correct {chat, from, to, text}`: puts `text` in place of the misspelled word at from..to. */
    pub(crate) fn spelling_correct(&mut self, chat: &str, from: usize, to: usize, word: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(c) = self.composers.get(chat).cloned() else { return };
        let text = c.read(cx).value().to_string();
        if to > text.len() || from > to || !text.is_char_boundary(from) || !text.is_char_boundary(to) {
            return;
        }
        let utf16 = text[..from].encode_utf16().count()..text[..to].encode_utf16().count();
        c.update(cx, |s, cx| {
            gpui_kit::EntityInputHandler::replace_text_in_range(s, Some(utf16), word, window, cx);
            s.focus(window, cx);
        });
    }

    /** `spelling.learn` / `spelling.ignore {word}`: stop marking a word, in every composer. */
    pub(crate) fn spelling_accept(&mut self, word: &str, learn: bool, cx: &mut Context<Self>) {
        if learn {
            crate::spelling::learn(word);
        } else {
            self.spell_ignored.insert(word.to_string());
        }
        let ids: Vec<String> = self.composers.keys().cloned().collect();
        for id in ids {
            self.check_spelling(&id, cx);
        }
    }

    /** The formatting a composer shows ({pieces: [{text, kind}], hidden: marks drawn otherwise, caret, misspelled}), for inspect.json. */
    pub(crate) fn composer_markdown(&self, id: &str, cx: &App) -> Value {
        let Some(c) = self.composers.get(id) else { return Value::Null };
        let text = c.read(cx).value().to_string();
        let literal = self.live_md.get(id).map(|m| m.0.clone()).unwrap_or_default();
        let pieces = crate::live_markdown::parse(&text, &literal).into_iter().flat_map(|k| k.pieces);
        let pieces: Vec<Value> = pieces.map(|(r, k)| json!({"text": text[r].to_string(), "kind": format!("{k:?}")})).collect();
        let tokens: Vec<Value> = c.read(cx).tokens().iter().map(|t| json!({"at": [t.range().start, t.range().end], "as": t.token().id().to_string()})).collect();
        let misspelled: Vec<&str> = self.spelling.get(id).map(|(w, _, _)| w.iter().filter_map(|r| text.get(r.clone())).collect()).unwrap_or_default();
        json!({"pieces": pieces, "hidden": tokens, "caret": c.read(cx).selected_range().start, "misspelled": misspelled, "spelling_current": self.spelling.get(id).is_some_and(|s| s.2 == text)})
    }

    /** Keys in a composer that act on its live markdown; true when one did and the key stops here.
    Backspace right after what completed some formatting (a line's "> " any time, a closing `**` just after
    typing it) keeps the text and shows it plain; otherwise backspace and the arrows step over hidden marks
    as if they weren't there. Shift-Enter in a quote or list continues it, and on an empty item ends it.
    Tab and Shift-Tab in a list nest its items a level in or out (the selected ones, or the caret's). */
    fn composer_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some((id, c)) = self.composers.iter().find(|(_, c)| c.read(cx).focus_handle(cx).is_focused(window)).map(|(i, c)| (i.clone(), c.clone())) else { return false };
        if self.settings.chat.get("live_markdown").and_then(|c| c.get("enabled")).and_then(Value::as_bool) == Some(false) {
            return false;
        }
        let (sel, text) = { let s = c.read(cx); (s.selected_range(), s.value().to_string()) };
        if key == "tab" || key == "outdent" {
            if self.completion.as_ref().is_some_and(|x| x.0 == id) {
                return false;
            }
            let literal = self.live_md.get(&id).map(|m| m.0.clone()).unwrap_or_default();
            let Some(r) = crate::live_markdown::reindent(&text, sel.clone(), key == "outdent", &literal) else { return false };
            if r.text != text[r.range.clone()] {
                let utf16 = |b: usize| text[..b].encode_utf16().count();
                let (from, to) = (r.map(sel.start), r.map(sel.end));
                c.update(cx, |s, cx| {
                    gpui_kit::EntityInputHandler::replace_text_in_range(s, Some(utf16(r.range.start)..utf16(r.range.end)), &r.text, window, cx);
                    s.set_selected_range(from..to, cx);
                });
            }
            return true;
        }
        if !sel.is_empty() {
            return false;
        }
        let at = sel.start;
        let hidden: Vec<std::ops::Range<usize>> = c.read(cx).tokens().iter().filter(|s| s.token().id() == "md:hide").map(|s| s.range()).collect();
        let md = self.live_md.entry(id.clone()).or_default();
        match key {
            "backspace" => {
                let typed = md.1;
                if let Some(k) = crate::live_markdown::parse(&text, &md.0).into_iter().find(|k| k.trigger_end == at && (k.block || typed == Some(at))) {
                    md.0.insert(k.id);
                    md.1 = None;
                    self.sync_markdown(&id, window, cx);
                    return true;
                }
                if let Some(r) = hidden.iter().find(|r| r.end == at) {
                    c.update(cx, |s, cx| s.set_selected_range(r.start..r.start, cx));
                }
                false
            }
            "left" | "right" => {
                let r = hidden.iter().find(|r| if key == "left" { r.end == at } else { r.start == at });
                if let Some(r) = r {
                    let to = if key == "left" { r.start } else { r.end };
                    c.update(cx, |s, cx| s.set_selected_range(to..to, cx));
                }
                false
            }
            _ => {
                let ls = text[..at].rfind('\n').map_or(0, |i| i + 1);
                let le = text[at..].find('\n').map_or(text.len(), |i| at + i);
                if md.0.contains(&ls) {
                    return false;
                }
                let Some((n, next)) = crate::live_markdown::continuation(&text[ls..le]) else { return false };
                if at < ls + n {
                    return false;
                }
                if text[ls + n..le].trim().is_empty() {
                    let utf16 = text[..ls].encode_utf16().count()..text[..le].encode_utf16().count();
                    c.update(cx, |s, cx| gpui_kit::EntityInputHandler::replace_text_in_range(s, Some(utf16), "", window, cx));
                } else {
                    c.update(cx, |s, cx| s.insert(format!("\n{next}"), window, cx));
                }
                true
            }
        }
    }

    /** `focus {area, chat?}`: put typing in a chat's composer ("composer", default the selected chat's) or
    the selected worktree's terminal ("terminal"); "none" takes focus off both. */
    pub(crate) fn focus_area_action(&mut self, area: &str, chat: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        match area {
            "terminal" => {
                let Some(path) = self.selected.clone() else { return };
                self.sync_selection(window, cx);
                let id = self.term_active(&path);
                if let Some(t) = self.terminals.get(&Self::term_key(&path, &id)).cloned() {
                    let h = t.read(cx).focus_handle(cx);
                    h.focus(window, cx);
                }
            }
            "none" => self.focus.focus(window, cx),
            _ => {
                let Some(id) = chat else { return };
                self.ensure_composer(&id, window, cx);
                if let Some(c) = self.composers.get(&id).cloned() {
                    c.update(cx, |s, cx| s.focus(window, cx));
                }
            }
        }
        cx.notify();
    }

    /** `composer.recall {by}`: steps through the chat's sent messages into its composer (-1: older,
    1: newer; past the newest leaves it empty). */
    pub(crate) fn recall_sent(&mut self, chat: &str, by: i64, window: &mut Window, cx: &mut Context<Self>) {
        let sent: Vec<String> = self.transcripts.get(chat).map(|t| t.items.iter().rev().filter_map(|i| if let Item::User { text } = i { Some(text.clone()) } else { None }).collect()).unwrap_or_default();
        let at = crate::behavior::recall_step(sent.len(), self.recall.get(chat).copied(), by);
        let text = match at {
            Some(i) => {
                self.recall.insert(chat.to_string(), i);
                sent[i].clone()
            }
            None => {
                self.recall.remove(chat);
                String::new()
            }
        };
        self.ensure_composer(chat, window, cx);
        if let Some(c) = self.composers.get(chat).cloned() {
            c.update(cx, |s, cx| {
                s.set_value(text, window, cx);
                s.focus(window, cx);
            });
            window.dispatch_action(Box::new(gpui_kit::component::input::MoveToEnd), cx);
        }
    }

    pub(crate) fn move_completion(&mut self, by: i64, cx: &mut Context<Self>) {
        if let Some(c) = &mut self.completion {
            let n = c.2.len() as i64;
            c.3 = ((c.3 as i64 + by).rem_euclid(n.max(1))) as usize;
            self.ctx_rev += 1;
            cx.notify();
        }
    }

    /** The dialog (settings `chat.takeover`) before a.box takes a chat over from the program running it. */
    pub(crate) fn takeover_confirm(&self, name: &str, then: Value) -> Value {
        let t = self.settings.chat.get("takeover").cloned().unwrap_or(Value::Null);
        let s = |k: &str, d: &str| t.get(k).and_then(Value::as_str).unwrap_or(d).replace("{name}", name);
        if t.get("confirm") == Some(&Value::Bool(false)) {
            return then;
        }
        json!({
            "do": "confirm",
            "title": s("title", "Take over “{name}”?"),
            "text": s("text", "It's running in super.engineering. a.box will close it there and continue the same conversation here."),
            "confirm": s("confirm_label", "Take it over"),
            "cancel": s("cancel_label", "Not now"),
            "then": then
        })
    }

    /** Sends the next queued message to each chat that's idle again. */
    pub(crate) fn flush_queue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ready: Vec<String> = self
            .queued
            .iter()
            .filter(|(_, q)| !q.is_empty())
            .filter(|(id, _)| self.chat(id).is_some_and(|c| c.alive && c.state == "idle"))
            .filter(|(id, _)| self.queue_sent.get(*id).map_or(true, |t| t.elapsed().as_secs_f64() > self.settings.num("chat.queue_resend_secs", 8.)))
            .map(|(id, _)| id.clone())
            .collect();
        for id in ready {
            let Some(text) = self.queued.get_mut(&id).filter(|q| !q.is_empty()).map(|q| q.remove(0)) else { continue };
            self.queue_sent.insert(id.clone(), std::time::Instant::now());
            self.send_chat(&id, text, window, cx);
            self.ctx_rev += 1;
        }
    }

    /** Sends settings `daemon` to a host's daemon (its timings, limits, parsing rules, and the box's ssh name). */
    fn configure_host(&self, host: &str) {
        let mut options = self.settings.get("daemon").cloned().unwrap_or(Value::Object(Map::new()));
        // The box's ssh name for the daemon and its scripts, unless `daemon.ssh_host` names one.
        if let Value::Object(o) = &mut options {
            if o.get("ssh_host").is_none_or(Value::is_null) {
                o.insert("ssh_host".into(), Value::String(self.ssh_host_of(&self.default_host())));
            }
        }
        // What this app's user set (their file's `daemon`, and the box's name when there is one): only that
        // overrides what other apps on the same daemon set; the rest are defaults.
        let mut own = settings::user_value().get("daemon").cloned().filter(Value::is_object).unwrap_or_else(|| json!({}));
        if let (Value::Object(o), Some(ssh)) = (&mut own, options.get("ssh_host").and_then(Value::as_str).filter(|s| !s.is_empty())) {
            o.entry("ssh_host").or_insert_with(|| json!(ssh));
        }
        let app = settings::dir().to_string_lossy().into_owned();
        self.remote_for_host(host).send(Call::Configure { options, own: Some(own), app: Some(app) });
    }

    /** Whether a reply asks you something: it ends with one of `chat.asks_endings` ("?"). */
    pub(crate) fn asks(&self, text: &str) -> bool {
        let t = text.trim_end();
        match self.settings.get("chat.asks_endings").and_then(Value::as_array) {
            Some(list) => list.iter().filter_map(Value::as_str).any(|e| !e.is_empty() && t.ends_with(e)),
            None => t.ends_with('?'),
        }
    }

    pub(crate) fn age_format(&self) -> Value {
        self.settings.get("format.age").cloned().unwrap_or(Value::Null)
    }

    /** Settings `background`: {image, fit, opacity, dim}: a picture behind the whole window (give panels
    see-through colors in `theme` so it shows). */
    fn background_layer(&self) -> Option<AnyElement> {
        let b = self.settings.get("background")?;
        let src = b.get("image").and_then(Value::as_str).filter(|s| !s.is_empty())?;
        let theme = &self.settings.theme;
        let fit = b.get("fit").and_then(Value::as_str).unwrap_or("cover");
        let opacity = b.get("opacity").and_then(Value::as_f64).unwrap_or(1.) as f32;
        let dim = b.get("dim").and_then(|d| style::color(d, theme));
        Some(
            div()
                .absolute()
                .inset_0()
                .child(div().size_full().opacity(opacity).child(media_element(src, fit)))
                .when_some(dim, |d, c| d.child(div().absolute().inset_0().bg(c)))
                .into_any_element(),
        )
    }

    /** A user-facing string: settings `strings.KEY` (else `default`), with `{name}` filled from `vars`. */
    pub(crate) fn tr(&self, key: &str, default: &str, vars: &[(&str, &str)]) -> String {
        let mut t = self.settings.text(key, default);
        for (k, v) in vars {
            t = t.replace(&format!("{{{k}}}"), v);
        }
        t
    }

    /** Keeps a notice in `.app.notifications` (the newest `notifications.keep`) for agents: `kind` is
    "toast", "system" (a banner) or "sound". */
    pub(crate) fn record_notice(&mut self, kind: &str, text: &str, title: Option<&str>) {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.);
        self.notices.push(json!({"at": now, "kind": kind, "text": text, "title": title}));
        let keep = self.settings.num("notifications.keep", 20.) as usize;
        if self.notices.len() > keep {
            let extra = self.notices.len() - keep;
            self.notices.drain(..extra);
        }
        self.ctx_rev += 1;
    }

    /** An in-window notification, kept in `.app.notifications` too. */
    pub(crate) fn toast(&mut self, text: impl Into<String>, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::WindowExt as _;
        let text: String = text.into();
        self.record_notice("toast", &text, None);
        if self.settings.get("notifications.show").and_then(Value::as_bool) != Some(false) {
            let autohide = self.settings.get("notifications.autohide").and_then(Value::as_bool) != Some(false);
            window.push_notification(gpui_kit::component::notification::Notification::new().message(text).autohide(autohide), cx);
        }
    }

    /** A chat shows as working from the moment you send, not from when its hook reports in a second later. */
    fn apply_just_sent(&mut self) {
        let hold = self.settings.num("chat.working_after_send_secs", 10.);
        self.just_sent.retain(|_, t| t.elapsed().as_secs_f64() < hold);
        let mut seen = vec![];
        for c in self.model.chats.iter_mut() {
            if self.just_sent.contains_key(&c.id) {
                if c.state == "idle" {
                    c.state = "working".into();
                } else {
                    seen.push(c.id.clone());
                }
            }
        }
        for id in seen {
            self.just_sent.remove(&id);
        }
    }

    /** Sends a message to a chat, showing it in the transcript at once. */
    /** Sends a message (a stopped or closed chat resumes first). One that doesn't go through comes back
    out of the transcript into the queue, with a toast saying why. */
    pub(crate) fn send_chat(&mut self, id: &str, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.just_sent.insert(id.to_string(), std::time::Instant::now());
        self.apply_just_sent();
        self.transcripts.entry(id.to_string()).or_default().echo(&text);
        let call = Call::ChatSend { chat_id: id.to_string(), text: text.clone() };
        let remote = self.route(&call);
        let id = id.to_string();
        cx.spawn_in(window, async move |this, cx| {
            if let Err(e) = remote.call(call).await {
                this.update_in(cx, |ws, window, cx| {
                    if let Some(t) = ws.transcripts.get_mut(&id) {
                        t.unecho(&text);
                    }
                    ws.just_sent.remove(&id);
                    ws.queued.entry(id.clone()).or_default().insert(0, text.clone());
                    ws.ctx_rev += 1;
                    let msg = ws.tr("send_failed", "Couldn't send: {why}", &[("why", &e.to_string())]);
                    ws.toast(msg, window, cx);
                })
                .ok();
            }
        })
        .detach();
    }

    /** Each new (worktree, chat) on screen is a visit; going back or forward moves along them. */
    fn track_nav(&mut self) {
        let Some(path) = self.selected.clone() else { return };
        let here = (path, self.current_chat().map(|c| c.id.clone()));
        if std::mem::take(&mut self.nav_jumping) {
            if let Some(slot) = self.nav.get_mut(self.nav_at) {
                *slot = here;
            }
            return;
        }
        if self.nav.get(self.nav_at) == Some(&here) {
            return;
        }
        self.nav.truncate(self.nav_at + 1);
        self.nav.push(here);
        if self.nav.len() > self.settings.num("nav.max", 200.).max(2.) as usize {
            self.nav.remove(0);
        }
        self.nav_at = self.nav.len() - 1;
        self.ctx_rev += 1;
    }

    /** Settings `mouse.back` / `mouse.forward` / `mouse.middle`: the action for that button (back and forward
    default to nav.back / nav.forward). */
    fn mouse_button(&mut self, which: &str, window: &mut Window, cx: &mut Context<Self>) {
        let default = match which {
            "back" => Value::String("nav.back".into()),
            "forward" => Value::String("nav.forward".into()),
            _ => Value::Null,
        };
        let action = self.settings.get(&format!("mouse.{which}")).cloned().unwrap_or(default);
        let ctx = self.context();
        let action = self.exprs.resolve_action(&action, &ctx);
        self.run_action(&action, window, cx);
    }

    /** Back (-1) or forward (1) to the nearest visit whose worktree is still here. */
    pub(crate) fn navigate(&mut self, step: i64, window: &mut Window, cx: &mut Context<Self>) {
        let mut i = self.nav_at as i64;
        loop {
            i += step;
            if i < 0 || i >= self.nav.len() as i64 {
                return;
            }
            if self.worktree(&self.nav[i as usize].0).is_some() {
                break;
            }
        }
        let (path, chat) = self.nav[i as usize].clone();
        self.nav_at = i as usize;
        self.nav_jumping = true;
        if let Some(c) = chat.filter(|c| self.chat(c).is_some_and(|c| self.chat_open(c))) {
            self.active_chat.insert(path.clone(), c);
        }
        self.select(path, window, cx);
    }

    fn can_navigate(&self, step: i64) -> bool {
        let mut i = self.nav_at as i64 + step;
        while i >= 0 && i < self.nav.len() as i64 {
            if self.worktree(&self.nav[i as usize].0).is_some() {
                return true;
            }
            i += step;
        }
        false
    }

    /** The gauge's mode: state `usage_meter` (the popover's picker) over settings `usage.meter`. */
    fn meter_options(&self) -> crate::usage::MeterOptions {
        let mode = self.state.get("usage_meter").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| self.settings.text("usage.meter", "smart"));
        let providers = self.settings.get("usage.meter_providers").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect());
        crate::usage::MeterOptions {
            mode,
            providers,
            warning_below: self.settings.num("usage.meter_levels.warning", 50.),
            danger_below: self.settings.num("usage.meter_levels.danger", 20.),
        }
    }

    /** What moving the selected chat takes along (settings `daemon.move_together`): `{scope, section,
    worktrees, chats}`, where scope is what actually applies (a chat outside any section moves its worktree). */
    fn move_plan(&self) -> Value {
        let Some(w) = self.current_worktree() else { return Value::Null };
        let wanted = self.settings.text("daemon.move_together", "section");
        let live = |w: &Worktree| self.chats_of(w).into_iter().filter(|c| !c.id.starts_with("sub:") && c.status != "archived").count();
        let section = (wanted == "section").then(|| self.model.sections.iter().find(|s| s.items.iter().any(|x| x.path == w.path && x.host == w.host))).flatten();
        match (wanted.as_str(), section) {
            ("chat", _) => json!({"scope": "chat", "section": null, "worktrees": 1, "chats": 1}),
            (_, Some(s)) => {
                let items: Vec<&Worktree> = s.items.iter().filter(|x| x.host == w.host).collect();
                json!({"scope": "section", "section": s.name, "worktrees": items.len(), "chats": items.iter().map(|x| live(x)).sum::<usize>()})
            }
            _ => json!({"scope": "worktree", "section": null, "worktrees": 1, "chats": live(w)}),
        }
    }

    /** What moving `chat` to `to` takes along, as `move_plan` works it out. */
    pub(crate) fn chat_move(&self, chat: &str, to: &str, scope: Option<&str>) -> ChatMove {
        let w = self.all_worktrees().into_iter().find(|w| w.chat_ids.iter().any(|c| c == chat));
        let scope = scope.map(str::to_string).unwrap_or_else(|| self.settings.text("daemon.move_together", "section"));
        let section = w.filter(|_| scope == "section").and_then(|w| self.model.sections.iter().find(|s| s.items.iter().any(|x| x.path == w.path && x.host == w.host)));
        let worktrees: Vec<&Worktree> = match (w, section) {
            (Some(w), Some(s)) => s.items.iter().filter(|x| x.host == w.host).collect(),
            (w, _) => w.into_iter().collect(),
        };
        let mut chats: HashSet<String> = if scope == "chat" { HashSet::new() } else { worktrees.iter().flat_map(|w| self.chats_of(w)).map(|c| c.id.clone()).collect() };
        chats.insert(chat.to_string());
        self.move_serial.set(self.move_serial.get() + 1);
        ChatMove { serial: self.move_serial.get(), to: to.to_string(), worktrees: worktrees.into_iter().map(move_key).collect(), chats }
    }

    /** Where a worktree is being moved: by a move this app started, else by `state.moving` (bin/chat's). */
    pub(crate) fn moving_to(&self, w: &Worktree) -> Option<String> {
        let key = move_key(w);
        if let Some(m) = self.moves.iter().find(|m| m.worktrees.contains(&key)) {
            return Some(m.to.clone());
        }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.);
        let mv = self.state.get("moving").filter(|v| v["at"].as_f64().is_some_and(|at| now - at < 600.))?;
        let branch = w.branch.as_deref()?;
        mv["branches"].as_array()?.iter().any(|b| b.as_str() == Some(branch)).then(|| mv["to"].as_str().unwrap_or_default().to_string())
    }

    pub(crate) fn chat_moving_to(&self, c: &Chat) -> Option<String> {
        if let Some(m) = self.moves.iter().find(|m| m.chats.contains(&c.id)) {
            return Some(m.to.clone());
        }
        let to = self.state.get("moving").and_then(|m| m["to"].as_str())?;
        (c.status == "moving").then(|| to.to_string())
    }

    /** Records a press for the innermost node that sees it (the first to call this for that event). */
    pub(crate) fn note_press(&mut self, id: &str, at: Point<Pixels>, count: usize) {
        let now = std::time::Instant::now();
        let dedupe = self.press_dedupe();
        if self.press.as_ref().is_some_and(|(_, t, p)| *p == at && now.duration_since(*t) < dedupe) {
            return;
        }
        self.press = Some((id.to_string(), now, at));
        if count == 1 {
            self.first_press = Some(id.to_string());
        }
        let (x, y) = (f32::from(at.x).round(), f32::from(at.y).round());
        if self.settings.get("inspect.log_input").and_then(Value::as_bool) == Some(true) {
            self.log_action(&json!({"input": "press", "node": id, "count": count, "x": x, "y": y}));
        }
    }

    /** Whether this event's press is this node's own: nothing inside it recorded one, and it isn't on a
    clickable node drawn inside it (by last frame's layout, so a button counts even when its hitbox
    missed). */
    pub(crate) fn press_is(&self, id: &str, at: Point<Pixels>) -> bool {
        let (x, y) = (f32::from(at.x), f32::from(at.y));
        let motion = self.motion.borrow();
        let inside = |b: &[f32; 4], o: &[f32; 4]| b[0] >= o[0] && b[1] >= o[1] && b[0] + b[2] <= o[0] + o[2] && b[1] + b[3] <= o[1] + o[3];
        if let Some(mine) = motion.bounds.get(id) {
            let on_child = self.clickables_last.iter().filter(|(cid, _, _)| cid != id).filter_map(|(cid, _, _)| motion.bounds.get(cid)).any(|b| {
                inside(b, mine) && x >= b[0] && x <= b[0] + b[2] && y >= b[1] && y <= b[1] + b[3]
            });
            if on_child {
                return false;
            }
        }
        match &self.press {
            Some((who, t, p)) if *p == at && t.elapsed() < self.press_dedupe() => who == id,
            _ => true,
        }
    }

    /** How long one press counts as the same press for every node it lands on (`input.press_dedupe_ms`). */
    fn press_dedupe(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.settings.num("input.press_dedupe_ms", 50.).max(0.) as u64)
    }

    pub(crate) fn first_press_is(&self, id: &str) -> bool {
        self.first_press.as_deref() == Some(id)
    }

    /** `inspect.log_input`: presses go in activity.log too, to see where clicks land. */
    pub(crate) fn log_input(&mut self, id: &str, count: usize, what: &str) {
        if self.settings.get("inspect.log_input").and_then(Value::as_bool) == Some(true) {
            self.log_action(&json!({"input": what, "node": id, "count": count}));
        }
    }

    /** Settings `usage.providers`: each provider's name, icon, models.dev rate tables and plan source. */
    fn usage_providers(&self) -> Map<String, Value> {
        self.settings.get("usage.providers").and_then(Value::as_object).map(|m| m.iter().filter(|(_, v)| v.is_object()).map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default()
    }

    /** Fetches every connected machine's token usage (or only `only`'s), reloads the plans' limits
    (refreshing them when stale or `force`), and models.dev rates once a day. */
    pub(crate) fn refresh_usage(&mut self, force: bool, only: Option<String>, cx: &mut Context<Self>) {
        let rates_path = settings::dir().join("usage-rates.json");
        let usage_providers = self.usage_providers();
        // models.dev tables to fetch: each provider's `rates` (its own id when unset).
        let mut providers: Vec<String> = vec![];
        for (id, p) in &usage_providers {
            let tables: Vec<String> = p["rates"].as_array().map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_else(|| vec![id.clone()]);
            for t in tables {
                if !providers.contains(&t) {
                    providers.push(t);
                }
            }
        }
        let cached = std::fs::read_to_string(&rates_path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok());
        let cached_has_all = cached.as_ref().is_some_and(|v| providers.iter().all(|p| v["providers"].get(p).is_some()));
        if self.usage.rates.is_empty() {
            if let Some(v) = cached.as_ref().filter(|_| cached_has_all) {
                self.usage.rates = v["providers"].as_object().cloned().unwrap_or_default();
                self.usage.rates_updated = v["fetched"].as_str().map(str::to_string);
            }
        }
        let max_age = self.settings.num("usage.rates_max_age_hours", 24.) * 3600.;
        let stale = !cached_has_all || std::fs::metadata(&rates_path).ok().and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok()).map_or(true, |e| e.as_secs_f64() > max_age);
        let url = self.settings.get("usage.rates_url").and_then(Value::as_str).unwrap_or("https://models.dev/api.json").to_string();
        if stale || force {
            cx.spawn(async move |this, cx| {
                let out = cx.background_spawn(async move { std::process::Command::new("/usr/bin/curl").args(["-sSL", "-m", "30", &url]).output() }).await;
                let Some(v) = out.ok().and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok()) else { return };
                let mut models = Map::new();
                for p in &providers {
                    let table: Map<String, Value> = v[p.as_str()]["models"].as_object().into_iter().flatten().filter_map(|(id, m)| m.get("cost").map(|c| (id.clone(), c.clone()))).collect();
                    if !table.is_empty() {
                        models.insert(p.clone(), Value::Object(table));
                    }
                }
                if models.is_empty() {
                    return;
                }
                let fetched = std::process::Command::new("date").arg("+%Y-%m-%d").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
                let _ = std::fs::write(settings::dir().join("usage-rates.json"), serde_json::to_string(&json!({"fetched": fetched, "providers": models})).unwrap_or_default());
                this.update(cx, |ws, cx| {
                    ws.usage.rates = models;
                    ws.usage.rates_updated = fetched;
                    let o = ws.settings.get("usage.rates").and_then(Value::as_object).cloned().unwrap_or_default();
                    ws.usage.recompute(&o);
                    ws.ctx_rev += 1;
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        self.configure_usage();
        // Plans read from a file (Claude Code's status line keeps ~/.claude/oauth-usage-cache.json fresh; a
        // `{source: "file", path, jq}` plan names its own): when it's older than the provider's
        // `plan_max_age_secs` (or on Refresh), run its `plan_command` (a file plan's `command`), if any.
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.);
        for (id, p) in &usage_providers {
            let (file, jq, command, updated) = match (p["plan"].as_str(), p["plan"]["source"].as_str()) {
                (Some("claude_oauth"), _) => {
                    let file = p["plan_file"].as_str().unwrap_or("~/.claude/oauth-usage-cache.json").to_string();
                    self.usage.load_plan(&file);
                    let command = p["plan_command"].as_str().or(self.settings.get("usage.plan_command").and_then(Value::as_str)).unwrap_or(PLAN_COMMAND).to_string();
                    (file, None, Some(command), self.usage.plan_updated)
                }
                (_, Some("file")) => {
                    let file = p["plan"]["path"].as_str().unwrap_or("").to_string();
                    let jq = p["plan"]["jq"].as_str().unwrap_or(".").to_string();
                    self.usage.load_file_plan(id, &file, &jq, &self.exprs);
                    let updated = self.usage.file_plans.get(id).and_then(|p| p.1);
                    (file, Some(jq), p["plan"]["command"].as_str().map(str::to_string), updated)
                }
                _ => continue,
            };
            let max_age = p["plan"]["max_age_secs"].as_f64().or(p["plan_max_age_secs"].as_f64()).unwrap_or_else(|| self.settings.num("usage.plan_max_age_secs", 300.));
            let Some(script) = command.filter(|_| force || updated.map_or(f64::MAX, |t| now - t) > max_age) else { continue };
            let (id, path) = (id.clone(), self.path_env());
            cx.spawn(async move |this, cx| {
                let _ = cx.background_spawn(async move { std::process::Command::new("/bin/sh").arg("-c").arg(script).env("PATH", path).output() }).await;
                this.update(cx, |ws, cx| {
                    match &jq {
                        None => ws.usage.load_plan(&file),
                        Some(jq) => ws.usage.load_file_plan(&id, &file, jq, &ws.exprs),
                    }
                    ws.ctx_rev += 1;
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        let hosts: Vec<(String, Remote)> = self
            .remotes
            .iter()
            .filter(|(h, _)| self.host_up.get(*h) == Some(&true) && only.as_ref().is_none_or(|o| o == *h))
            .map(|(h, r)| (h.clone(), r.clone()))
            .collect();
        if hosts.is_empty() {
            return;
        }
        self.usage.loading = true;
        cx.spawn(async move |this, cx| {
            for (host, r) in hosts {
                if let Ok(v) = r.call(Call::Usage).await {
                    this.update(cx, |ws, _| {
                        ws.usage.raw.insert(host.clone(), v);
                    })
                    .ok();
                }
            }
            this.update(cx, |ws, cx| {
                ws.usage.loading = false;
                let o = ws.settings.get("usage.rates").and_then(Value::as_object).cloned().unwrap_or_default();
                ws.usage.recompute(&o);
                ws.ctx_rev += 1;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /** What `usage` reads from settings: which host is this machine, rate fallbacks, strings. */
    fn configure_usage(&mut self) {
        self.usage.local_host = self.local_host();
        self.usage.rate_fallbacks = self.settings.get("usage.rate_fallbacks").and_then(Value::as_object).cloned().unwrap_or_default();
        self.usage.strings = self.settings.get("strings").cloned().unwrap_or(Value::Null);
    }

    /** A subagent tab is working while its parent still lists it as running. */
    fn refresh_sub_chats(&mut self) {
        for (_, c) in self.sub_chats.iter_mut() {
            let Some((parent, agent)) = c.id.strip_prefix("sub:").and_then(|r| r.split_once(':')) else { continue };
            let running = self.transcripts.get(parent).is_some_and(|t| {
                t.background().iter().any(|b| b.tool_id == agent || b.task_id.as_deref() == Some(agent))
            });
            let state = if running { "working" } else { "idle" };
            if c.state != state {
                c.state = state.into();
                self.ctx_rev += 1;
            }
        }
    }

    /** Queued messages survive a restart: kept in `queued.json` in the settings folder. */
    /** Unsent text in each message box survives a restart: `drafts.json` in the settings folder. */
    fn save_drafts(&mut self, cx: &mut Context<Self>) {
        let mut drafts: std::collections::BTreeMap<String, String> = self.drafts_loaded.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        for (id, c) in &self.composers {
            let v = c.read(cx).value().to_string();
            if v.trim().is_empty() {
                drafts.remove(id);
            } else {
                drafts.insert(id.clone(), v);
            }
        }
        let text = serde_json::to_string_pretty(&drafts).unwrap_or_default();
        if text != self.drafts_saved {
            let _ = std::fs::write(settings::dir().join("drafts.json"), &text);
            self.drafts_saved = text;
        }
    }

    fn save_queue(&mut self) {
        self.queued.retain(|_, q| !q.is_empty());
        let text = serde_json::to_string_pretty(&self.queued).unwrap_or_default();
        if text != self.queue_saved {
            let _ = std::fs::write(settings::dir().join("queued.json"), &text);
            self.queue_saved = text;
        }
    }

    pub(crate) fn send_composer(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.composers.get(id).cloned() else { return };
        let typed = state.read(cx).value().trim().to_string();
        let Some(paths) = self.take_attachments(id) else {
            self.send_when_ready.insert(id.to_string());
            let t = self.tr("sending_after_upload", "Sending when the upload finishes", &[]);
            self.toast(t, window, cx);
            return;
        };
        let typed = crate::ui::quote_view::with_quotes(&self.quotes.remove(id).unwrap_or_default(), &typed);
        let text = [typed, paths.join(" ")].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
        if text.is_empty() {
            return;
        }
        // A chat another program (super.engineering) is still running: hold the message and offer to take it over.
        if self.chat(id).is_some_and(|c| c.status == "running_elsewhere") {
            self.queued.entry(id.to_string()).or_default().push(text);
            let name = self.chat(id).map(|c| c.name.clone()).unwrap_or_default();
            let ask = json!({"do": "chat.open", "chat": id, "force": true});
            self.run_action(&self.takeover_confirm(&name, ask), window, cx);
            state.update(cx, |s, cx| s.set_value("", window, cx));
            return;
        }
        let busy = self.chat(id).is_some_and(|c| c.state == "working" || c.state == "waiting") || self.queued.get(id).is_some_and(|q| !q.is_empty());
        let queue = self.settings.chat.get("queue").and_then(Value::as_bool) != Some(false);
        if id == crate::designer::CHAT_ID {
            self.designer_send(text, cx);
        } else if busy && queue {
            self.queued.entry(id.to_string()).or_default().push(text);
            self.ctx_rev += 1;
        } else {
            self.send_chat(id, text, window, cx);
        }
        self.jump_to_latest(id);
        self.recall.remove(id);
        state.update(cx, |s, cx| s.set_value("", window, cx));
        cx.notify();
    }

    fn register_terminals(&mut self, cx: &mut Context<Self>) {
        for t in self.terminals.values() {
            let t_ref = t.read(cx);
            if let Some(id) = t_ref.term_id.filter(|id| *id != 0) {
                self.term_by_id.entry((t_ref.host.clone(), id)).or_insert_with(|| t.clone());
            }
        }
    }

    pub(crate) fn call(&self, call: Call, cx: &mut Context<Self>) {
        let remote = self.route(&call);
        cx.spawn(async move |this, cx| {
            if let Err(e) = remote.call(call).await {
                this.update(cx, |ws, cx| {
                    ws.pending_actions.push(json!({"do": "notify", "text": ws.tr("action_failed", "That didn't work: {error}", &[("error", &e.to_string())])}));
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /** Starts a chat (new or forked); with `select`, shows it once it appears. */
    pub(crate) fn start_chat(&mut self, call: Call, select: bool, cx: &mut Context<Self>) {
        let remote = self.route(&call);
        self.status = self.tr("starting_chat", "Starting a chat…", &[]);
        cx.spawn(async move |this, cx| {
            let res = remote.call(call).await;
            this.update(cx, |ws, cx| {
                match res {
                    Ok(v) => {
                        if let Some(id) = v["stdout"].as_str().and_then(|o| o.split_whitespace().next()).filter(|_| select) {
                            ws.pending_select = Some(id.to_string());
                        }
                        ws.status = ws.tr("connected", "Connected", &[]);
                    }
                    Err(e) => {
                        ws.status = ws.tr("start_failed", "Couldn't start the chat: {error}", &[("error", &e.to_string())]);
                        ws.starting = None;
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /** A dialog of labeled text fields; `done` gets their values in order when saved (Enter in any field saves).
    Agents see it as `.app.dialog` and answer it with `dialog.submit` / `dialog.cancel`. */
    pub(crate) fn fields_dialog(
        &mut self,
        title: &str,
        fields: Vec<(String, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
        done: impl Fn(&mut Workspace, Vec<String>, &mut Context<Workspace>) + 'static,
    ) {
        self.open_fields(title, fields, false, window, cx, done);
    }

    fn open_fields(
        &mut self,
        title: &str,
        fields: Vec<(String, String)>,
        need_first: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
        done: impl Fn(&mut Workspace, Vec<String>, &mut Context<Workspace>) + 'static,
    ) {
        use gpui_kit::component::input::{Input, InputState};
        use gpui_kit::component::WindowExt as _;
        let labels: Vec<String> = fields.iter().map(|(l, _)| l.clone()).collect();
        // `views.dialog` draws text dialogs from settings: the fields are state `dialog_field:N`.
        if self.settings.views.contains_key("dialog") {
            for (i, (_, v)) in fields.iter().enumerate() {
                self.state.insert(format!("dialog_field:{i}"), Value::String(v.clone()));
            }
            let done: std::rc::Rc<dyn Fn(&mut Workspace, Vec<String>, &mut Context<Workspace>)> = std::rc::Rc::new(done);
            self.dialog = Some(OpenDialog { title: title.to_string(), labels, inputs: vec![], need_first, done, view: true });
            self.ctx_rev += 1;
            return;
        }
        let inputs: Vec<(SharedString, Entity<InputState>)> =
            fields.into_iter().map(|(label, value)| (label.into(), cx.new(|cx| InputState::new(window, cx).default_value(value)))).collect();
        let view = cx.entity();
        let title: SharedString = title.to_string().into();
        let done = std::rc::Rc::new(done);
        let submit: std::rc::Rc<dyn Fn(&mut Window, &mut App)> = {
            let (inputs, view, done) = (inputs.clone(), view.clone(), done.clone());
            std::rc::Rc::new(move |window: &mut Window, cx: &mut App| {
                let vals: Vec<String> = inputs.iter().map(|(_, i)| i.read(cx).value().trim().to_string()).collect();
                if need_first && vals.first().is_none_or(|v| v.is_empty()) {
                    return;
                }
                let done = done.clone();
                view.update(cx, |ws, cx| {
                    ws.dialog = None;
                    done(ws, vals, cx)
                });
                window.close_dialog(cx);
            })
        };
        // Enter arrives while the workspace is already being updated, so it saves through `ws`, not `submit`.
        for (_, input) in &inputs {
            self._subscriptions.push(cx.subscribe_in(input, window, move |ws, _, ev: &InputEvent, window, cx| {
                if matches!(ev, InputEvent::PressEnter { .. }) {
                    ws.submit_dialog(None, window, cx);
                }
            }));
        }
        let focus = inputs.first().map(|i| i.1.clone());
        let save: SharedString = self.tr("dialog_save", "Save", &[]).into();
        let shown = inputs.clone();
        let button = submit.clone();
        let single = inputs.len() == 1;
        let title_text = title.to_string();
        window.open_dialog(cx, move |dialog, _, _| {
            let submit = button.clone();
            let body = v_flex().gap_3().children(shown.iter().map(|(label, input)| {
                v_flex().gap_1().when(!single, |d| d.child(div().text_sm().child(label.clone()))).child(Input::new(input))
            }));
            dialog.title(title.clone()).child(body).footer(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(gpui_kit::component::button::Button::new("ok").label(save.clone()).on_click(move |_, window, cx| submit(window, cx))),
            )
        });
        let done: std::rc::Rc<dyn Fn(&mut Workspace, Vec<String>, &mut Context<Workspace>)> = done;
        self.dialog = Some(OpenDialog { title: title_text, labels, inputs, need_first, done, view: false });
        if let Some(f) = focus {
            use gpui_kit::component::input::RopeExt as _;
            f.update(cx, |s, cx| {
                let end = s.text().offset_to_position(s.text().len());
                s.set_cursor_position(end, window, cx)
            });
        }
    }

    /** One text field; Save does nothing while it's empty. */
    pub(crate) fn name_dialog(
        &mut self,
        title: &str,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
        done: impl Fn(&mut Workspace, String, &mut Context<Workspace>) + 'static,
    ) {
        let label = self.tr("dialog_name", "Name", &[]);
        self.open_fields(title, vec![(label, value.to_string())], true, window, cx, move |ws, mut vals, cx| done(ws, vals.remove(0), cx));
    }

    /** `dialog.submit {values?}`: fill the open dialog's fields (in order) and save it. */
    pub(crate) fn submit_dialog(&mut self, values: Option<Vec<String>>, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::WindowExt as _;
        let Some(d) = self.dialog.as_ref() else { return };
        let vals: Vec<String> = if d.view {
            let n = d.labels.len();
            let field = |ws: &Self, i: usize| ws.state.get(&format!("dialog_field:{i}")).map(crate::ui::render::value_text).unwrap_or_default();
            match values {
                Some(v) => v.into_iter().map(|x| x.trim().to_string()).chain(std::iter::repeat(String::new())).take(n).collect(),
                None => (0..n).map(|i| field(self, i).trim().to_string()).collect(),
            }
        } else {
            if let Some(values) = values {
                for ((_, input), v) in d.inputs.iter().zip(values) {
                    input.update(cx, |s, cx| s.set_value(v, window, cx));
                }
            }
            d.inputs.iter().map(|(_, i)| i.read(cx).value().trim().to_string()).collect()
        };
        let d = self.dialog.as_ref().expect("checked above");
        if d.need_first && vals.first().is_none_or(|v| v.is_empty()) {
            return;
        }
        let (done, view, n) = (d.done.clone(), d.view, d.labels.len());
        self.dialog = None;
        if view {
            self.clear_dialog_fields(n);
        }
        done(self, vals, cx);
        if !view {
            window.close_dialog(cx);
        }
    }

    fn clear_dialog_fields(&mut self, n: usize) {
        for i in 0..n {
            self.state.remove(&format!("dialog_field:{i}"));
            self.user_state.remove(&format!("dialog_field:{i}"));
        }
        self.save_state();
        self.ctx_rev += 1;
    }

    pub(crate) fn cancel_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::WindowExt as _;
        if let Some(d) = self.dialog.take() {
            if d.view {
                self.clear_dialog_fields(d.labels.len());
            } else {
                window.close_dialog(cx);
            }
        }
    }

    /** Working state an agent can't see in the context: back/forward history, recent sends, which chats
    were opened or closed by hand, focus, subagent tabs, hook events, the open dialog and notifications. */
    pub(crate) fn internal_json(&self) -> Value {
        json!({
            "nav": {"at": self.nav_at, "visits": self.nav.iter().map(|(w, c)| json!({"worktree": w, "chat": c})).collect::<Vec<_>>()},
            "just_sent": self.just_sent.iter().map(|(k, t)| (k.clone(), json!(t.elapsed().as_secs_f32()))).collect::<Map<_, _>>(),
            "queue_sent": self.queue_sent.iter().map(|(k, t)| (k.clone(), json!(t.elapsed().as_secs_f32()))).collect::<Map<_, _>>(),
            "opened": self.opened.iter().collect::<Vec<_>>(),
            "closed": self.closed.iter().collect::<Vec<_>>(),
            "subagent_tabs": self.sub_chats.iter().map(|(w, c)| json!({"worktree": w, "chat": c.id, "name": c.name, "state": c.state})).collect::<Vec<_>>(),
            "hooks": self.hooks,
            "palette_open": self.palette_open,
            "caches": self.cache_sizes(),
        })
    }

    /** How much each long-lived cache holds, to spot one that only grows. */
    fn cache_sizes(&self) -> Value {
        let (motion, bounds, tracked, owners) = self.motion.borrow().sizes();
        json!({
            "exprs": self.exprs.cached(),
            "transcripts": self.transcripts.len(),
            "transcript_items": self.transcripts.values().map(|t| t.items.len()).sum::<usize>(),
            "images": self.thumbs.len(),
            "images_kb": self.thumbs.kb(),
            "terminals": self.terminals.len(),
            "regions": self.regions.len(),
            "kept": self.kept.values().map(|k| k.clickables.len() + k.menus.len() + k.tooltips.len() + k.drops.len() + k.hovers.len() + k.keys.len()).sum::<usize>(),
            "motion": {"entries": motion, "bounds": bounds, "tracked": tracked, "owners": owners},
            "entered": self.entered.len(),
            "transitions": self.transitions.len(),
            "lists": self.lists.len(),
            "virtual_lists": self.virtual_lists.len(),
            "diffs": self.diffs.len(),
            "live_md": self.live_md.len(),
            "spelling": self.spelling.len(),
            "composers": self.composers.len(),
            "inputs": self.inputs.len(),
            "hash_memo": self.hash_memo.borrow().len(),
            "orchestrator_memo": self.orchestrator_memo.borrow().len(),
            "scroll_handles": self.scroll_handles.len(),
            "region_deps": self.region_deps.len(),
            "anim_slots": crate::ui::anim::count(),
            "traffic": crate::remote::traffic(),
        })
    }

    /** The open dialog for inspect.json: title and each field's label and current text. */
    pub(crate) fn dialog_json(&self, cx: &App) -> Value {
        self.dialog.as_ref().map(|d| {
            let value = |i: usize| match d.inputs.get(i) {
                Some((_, input)) => input.read(cx).value().to_string(),
                None => self.state.get(&format!("dialog_field:{i}")).map(crate::ui::render::value_text).unwrap_or_default(),
            };
            json!({
                "title": d.title,
                "fields": d.labels.iter().enumerate().map(|(i, l)| json!({"label": l, "value": value(i)})).collect::<Vec<_>>(),
            })
        }).unwrap_or(Value::Null)
    }

    /** A new section named `name` (asks when it's missing) on `host` (default: the worktree's, else the
    default host's), and moves `assign` into it. */
    pub(crate) fn new_section(&mut self, name: Option<String>, assign: Option<String>, host: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let create = move |ws: &mut Workspace, name: String, cx: &mut Context<Workspace>| {
            let host = host.clone().or_else(|| assign.as_deref().map(|p| ws.host_of_path(p))).unwrap_or_else(|| ws.default_host());
            let remote = ws.remote_for_host(&host);
            let assign = assign.clone();
            cx.spawn(async move |_, _| {
                // The new section's id comes back with it, so the worktree moves in right away.
                if let Ok(v) = remote.call(Call::SectionCreate { name }).await {
                    let id = v["response"]["section_id"].as_str().or(v["section_id"].as_str()).map(str::to_string);
                    if let (Some(path), Some(id)) = (assign, id) {
                        let _ = remote.call(Call::SectionAssign { path, section: Some(id) }).await;
                    }
                }
            })
            .detach();
        };
        match name.filter(|n| !n.trim().is_empty()) {
            Some(n) => create(self, n, cx),
            None => {
                let title = self.tr("new_section_title", "New section", &[]);
                self.name_dialog(&title, "", window, cx, move |ws, name, cx| create(ws, name, cx));
            }
        }
    }

    pub(crate) fn run_local(&mut self, args: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.run_local_then(args, window, cx, |_, _| {});
    }

    /** `run_local`, then `after` once the command exits. */
    pub(crate) fn run_local_then(&mut self, args: Vec<String>, window: &mut Window, cx: &mut Context<Self>, after: impl FnOnce(&mut Self, &mut Context<Self>) + 'static) {
        let mut cmd = std::process::Command::new(self.bin_dir().join(&args[0]));
        cmd.args(&args[1..]);
        self.run_command(cmd, true, window, cx, after);
    }

    pub(crate) fn run_shell(&mut self, script: String, env: Vec<(String, String)>, notify: bool, window: &mut Window, cx: &mut Context<Self>) {
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.arg("-c").arg(script).envs(env);
        self.run_command(cmd, notify, window, cx, |_, _| {});
    }

    /** Runs a Mac command off the UI thread with a usable PATH; `notify` shows its last output line. */
    fn run_command(&mut self, mut cmd: std::process::Command, notify: bool, window: &mut Window, cx: &mut Context<Self>, after: impl FnOnce(&mut Self, &mut Context<Self>) + 'static) {
        cmd.env("PATH", self.path_env());
        if notify {
            let t = self.tr("working", "Working…", &[]);
            self.toast(t, window, cx);
        }
        let (done, failed) = (self.tr("done", "Done", &[]), self.tr("failed", "Failed", &[]));
        cx.spawn_in(window, async move |this, cx| {
            let out = cx.background_spawn(async move { cmd.output() }).await;
            this.update(cx, |ws, cx| {
                after(ws, cx);
                cx.notify();
            })
            .ok();
            if !notify {
                return;
            }
            let msg = match out {
                Ok(o) => {
                    let text = String::from_utf8_lossy(if o.status.success() { &o.stdout } else { &o.stderr }).trim().to_string();
                    text.lines().last().map(str::to_string).unwrap_or(if o.status.success() { done } else { failed })
                }
                Err(e) => e.to_string(),
            };
            this.update_in(cx, |ws, window, cx| ws.toast(msg, window, cx)).ok();
        })
        .detach();
    }

    /** The command palette's entries: settings `commands` (with `if` applied), then one per worktree
    (`palette.worktrees`, labeled by `palette.worktree_label`: a template with the worktree as `.w`, or
    `{title}`, `{branch}`, `{project}` placeholders). */
    pub(crate) fn palette_entries(&self) -> Vec<(String, Value)> {
        let ctx = self.context();
        let mut entries: Vec<(String, Value)> = vec![];
        for c in self.settings.commands.clone() {
            if let Some(cond) = c.get("if").and_then(Value::as_str) {
                if !self.exprs.truthy(cond, &ctx) {
                    continue;
                }
            }
            let label = value_text(&self.exprs.resolve(c.get("label").unwrap_or(&Value::Null), &ctx));
            entries.push((label, self.exprs.resolve_action(c.get("do").unwrap_or(&Value::Null), &ctx)));
        }
        if self.settings.get("palette.worktrees").and_then(Value::as_bool) != Some(false) {
            let fmt = self.settings.get("palette.worktree_label").and_then(Value::as_str).unwrap_or("Go to {title}").to_string();
            for w in self.all_worktrees() {
                let label = if fmt.contains("{{") {
                    let c = crate::ui::expr::with(&ctx, vec![("w".into(), to_val(&self.worktree_json(w, Value::Null)))]);
                    value_text(&self.exprs.resolve(&Value::String(fmt.clone()), &c))
                } else {
                    fmt.replace("{title}", &w.title).replace("{branch}", w.branch.as_deref().unwrap_or("")).replace("{project}", &w.project)
                };
                entries.push((label, json!({"do": "select", "worktree": w.path})));
            }
        }
        entries
    }

    /** `palette.run {label | index}`: run a palette entry without opening it (label: exact, else the first containing it). */
    pub(crate) fn run_palette(&mut self, label: Option<String>, index: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let entries = self.palette_entries();
        let pick = match (index, label) {
            (Some(i), _) => entries.get(i).cloned(),
            (None, Some(l)) => {
                let low = l.to_lowercase();
                entries.iter().find(|(x, _)| *x == l).or_else(|| entries.iter().find(|(x, _)| x.to_lowercase().contains(&low))).cloned()
            }
            _ => None,
        };
        match pick {
            Some((_, action)) => self.run_action(&action, window, cx),
            None => self.exprs.note("palette.run: no such entry".to_string()),
        }
    }

    /** The palette's entries that match what's typed (state `palette_query`), for `views.palette`. */
    pub(crate) fn palette_matches(&self) -> Vec<(usize, String, Value)> {
        let q = self.state.get("palette_query").map(value_text).unwrap_or_default().to_lowercase();
        self.palette_entries().into_iter().enumerate().filter(|(_, (l, _))| q.is_empty() || l.to_lowercase().contains(&q)).map(|(i, (l, a))| (i, l, a)).collect()
    }

    /** `.app.palette` while `views.palette` draws it: `{query, selected, entries: [{index, label, selected}]}`. */
    fn palette_json(&self, p: &PaletteView) -> Value {
        let matches = self.palette_matches();
        let sel = p.selected.min(matches.len().saturating_sub(1));
        json!({
            "query": self.state.get("palette_query").map(value_text).unwrap_or_default(),
            "selected": sel,
            "entries": matches.iter().enumerate().map(|(k, (i, l, _))| json!({"index": i, "label": l, "selected": k == sel})).collect::<Vec<_>>(),
        })
    }

    /** `palette.move {by}`, `palette.confirm {index?}` (an entry's `index`, default the highlighted one), `palette.close`. */
    pub(crate) fn palette_action(&mut self, name: &str, a: &Map<String, Value>, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::WindowExt as _;
        match name {
            "palette.move" => {
                let n = self.palette_matches().len().max(1) as i64;
                if let Some(p) = self.palette_view.as_mut() {
                    p.selected = ((p.selected as i64 + a.get("by").and_then(Value::as_i64).unwrap_or(1)).rem_euclid(n)) as usize;
                }
            }
            "palette.confirm" => {
                let pick = match a.get("index").and_then(Value::as_u64) {
                    Some(i) => self.palette_entries().get(i as usize).map(|(_, a)| a.clone()),
                    None => {
                        let sel = self.palette_view.as_ref().map(|p| p.selected).unwrap_or(0);
                        let m = self.palette_matches();
                        m.get(sel.min(m.len().saturating_sub(1))).map(|(_, _, a)| a.clone())
                    }
                };
                self.close_palette(window, cx);
                if let Some(action) = pick {
                    self.pending_actions.push(action);
                }
            }
            _ => {
                if self.palette_view.is_some() {
                    self.close_palette(window, cx);
                } else if self.palette_open {
                    self.palette_open = false;
                    window.close_dialog(cx);
                }
            }
        }
        self.ctx_rev += 1;
        cx.notify();
    }

    fn close_palette(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.palette_view = None;
        self.palette_open = false;
        self.state.remove("palette_query");
        self.user_state.remove("palette_query");
        self.ctx_rev += 1;
    }

    pub(crate) fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::command::{Command, CommandItem, CommandState};
        use gpui_kit::component::WindowExt as _;
        // `views.palette` draws the palette from settings instead (`.app.palette`, palette.* actions).
        if self.settings.views.contains_key("palette") {
            self.state.insert("palette_query".into(), Value::String(String::new()));
            self.palette_view = Some(PaletteView::default());
            self.palette_open = true;
            self.ctx_rev += 1;
            cx.notify();
            return;
        }
        let entries = self.palette_entries();
        let placeholder: SharedString = self.settings.get("palette.placeholder").and_then(Value::as_str).unwrap_or("Type a command or a worktree…").to_string().into();
        let state = cx.new(|cx| CommandState::new(window, cx));
        let view = cx.entity();
        let entries = std::rc::Rc::new(entries);
        self.palette_open = true;
        window.open_dialog(cx, move |dialog, _, _| {
            let (state, view, entries, placeholder) = (state.clone(), view.clone(), entries.clone(), placeholder.clone());
            dialog.close_button(false).p_0().content(move |content, _, _| {
                let (view, entries) = (view.clone(), entries.clone());
                content.child(
                    Command::new(&state)
                        .bordered(false)
                        .placeholder(placeholder.clone())
                        .items(entries.iter().map(|(label, _)| CommandItem::new().label(label.clone())).collect::<Vec<_>>())
                        .on_confirm(move |index, window, cx| {
                            if let Some((_, action)) = entries.get(index.row) {
                                let action = action.clone();
                                window.close_dialog(cx);
                                view.update(cx, |ws, cx| {
                                    ws.palette_open = false;
                                    ws.pending_actions.push(action);
                                    cx.notify();
                                });
                            }
                        }),
                )
            })
        });
    }

    pub(crate) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::input::{Editor, EditorState};
        use gpui_kit::component::{Placement, WindowExt as _};
        crate::ui::docs::write_reference();
        // `views.settings` draws the panel from settings instead (`.app.settings_sheet`, settings.save / settings.close).
        if self.settings.views.contains_key("settings") {
            self.settings_sheet = true;
            self.ctx_rev += 1;
            cx.notify();
            return;
        }
        let text = std::fs::read_to_string(settings::path()).unwrap_or_else(|_| "{\n}\n".into());
        let editor = cx.new(|cx| EditorState::new(window, cx).language("json").default_value(text));
        let view = cx.entity();
        window.open_sheet_at(Placement::Right, cx, move |sheet, _, cx| {
            let (editor, view) = (editor.clone(), view.clone());
            let ws = view.read(cx);
            let error = ws.settings_error.clone();
            let errors = ws.exprs.errors.borrow().clone();
            let docs = settings::dir().join("SETTINGS.md");
            sheet
                .title(ws.tr("settings_title", "Settings", &[]))
                .size(px(ws.settings.num("settings_editor.width", 760.) as f32))
                .child(
                    v_flex()
                        .size_full()
                        .gap_2()
                        .child(div().text_size(px(12.)).text_color(rgb(0x898a8e)).child(ws.tr(
                            "settings_intro",
                            "Everything about the app is here. Your file is merged over the built-in defaults; set a key to null to remove a default. Reference for you and agents: {docs}",
                            &[("docs", &docs.display().to_string())],
                        )))
                        .when_some(error, |d, e| d.child(div().text_size(px(12.)).text_color(rgb(0xd8566c)).child(e)))
                        .when(!errors.is_empty(), |d| d.child(div().text_size(px(12.)).text_color(rgb(0xf0a43a)).child(errors.join("\n"))))
                        .child(div().flex_1().min_h_0().child(Editor::new(&editor).h_full())),
                )
                .footer(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(gpui_kit::component::button::Button::new("docs").label(ws.tr("settings_reference", "Reference", &[])).on_click(move |_, _, _| {
                            let _ = std::process::Command::new("open").arg(&docs).spawn();
                        }))
                        .child(gpui_kit::component::button::Button::new("save-settings").label(ws.tr("settings_save", "Save", &[])).on_click(move |_, _, cx| {
                            let text = editor.read(cx).value().to_string();
                            let _ = std::fs::write(settings::path(), text);
                            view.update(cx, |ws, cx| ws.reload_settings(cx));
                        })),
                )
        });
    }

    // ---------- built-in composites ----------

    pub(crate) fn transcript_view(&mut self, chat: Option<String>, props: &Map<String, Value>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.settings.theme.clone();
        let cs = self.settings.chat.clone();
        let st = |k: &str| match cs.get(k) {
            Some(Value::Object(o)) => o.clone(),
            _ => Map::new(),
        };
        let designer = chat.as_deref() == Some(crate::designer::CHAT_ID);
        let has_items = chat.as_ref().and_then(|id| self.transcripts.get(id)).is_some_and(|t| !t.items.is_empty());
        let Some(chat_id) = chat.filter(|id| self.chat(id).is_some() || (designer && has_items)) else {
            let empty = props.get("empty").map(value_text).unwrap_or_else(|| self.tr("no_chat", "No chat here yet.", &[]));
            let overlay = if designer { self.drop_overlay(crate::designer::CHAT_ID, cx) } else { None };
            return div().relative().size_full().flex().items_center().justify_center().child(style::apply(div(), &st("empty"), &theme).child(empty)).children(overlay).into_any_element();
        };
        let working = if designer { self.designer.working } else { self.chat(&chat_id).map(|c| c.state == "working").unwrap_or(false) };
        let state = self.sync_list(&chat_id, working);
        if !designer && !state.is_following_tail() && state.item_count() > 0 && state.logical_scroll_top().item_ix <= self.settings.num("chat.load_earlier_within", 0.) as usize && self.transcripts.get(&chat_id).is_some_and(|t| t.start > 0) {
            self.load_earlier(&chat_id, None, cx);
        }
        let loading_earlier = (self.loading_earlier.get(&chat_id) == Some(&false)).then(|| {
            let st = self.settings.chat.get("loading_chip").or_else(|| self.settings.chat.get("jump")).and_then(Value::as_object).cloned().unwrap_or_default();
            let label = self.settings.chat.get("jump_labels").and_then(|l| l.get("loading")).and_then(Value::as_str).unwrap_or("Loading earlier messages…").to_string();
            div().absolute().top(px(self.settings.num("chat.loading_chip_top", 10.) as f32)).left_0().right_0().flex().justify_center()
                .child(style::apply(h_flex().gap(px(6.)).items_center(), &st, &theme).child(self.spinner(Some("xsmall"), None, cx)).child(label))
                .into_any_element()
        });
        let row_view = chat_id.clone();
        let owner = crate::ui::region::owner();
        let rows = list(state.clone(), cx.processor(move |ws: &mut Workspace, ix: usize, window, cx| ws.as_owner(&owner, window, |ws, window| ws.transcript_row(&row_view, ix, window, cx)))).size_full();
        let jump = self.jump_chip(&chat_id, &state, &theme, window, cx);
        let overlay = self.drop_overlay(&chat_id, cx);
        let shift_holds = cs.get("code_scroll").and_then(|c| c.get("shift_holds_transcript")).and_then(Value::as_bool) != Some(false);
        // Files dropped anywhere on the transcript attach to its chat, whether or not the overlay that
        // names the chat has rendered yet.
        let (drop_view, drop_chat) = (cx.entity().downgrade(), chat_id.to_string());
        let drop_attaches = self.settings.chat.get("drop_on_transcript").and_then(Value::as_bool) != Some(false);
        let tip = self.selection_tip(&chat_id, window, cx);
        let tip_chat = chat_id.clone();
        div()
            .id(SharedString::from(format!("transcript-drop:{chat_id}")))
            .relative()
            .size_full()
            .on_mouse_down(MouseButton::Left, cx.listener(|ws, _, _, cx| {
                if ws.select_tip.take().is_some() {
                    cx.notify();
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(move |ws, e: &MouseUpEvent, _, cx| {
                ws.select_tip = Some(crate::ui::quote_view::SelectTip { chat: tip_chat.clone(), at: e.position, text: String::new() });
                cx.notify();
            }))
            .on_scroll_wheel(cx.listener(|ws, _, _, cx| {
                if ws.select_tip.take().is_some() {
                    cx.notify();
                }
            }))
            .when(drop_attaches, |d| d.on_drop(move |paths: &ExternalPaths, window, cx| {
                let files = paths.paths().iter().cloned().map(Attachment::Path).collect();
                let chat = drop_chat.clone();
                let _ = drop_view.update(cx, |ws, cx| ws.attach(chat, files, window, cx));
            }))
            .child(rows)
            .when_some(crate::ui::render::scrollbar_mode(cs.get("scrollbar")), |d, m| d.child(gpui_kit::component::scroll::Scrollbar::vertical(&state).mode(m)))
            .when(shift_holds, |d| d.child(crate::ui::trace_view::shift_scroll_layer(None, 1.)))
            .children(jump)
            .children(loading_earlier)
            .children(overlay)
            .children(tip)
            .into_any_element()
    }

    /** Loads `chat.transcript_page` more of a chat's older lines (when scrolled to the top), keeping
    the view where it was. */
    pub(crate) fn load_earlier(&mut self, chat_id: &str, page: Option<usize>, cx: &mut Context<Self>) {
        let Some(t) = self.transcripts.get(chat_id) else { return };
        if t.start == 0 || self.loading_earlier.contains_key(chat_id) {
            return;
        }
        self.loading_earlier.insert(chat_id.to_string(), page.is_some());
        let page = page.unwrap_or_else(|| self.settings.chat.get("transcript_page").and_then(Value::as_u64).unwrap_or(600) as usize);
        let before = t.start;
        let remote = self.route(&Call::TranscriptUnwatch { chat_id: chat_id.to_string() });
        let id = chat_id.to_string();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = remote.call(Call::TranscriptRead { chat_id: id.clone(), from: 0, tail: Some(page), before: Some(before) }).await;
            this.update(cx, |ws, cx| {
                ws.loading_earlier.remove(&id);
                if let Ok(v) = res {
                    let entries = v["entries"].as_array().cloned().unwrap_or_default();
                    let start = v["from"].as_u64().unwrap_or(0) as usize;
                    if let Some(t) = ws.transcripts.get_mut(&id).filter(|t| t.start == before) {
                        t.prepend(start, &entries);
                        ws.reanchor_list(&id);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /** After older rows were added above, rebuilds a chat's list and keeps the same row at the top. */
    fn reanchor_list(&mut self, chat_id: &str) {
        let Some(t) = self.transcripts.get(chat_id) else { return };
        let Some(l) = self.lists.get_mut(chat_id) else { return };
        let blocks = crate::ui::trace_view::blocks_of(&t.items, l.working);
        let top = l.state.logical_scroll_top();
        let added = blocks.len().saturating_sub(l.blocks.len());
        l.state.reset(blocks.len());
        l.state.scroll_to(ListOffset { item_ix: top.item_ix + added, offset_in_item: top.offset_in_item });
        l.sigs = blocks.iter().map(|b| block_sig(&t.items, b)).collect();
        l.blocks = std::rc::Rc::new(blocks);
        (l.rev, l.items) = (t.revision, t.items.len());
    }

    /** `chat.follow`: "at_bottom" (stay at the newest message while you're there), "always", or "never". */
    fn follow_mode(&self) -> String {
        self.settings.chat.get("follow").and_then(Value::as_str).unwrap_or("at_bottom").to_string()
    }

    /** Brings a chat's list up to date with its transcript: rows are re-grouped when it changes, and the last
    two are measured again (they're the ones that grow). */
    fn sync_list(&mut self, chat_id: &str, working: bool) -> ListState {
        let follow = self.follow_mode();
        let (rev, len) = self.transcripts.get(chat_id).map(|t| (t.revision, t.items.len())).unwrap_or((0, 0));
        let l = self.lists.entry(chat_id.to_string()).or_insert_with(|| {
            let state = ListState::new(0, ListAlignment::Top, px(800.));
            if follow != "never" {
                state.set_follow_mode(FollowMode::Tail);
            }
            TranscriptList { state, blocks: Default::default(), sigs: vec![], rev: u64::MAX, working: false, items: 0, unseen: false }
        });
        if l.rev != rev || l.working != working || l.items != len {
            let Some(t) = self.transcripts.get(chat_id) else { return l.state.clone() };
            let blocks = crate::ui::trace_view::blocks_of(&t.items, working);
            let sigs: Vec<u64> = blocks.iter().map(|b| block_sig(&t.items, b)).collect();
            let (old, new) = (l.blocks.len(), blocks.len());
            // Where you're reading, by the transcript item at the top of the view, so it stays put when the
            // rows around it are rebuilt (the list would otherwise snap to the start of the changed range).
            let reading = (!l.state.is_following_tail() && old > 0).then(|| l.state.logical_scroll_top());
            let anchor = reading.and_then(|t| l.blocks.get(t.item_ix).and_then(first_item).map(|i| (i, t.offset_in_item)));
            let restore;
            if len >= l.items && l.rev != u64::MAX {
                // Re-measure only the rows whose content changed (a late tool result can change an older row):
                // the unchanged run at the start and at the end keep their measured heights.
                let keep = l.sigs.iter().zip(&sigs).take_while(|(a, b)| a == b).count().min(old).min(new);
                let tail = l.sigs[keep..].iter().rev().zip(sigs[keep..].iter().rev()).take_while(|(a, b)| a == b).count().min(old - keep).min(new - keep);
                l.state.splice(keep..old - tail, new - keep - tail);
                restore = reading.is_some_and(|t| t.item_ix >= keep && t.item_ix < old - tail);
            } else {
                l.state.reset(new);
                if follow != "never" && reading.is_none() {
                    l.state.set_follow_mode(FollowMode::Tail);
                }
                restore = reading.is_some();
            }
            if let (true, Some((item, offset))) = (restore, anchor) {
                use crate::ui::trace_view::Block;
                let holds = |b: &Block| match b {
                    Block::One(i) => *i == item,
                    Block::Steps(v) | Block::Background(v) => v.contains(&item),
                    Block::Pending(_) => false,
                };
                let found = blocks.iter().position(holds).or_else(|| blocks.iter().position(|b| first_item(b).is_some_and(|f| f >= item)));
                if let Some(ix) = found {
                    l.state.scroll_to(ListOffset { item_ix: ix, offset_in_item: offset });
                }
            }
            let grew = l.rev != u64::MAX && len > l.items;
            if grew && !l.state.is_following_tail() {
                l.unseen = true;
            }
            if follow == "always" && grew {
                l.state.set_follow_mode(FollowMode::Tail);
            }
            l.blocks = std::rc::Rc::new(blocks);
            l.sigs = sigs;
            (l.rev, l.working, l.items) = (rev, working, len);
        }
        if l.state.is_following_tail() {
            l.unseen = false;
        }
        l.state.clone()
    }

    fn transcript_row(&mut self, chat_id: &str, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some((blocks, working)) = self.lists.get(chat_id).map(|l| (l.blocks.clone(), l.working)) else { return div().into_any_element() };
        let Some(mut t) = self.transcripts.remove(chat_id) else { return div().into_any_element() };
        self.row_times = std::mem::take(&mut t.times);
        let row = self.render_block(chat_id, &t.items, &blocks, ix, working, window, cx);
        t.times = std::mem::take(&mut self.row_times);
        self.transcripts.insert(chat_id.to_string(), t);
        // `chat.list` styles the column the rows sit in: its gap goes between rows, its padding around them.
        let mut ls = self.settings.chat.get("list").and_then(Value::as_object).cloned().unwrap_or_default();
        let gap = ls.remove("gap").and_then(|g| g.as_f64()).unwrap_or(0.);
        let [top, right, bottom, left] = ls.remove("padding").and_then(|p| style::edges(&p)).unwrap_or_default();
        let last = ix + 1 == blocks.len();
        ls.insert("padding".into(), json!([if ix == 0 { top as f64 } else { gap }, right, if last { bottom } else { 0. }, left]));
        let inner = style::apply(v_flex(), &ls, &self.settings.theme).children(row);
        div().w_full().flex().flex_col().child(inner).into_any_element()
    }

    /** Scrolls a chat to its newest message and keeps it there (unless `chat.follow` is "never"). */
    pub(crate) fn jump_to_latest(&mut self, chat_id: &str) {
        let never = self.follow_mode() == "never";
        if let Some(l) = self.lists.get_mut(chat_id) {
            if never {
                l.state.scroll_to_end();
            } else {
                l.state.set_follow_mode(FollowMode::Tail);
            }
            l.unseen = false;
        }
    }

    /** The chip over a transcript scrolled up from the bottom: "New messages" once more arrive, and a click
    jumps to the newest. Look: settings `chat.jump`; `chat.jump_labels` {new, latest}; false hides it. */
    fn jump_chip(&mut self, chat_id: &str, state: &ListState, theme: &Map<String, Value>, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.settings.chat.get("jump").and_then(Value::as_bool) == Some(false) {
            return None;
        }
        let unseen = self.lists.get(chat_id).is_some_and(|l| l.unseen);
        let away = !state.is_following_tail() && state.is_scrolled_to_end() != Some(true) && state.item_count() > 0;
        if away {
            // Layout picks following back up when the view is at the bottom (or everything fits); redraw then.
            let (s, view) = (state.clone(), cx.entity().downgrade());
            window.on_next_frame(move |_, cx| {
                if s.is_following_tail() {
                    let _ = view.update(cx, |_, cx| cx.notify());
                }
            });
        }
        if !away && !unseen {
            return None;
        }
        let labels = self.settings.chat.get("jump_labels").cloned().unwrap_or(Value::Null);
        let label = |k: &str, d: &str| labels.get(k).and_then(Value::as_str).unwrap_or(d).to_string();
        let text = if unseen { label("new", "New messages") } else { label("latest", "Jump to latest") };
        let st = self.settings.chat.get("jump").and_then(Value::as_object).cloned().unwrap_or_default();
        let id = chat_id.to_string();
        self.clickables.push((format!("jump:{chat_id}"), text.clone(), json!({"do": "chat.jump_to_latest", "chat": chat_id})));
        let chip = style::apply(h_flex().id(SharedString::from(format!("jump:{chat_id}"))).gap(px(6.)).items_center().cursor_pointer(), &st, theme)
            .child(gpui_kit::component::Icon::empty().path(format!("icons/{}.svg", self.settings.get("chat.jump_icon").and_then(Value::as_str).unwrap_or("arrow-down"))).size(px(self.settings.num("chat.jump_icon_size", 13.) as f32)))
            .child(text)
            .on_click(cx.listener(move |ws, _, _, cx| {
                ws.jump_to_latest(&id);
                cx.notify();
            }));
        Some(div().absolute().bottom(px(self.settings.num("chat.jump_bottom", 14.) as f32)).left_0().right_0().flex().justify_center().child(chip).into_any_element())
    }

    pub(crate) fn composer_view(&mut self, chat: Option<String>, props: &Map<String, Value>, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let chat = chat?;
        if chat == crate::designer::CHAT_ID {
            self.ensure_composer(&chat, window, cx);
        }
        let c = self.composers.get(&chat)?.clone();
        // The list closes once the trigger word is gone or the message box loses focus (a click elsewhere).
        if self.completion.as_ref().is_some_and(|x| x.0 == chat) {
            let text = c.read(cx).value().to_string();
            let triggers = vec![self.completion.as_ref().map(|x| x.1.clone()).unwrap_or_default()];
            let focused = c.read(cx).focus_handle(cx).is_focused(window);
            if crate::completions::token(&text, &triggers).is_none() || !focused {
                self.completion = None;
                self.ctx_rev += 1;
            }
        }
        // Text set without an edit (a sent message clearing it, a completion, a draft) formats afresh.
        if self.composer_texts.get(&chat).map(String::as_str) != Some(c.read(cx).value().as_ref()) {
            self.live_md.remove(&chat);
            self.sync_markdown(&chat, window, cx);
        }
        let text = c.read(cx).value().to_string();
        if self.follow_spelling(&chat, &text, cx) {
            self.paint_composer(&chat, cx);
        }
        // Claude Code's suggested next prompt shows as the placeholder; Tab fills it in (`chat.suggestions`).
        let suggestion = (self.settings.chat.get("suggestions").and_then(Value::as_bool) != Some(false))
            .then(|| self.chat(&chat).filter(|c| c.state == "idle").and_then(|c| c.suggestion.clone()))
            .flatten();
        let fallback = self.settings.chat.get("placeholder").and_then(Value::as_str).unwrap_or("Message the agent…").to_string();
        let hint = self.settings.get("chat.suggestion_hint").and_then(Value::as_str).unwrap_or("   ⇥ Tab").to_string();
        let wanted = suggestion.as_ref().map(|s| format!("{s}{hint}")).unwrap_or(fallback);
        if chat != crate::designer::CHAT_ID && self.placeholders.get(&chat) != Some(&wanted) {
            self.placeholders.insert(chat.clone(), wanted.clone());
            c.update(cx, |s, cx| s.set_placeholder(wanted, window, cx));
        }
        let view = cx.entity().downgrade();
        let (paste_view, paste_chat) = (view.clone(), chat.clone());
        let paste_attaches = self.settings.chat.get("paste_attach").and_then(Value::as_bool) != Some(false);
        // Hidden markdown marks (sync_markdown): a quote's "> " draws as a bar, a list's "- " as a bullet.
        let md = self.settings.chat.get("live_markdown").cloned().unwrap_or_default();
        let md_color = |k: &str, d: &str| crate::ui::style::color(md.get(k).unwrap_or(&Value::from(d)), &self.settings.theme).unwrap_or(gpui_kit::transparent_black());
        let (bar, bullet) = (md_color("quote_bar", "@text_faint"), md_color("bullet", "@text_muted"));
        let textarea = Textarea::new(&c).appearance(false).token(move |t, _, _| {
            let h = t.line_height();
            match t.token().id().as_ref() {
                "md:quote" => div().w(px(14.)).h(h).flex().child(div().w(px(3.)).h_full().rounded_sm().bg(bar)).into_any_element(),
                "md:bullet" => div().w(px(16.)).h(h).flex().items_center().text_color(bullet).child("•").into_any_element(),
                "md:hide" => div().w(px(0.)).h(h).into_any_element(),
                _ => div().child(t.token().label().clone()).into_any_element(),
            }
        }).on_paste(move |item, window, cx| {
            if !paste_attaches {
                return false;
            }
            let files = attachments_from_clipboard(item);
            if files.is_empty() {
                return false;
            }
            let chat = paste_chat.clone();
            paste_view.update(cx, |ws, cx| ws.attach(chat, files, window, cx)).is_ok()
        });
        let drop_style = if self.settings.text("chat.drop_target", "card") == "card" { Default::default() } else { self.settings.chat.get("composer_drop").and_then(Value::as_object).cloned().unwrap_or_default() };
        let drop_theme = self.settings.theme.clone();
        let chips = if props.get("attachments").and_then(Value::as_bool) == Some(false) { None } else { self.attachments_view(Some(chat.clone()), props, cx) };
        let key_view = cx.entity().downgrade();
        let key_chat = chat.clone();
        Some(
            div()
                .id(SharedString::from(format!("composer:{chat}")))
                // The input binds these keys to its own actions, so take them while a list is open.
                .capture_action({
                    let (v, c) = (key_view.clone(), key_chat.clone());
                    move |_: &gpui_kit::component::input::MoveUp, window, cx| {
                        let _ = v.update(cx, |ws, cx| ws.composer_builtin_key(&c, "up", window, cx));
                    }
                })
                .capture_action({
                    let (v, c) = (key_view.clone(), key_chat.clone());
                    move |_: &gpui_kit::component::input::MoveDown, window, cx| {
                        let _ = v.update(cx, |ws, cx| ws.composer_builtin_key(&c, "down", window, cx));
                    }
                })
                .capture_action({
                    let (v, c) = (key_view.clone(), key_chat.clone());
                    move |_: &gpui_kit::component::input::Escape, window, cx| {
                        let _ = v.update(cx, |ws, cx| ws.composer_builtin_key(&c, "escape", window, cx));
                    }
                })
                .capture_action({
                    let (v, c) = (key_view.clone(), key_chat.clone());
                    move |e: &gpui_kit::component::input::Enter, window, cx| {
                        let key = if e.secondary { "cmd-enter" } else if e.shift { "shift-enter" } else { "enter" };
                        let _ = v.update(cx, |ws, cx| ws.composer_builtin_key(&c, key, window, cx));
                    }
                })
                .capture_action({
                    let (v, c) = (key_view.clone(), key_chat.clone());
                    move |_: &gpui_kit::component::input::IndentInline, window, cx| {
                        let _ = v.update(cx, |ws, cx| ws.composer_builtin_key(&c, "tab", window, cx));
                    }
                })
                .flex()
                .flex_col()
                .gap(px(self.settings.num("chat.composer_gap", 6.) as f32))
                .children(chips)
                .child(textarea)
                .rounded(px(self.settings.num("chat.composer_radius", 6.) as f32))
                .border_1()
                .border_color(transparent_black())
                .drag_over::<ExternalPaths>(move |mut s, _, _, _| {
                    style::refine(&mut s, &drop_style, &drop_theme);
                    s
                })
                .on_drop(move |paths: &ExternalPaths, window, cx| {
                    let files = paths.paths().iter().cloned().map(Attachment::Path).collect();
                    let chat = chat.clone();
                    let _ = view.update(cx, |ws, cx| ws.attach(chat, files, window, cx));
                })
                .into_any_element(),
        )
    }

    /** Drop target over a chat while files are dragged over the window, naming the chat they'll go to. */
    fn drop_overlay(&self, chat_id: &str, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.dragging || self.settings.text("chat.drop_target", "card") == "card" {
            return None;
        }
        let theme = self.settings.theme.clone();
        let st = self.settings.chat.get("drop_overlay").and_then(Value::as_object).cloned().unwrap_or_default();
        let label = if chat_id == crate::designer::CHAT_ID { self.tr("the_designer", "the designer", &[]) } else { self.chat(chat_id).map(|c| format!("“{}”", c.name)).unwrap_or_else(|| self.tr("this_chat", "this chat", &[])) };
        let icon = self.settings.get("chat.drop_overlay_icon").and_then(Value::as_str).unwrap_or("paperclip").to_string();
        let text = self.tr("drop_to_attach", "Drop to attach to {chat}", &[("chat", &label)]);
        let accent = style::color(&Value::from("@accent"), &theme).unwrap_or(blue());
        let (view, chat) = (cx.entity().downgrade(), chat_id.to_string());
        Some(
            style::apply(div().id(SharedString::from(format!("drop:{chat_id}"))).absolute().inset(px(8.)).flex().flex_col().items_center().justify_center(), &st, &theme)
                .opacity(0.)
                .drag_over::<ExternalPaths>(|s, _, _, _| s.opacity(1.))
                .child(gpui_kit::component::Icon::empty().path(format!("icons/{icon}.svg")).size(px(self.settings.num("chat.drop_overlay_icon_size", 22.) as f32)).text_color(accent))
                .child(text)
                .on_drop(move |paths: &ExternalPaths, window, cx| {
                    let files = paths.paths().iter().cloned().map(Attachment::Path).collect();
                    let chat = chat.clone();
                    let _ = view.update(cx, |ws, cx| ws.attach(chat, files, window, cx));
                })
                .into_any_element(),
        )
    }

    pub(crate) fn waiting_card(&mut self, chat: &Chat, cx: &mut Context<Self>) -> AnyElement {
        let theme = &self.settings.theme.clone();
        let cs = &self.settings.chat.clone();
        let st = |k: &str| match cs.get(k) {
            Some(Value::Object(o)) => o.clone(),
            _ => Map::new(),
        };
        let w = chat.waiting_on.clone().unwrap_or_default();
        let fallback = self.tr("waiting", "The agent is waiting for you", &[]);
        let message = waiting_message(&w).unwrap_or(fallback);
        let (options, mut questions) = waiting_options(&w, &self.settings);
        questions.retain(|q| *q != message);
        let cancel = self.tr("waiting_cancel", "Cancel", &[]);
        let several = waiting_questions(&w);
        if several.len() > 1 {
            return self.questions_card(chat, &several, cancel, cx);
        }
        let mut row = style::apply(h_flex().gap(px(6.)).flex_wrap(), &st("waiting_row"), theme);
        for (i, label) in options.iter().enumerate().chain(std::iter::once((usize::MAX, &cancel))) {
            let id = chat.id.clone();
            let keys = if i == usize::MAX { self.agent_keys(&chat.agent, "cancel_keys") } else { vec![self.answer_key(&chat.agent, i + 1)] };
            let option = if i == usize::MAX { json!("cancel") } else { json!(i) };
            self.clickables.push((format!("opt:{}:{i}", chat.id), label.clone(), json!({"do": "chat.answer", "chat": chat.id, "option": option})));
            row = row.child(
                style::apply(div().id(SharedString::from(format!("opt:{}:{i}", chat.id))).cursor_pointer(), &st("waiting_option"), theme)
                    .child(label.clone())
                    .on_click(cx.listener(move |ws, _, _, cx| ws.call(Call::ChatKeys { chat_id: id.clone(), keys: keys.clone() }, cx))),
            );
        }
        style::apply(v_flex().gap(px(10.)), &st("waiting"), theme)
            .child(div().child(message))
            .children(questions.into_iter().map(|q| div().child(q)))
            .child(row)
            .into_any_element()
    }

    /** Puts `text` back in a chat's composer, ahead of anything typed there, to change and send again. */
    pub(crate) fn refill_composer(&mut self, id: &str, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.composers.get(id).cloned() else { return };
        state.update(cx, |s, cx| {
            let rest = s.value().to_string();
            let joined = if rest.trim().is_empty() { text.to_string() } else { format!("{text}\n{rest}") };
            s.set_value(joined, window, cx);
            s.focus(window, cx);
        });
        window.dispatch_action(Box::new(gpui_kit::component::input::MoveToEnd), cx);
    }

    /** The keys an agent takes to stop (`interrupt_keys`) or cancel a question (`cancel_keys`), from
    settings `daemon.agents.<agent>` (default Escape). */
    pub(crate) fn agent_keys(&self, agent: &str, kind: &str) -> Vec<String> {
        crate::behavior::agent_keys(self.settings.get("daemon.agents").unwrap_or(&Value::Null), agent, kind)
    }

    /** The key that picks a waiting question's option `n` (1-based): `daemon.agents.<agent>.answer_key`. */
    pub(crate) fn answer_key(&self, agent: &str, n: usize) -> String {
        crate::behavior::answer_key(self.settings.get("daemon.agents").unwrap_or(&Value::Null), agent, n)
    }

    /** Where typing goes now: "terminal", "composer", "input" (another text field), or "" (none). */
    pub(crate) fn focus_area(&self, window: &mut Window, cx: &mut Context<Self>) -> &'static str {
        use gpui_kit::component::WindowExt as _;
        if self.terminals.values().any(|t| t.read(cx).is_focused(window)) {
            "terminal"
        } else if self.composers.values().any(|c| c.read(cx).focus_handle(cx).is_focused(window)) {
            "composer"
        } else if window.has_focused_input(cx) {
            "input"
        } else {
            ""
        }
    }

    /** The full-size image over the window (`chat.image_viewer`: backdrop, padding); a click anywhere closes it. */
    fn image_viewer(&self, src: ImageSource, cx: &mut Context<Self>) -> AnyElement {
        let opts = self.settings.chat.get("image_viewer").cloned().unwrap_or(Value::Null);
        let backdrop = opts.get("backdrop").and_then(|c| style::color(c, &self.settings.theme)).unwrap_or(hsla(0., 0., 0., 0.85));
        let pad = opts.get("padding").and_then(Value::as_f64).unwrap_or(32.) as f32;
        div()
            .id("image-viewer")
            .absolute()
            .inset_0()
            .occlude()
            .bg(backdrop)
            .p(px(pad))
            .cursor_pointer()
            .on_click(cx.listener(|ws, _, window, cx| ws.run_action(&json!({"do": "image.close"}), window, cx)))
            .child(img(src).size_full().object_fit(ObjectFit::Contain))
            .into_any_element()
    }

    fn handle_keys(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let pressed = e.keystroke.unparse();
        if self.image_view.is_some() && pressed == "escape" {
            self.run_action(&json!({"do": "image.close"}), window, cx);
            cx.notify();
            cx.stop_propagation();
            return;
        }
        let area = self.focus_area(window, cx);
        // The composer's completion list: `chat.completion_keys` {next, prev, accept, close}, each a list of keys.
        if let Some(chat) = self.completion.as_ref().map(|c| c.0.clone()) {
            let keys = self.settings.get("chat.completion_keys").cloned().unwrap_or(Value::Null);
            let hit = |k: &str| keys.get(k).and_then(Value::as_array).is_some_and(|l| l.iter().any(|x| x.as_str() == Some(pressed.as_str())));
            if hit("next") {
                self.move_completion(1, cx);
            } else if hit("prev") {
                self.move_completion(-1, cx);
            } else if hit("accept") {
                self.accept_completion(None, window, cx);
            } else if hit("close") {
                self.completion = None;
            } else {
                let _ = chat;
            }
            if ["next", "prev", "accept", "close"].iter().any(|k| hit(k)) {
                cx.notify();
                cx.stop_propagation();
                return;
            }
        }
        // `chat.suggestion_keys`: keys that take Claude's suggestion into an empty composer (Tab works too).
        if area == "composer" && self.settings.get("chat.suggestion_keys").and_then(Value::as_array).is_some_and(|l| l.iter().any(|x| x.as_str() == Some(pressed.as_str()))) {
            if let Some(id) = self.current_chat().map(|c| c.id.clone()) {
                self.on_tab(&id, window, cx);
                return;
            }
        }
        // The last key for this combo whose `if` holds; a key's `not_in` (["terminal", "composer", "input"])
        // leaves it to whatever has focus there.
        let candidates: Vec<Value> = self
            .settings
            .keys
            .iter()
            .filter(|k| k.get("key").and_then(Value::as_str) == Some(pressed.as_str()) && !k.get("not_in").and_then(Value::as_array).is_some_and(|l| l.iter().any(|x| x.as_str() == Some(area))))
            .cloned()
            .collect();
        if candidates.is_empty() {
            return;
        }
        let ctx = self.context();
        let found = candidates.into_iter().rev().find(|k| k.get("if").and_then(Value::as_str).is_none_or(|cond| self.exprs.truthy(cond, &ctx)));
        if let Some(k) = found {
            let action = self.exprs.resolve_action(k.get("do").unwrap_or(&Value::Null), &ctx);
            self.run_action(&action, window, cx);
            cx.stop_propagation();
        }
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let started = std::time::Instant::now();
        crate::ui::anim::set_fps(if self.motion.borrow().enabled { self.settings.motion.get("animation_fps").and_then(Value::as_f64).unwrap_or(60.) } else { 0. });
        crate::ui::anim::clear("");
        self.thumbs.clear("");
        self.register_terminals(cx);
        let t = std::time::Instant::now();
        self.note_selection(cx);
        self.flush(window, cx);
        self.track_nav();
        self.save_queue();
        self.save_drafts(cx);
        self.refresh_sub_chats();
        self.perf.borrow_mut().spans.insert("flush", t.elapsed().as_secs_f32() * 1000.);
        self.render_generation += 1;
        self.settle_regions();
        self.clickables_last = std::mem::take(&mut self.clickables);
        self.menus_last = std::mem::take(&mut self.menus);
        let generation = self.render_generation;
        // A reused region didn't draw its animations again, but they're still on screen.
        let (owners, live) = (&self.anim_owner, &self.live_regions);
        let live = |k: &String| owners.get(k).is_some_and(|o| !o.is_empty() && live.contains(o));
        self.entered.retain(|k, (_, seen, _)| *seen + 2 >= generation || live(k));
        self.transitions.retain(|k, t| t.seen + 2 >= generation || live(k));
        let (entered, transitions) = (&self.entered, &self.transitions);
        self.anim_owner.retain(|k, _| entered.contains_key(k) || transitions.contains_key(k));
        {
            let mut m = self.motion.borrow_mut();
            m.frame = generation;
            if std::mem::take(&mut self.pending_motion) {
                m.trigger = generation;
            }
            m.prune();
        }
        if let Some(p) = self.selected.clone() {
            let base = self.diff_base();
            self.ensure_diff(&p, &base, cx);
        }
        {
            use gpui_kit::component::WindowExt as _;
            // A dialog closed by Escape or a click outside is gone from the state too (ones drawn from
            // settings views close by action).
            let builtin_dialog = self.dialog.as_ref().is_some_and(|d| !d.view);
            let builtin_palette = self.palette_open && self.palette_view.is_none();
            if (builtin_dialog || builtin_palette) && !window.has_active_dialog(cx) {
                if builtin_dialog {
                    self.dialog = None;
                }
                self.palette_open = self.palette_view.is_some();
                self.ctx_rev += 1;
            }
        }
        let area = self.focus_area(window, cx);
        if area != self.focus_now {
            self.focus_now = area;
            self.ctx_rev += 1;
        }
        let dragging = cx.has_active_drag();
        if dragging != self.dragging {
            self.dragging = dragging;
            self.ctx_rev += 1;
            if self.settings.get("inspect.log_input").and_then(Value::as_bool) == Some(true) {
                // A drag that ends with a drop hint left was let go somewhere its drop didn't run.
                let at = window.mouse_position();
                let item = self.drag_item.borrow().clone().map(|(kind, value)| json!({"kind": kind, "value": value}));
                let hint = self.drop_hint.as_ref().map(|h| json!({"key": h.key, "zone": h.zone}));
                let mut what = if dragging { json!({"input": "drag", "item": item}) } else { json!({"input": "drag_end", "item": item, "hint": hint}) };
                what["x"] = json!(f32::from(at.x).round());
                what["y"] = json!(f32::from(at.y).round());
                self.log_action(&what);
            }
        }
        if self.dragging {
            self.drop_preview = false;
        } else if !self.drop_preview {
            self.drag_item.borrow_mut().take();
            self.drop_hint = None;
        }
        let into = self.drop_into().map(|(_, name)| self.tr("drag_move_to", "Move to {name}", &[("name", &name)])).unwrap_or_default();
        if *self.drag_note.borrow() != into {
            *self.drag_note.borrow_mut() = into;
        }
        // What a drag shows (`.app.drop`, `.app.drag`) rebuilds the context only when it changes: a drag
        // redraws the whole window on every mouse move.
        let sig = format!("{:?}{:?}", self.drop_hint, self.drag_item.borrow());
        if sig != self.drag_sig {
            self.drag_sig = sig;
            self.ctx_rev += 1;
        }
        self.focused = window.is_window_active();
        let size = window.viewport_size();
        let size = (f32::from(size.width), f32::from(size.height));
        if size != self.window_size {
            self.window_size = size;
            self.ctx_rev += 1;
        }
        let texts: Vec<(String, String)> = self.composers.iter().map(|(k, c)| (k.clone(), c.read(cx).value().to_string())).collect();
        for (k, v) in texts {
            if self.composer_texts.get(&k) != Some(&v) {
                self.composer_texts.insert(k, v);
                self.ctx_rev += 1;
            }
        }
        let t = std::time::Instant::now();
        let ctx = self.context();
        self.perf.borrow_mut().spans.insert("context", t.elapsed().as_secs_f32() * 1000.);
        self.refresh_shown(&ctx, cx);
        self.view_handle = Some(cx.entity().downgrade());
        let t = std::time::Instant::now();
        let body = self.render_view("main", &ctx, window, cx);
        // Panels drawn from settings views instead of the built-in ones, over everything.
        let shown: Vec<&str> = [("palette", self.palette_view.is_some()), ("dialog", self.dialog.as_ref().is_some_and(|d| d.view)), ("settings", self.settings_sheet)]
            .into_iter()
            .filter(|(name, on)| *on && self.settings.views.contains_key(*name))
            .map(|(name, _)| name)
            .collect();
        let mut overlays: Vec<AnyElement> = shown.into_iter().map(|name| div().absolute().inset_0().child(self.render_view(name, &ctx, window, cx)).into_any_element()).collect();
        if let Some((_, src)) = self.image_view.clone() {
            overlays.push(self.image_viewer(src, cx));
        }
        // Deferred, so the spinners turning over the window stay under them.
        let overlays: Vec<AnyElement> = overlays.into_iter().map(|o| deferred(crate::ui::anim::in_layer(0, o)).with_priority(0).into_any_element()).collect();
        self.perf.borrow_mut().spans.insert("tree", t.elapsed().as_secs_f32() * 1000.);
        let t = std::time::Instant::now();
        self.write_inspect(&ctx, window, cx);
        self.perf.borrow_mut().spans.insert("inspect", t.elapsed().as_secs_f32() * 1000.);
        crate::ui::region::frame_if_wanted("", window);
        self.write_problems();
        self.trim_thumbs(window, cx);
        // The chat being opened counts as open on the first frame painted with its transcript loaded.
        let opened = self.perf.borrow().opening.as_ref().map(|o| o.0.clone()).filter(|c| self.transcripts.contains_key(c));
        self.perf.borrow_mut().drew(started.elapsed(), opened);
        let theme = self.settings.theme.clone();
        let bg = style::color(&Value::from("@window"), &theme).unwrap_or(parse_color("#212122"));
        let fg = style::color(&Value::from("@text"), &theme).unwrap_or(parse_color("#f5f5f7"));
        let font = theme.get("font_ui").and_then(Value::as_str).unwrap_or(".SystemUIFont").to_string();
        div()
            .id("workspace")
            .track_focus(&self.focus)
            .key_context("Workspace")
            .size_full()
            .bg(bg)
            .text_color(fg)
            .font_family(font)
            .capture_key_down(cx.listener(Self::handle_keys))
            // A click's handlers can change what a built-in shows (a collapsed file, an expanded step)
            // without anything a region's fingerprint reads: the frame after one draws every region.
            .capture_any_mouse_down(cx.listener(|ws, _, _, _| ws.refresh_regions = true))
            .capture_any_mouse_up(cx.listener(|ws, _, _, _| ws.refresh_regions = true))
            .on_key_down(cx.listener(Self::type_to_focus))
            .on_action(cx.listener(|ws, _: &OpenSettings, w, cx| ws.open_settings(w, cx)))
            .on_action(cx.listener(|ws, _: &Palette, w, cx| ws.open_palette(w, cx)))
            .on_action(cx.listener(|ws, item: &MenuBarItem, w, cx| match MENU_ACTIONS.with(|a| a.borrow().get(&item.id).cloned()) {
                Some(action) => {
                    let ctx = ws.context();
                    let action = ws.exprs.resolve_action(&action, &ctx);
                    ws.run_action(&action, w, cx);
                }
                None => cx.propagate(),
            }))
            .on_mouse_move(cx.listener(|ws, e: &MouseMoveEvent, _, cx| {
                if let Some(d) = &ws.drag {
                    let pos = if d.vertical { f32::from(e.position.y) } else { f32::from(e.position.x) };
                    let delta = if d.invert { d.start - pos } else { pos - d.start };
                    let v = (d.start_size + delta).clamp(d.min, d.max);
                    ws.state.insert(d.key.clone(), Value::from(v as f64));
                    ws.user_state.insert(d.key.clone(), Value::from(v as f64));
                    cx.notify();
                }
            }))
            // The mouse's back and forward buttons (and trackpad swipes) move through where you've been.
            .on_mouse_down(MouseButton::Navigate(NavigationDirection::Back), cx.listener(|ws, _, window, cx| ws.mouse_button("back", window, cx)))
            .on_mouse_down(MouseButton::Navigate(NavigationDirection::Forward), cx.listener(|ws, _, window, cx| ws.mouse_button("forward", window, cx)))
            .on_mouse_down(MouseButton::Middle, cx.listener(|ws, _, window, cx| ws.mouse_button("middle", window, cx)))
            .on_mouse_up(MouseButton::Left, cx.listener(|ws, _, _, _| {
                if ws.drag.take().is_some() {
                    ws.save_state();
                }
            }))
            .relative()
            .children(self.background_layer())
            .child(body)
            .children(overlays)
    }
}

impl Workspace {
    /** Typing with nothing focused (not a shortcut) goes to the selected chat's message box
    (`chat.type_to_focus`). */
    fn type_to_focus(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let m = &e.keystroke.modifiers;
        if m.platform || m.control || self.settings.chat.get("type_to_focus").and_then(Value::as_bool) == Some(false) {
            return;
        }
        if window.focused(cx).is_some_and(|f| f != self.focus) {
            return;
        }
        let Some(text) = e.keystroke.key_char.clone().filter(|t| !t.chars().any(char::is_control)) else { return };
        let Some(chat) = self.current_chat().map(|c| c.id.clone()) else { return };
        // Settings `events.type_to_focus` decides where the typing goes (`.text`), when it's there.
        if self.settings.events.contains_key("type_to_focus") {
            self.fire_event("type_to_focus", vec![("text".into(), to_val(&Value::String(text)))], window, cx);
            cx.stop_propagation();
            return;
        }
        self.ensure_composer(&chat, window, cx);
        let Some(state) = self.composers.get(&chat).cloned() else { return };
        state.update(cx, |s, cx| {
            s.focus(window, cx);
            s.insert(text, window, cx);
        });
        self.update_completion(&chat, cx);
        cx.stop_propagation();
    }
}

impl Workspace {
    /** Fires `select` when the selected worktree or its active chat changed (`.worktree`, `.chat`: path and id). */
    /** Lets go of the transcripts of chats not shown lately: all but the `chat.keep_transcripts` most
    recently shown and the selected worktree's open tabs. One shown again loads from its machine anew. */
    fn drop_old_transcripts(&mut self, cx: &mut Context<Self>) {
        let keep_n = self.settings.num("chat.keep_transcripts", 12.) as usize;
        if self.transcripts.len() <= keep_n {
            return;
        }
        let mut keep: HashSet<String> = self.current_worktree().map(|w| self.chats_of(w).into_iter().filter(|c| self.chat_open(c)).map(|c| c.id.clone()).collect()).unwrap_or_default();
        keep.insert(crate::designer::CHAT_ID.to_string());
        let mut recent: Vec<(&std::time::Instant, &String)> = self.chat_seen.iter().map(|(c, t)| (t, c)).collect();
        recent.sort_by(|a, b| b.0.cmp(a.0));
        keep.extend(recent.into_iter().take(keep_n).map(|(_, c)| c.clone()));
        let gone: Vec<String> = self.transcripts.keys().filter(|id| !keep.contains(*id)).cloned().collect();
        for id in gone {
            if let Some(t) = self.transcripts.remove(&id) {
                self.dropped.insert(id.clone(), (Self::last_reply(&t), t.model.clone()));
            }
            self.lists.remove(&id);
            self.loading_earlier.remove(&id);
            if self.watching.remove(&id) {
                let call = Call::TranscriptUnwatch { chat_id: id.clone() };
                let remote = self.route(&call);
                cx.background_spawn(async move {
                    let _ = remote.call(call).await;
                })
                .detach();
            }
        }
    }

    /** Lets go of the terminals of worktrees not selected lately (all but `terminal.keep_views` of them);
    their shells keep running, and selecting one attaches to it again. */
    fn drop_old_terminals(&mut self, cx: &mut Context<Self>) {
        let keep_n = self.settings.num("terminal.keep_views", 8.) as usize;
        let mut recent: Vec<(&std::time::Instant, &String)> = self.worktree_seen.iter().map(|(p, t)| (t, p)).collect();
        recent.sort_by(|a, b| b.0.cmp(a.0));
        let keep: HashSet<String> = recent.into_iter().take(keep_n).map(|(_, p)| p.clone()).collect();
        let path_of = |k: &str| k.split_once('#').map_or(k, |(p, _)| p).to_string();
        let gone: Vec<String> = self.terminals.keys().filter(|k| !k.starts_with("screen:") && !keep.contains(&path_of(k))).cloned().collect();
        for k in gone {
            if let Some(t) = self.terminals.remove(&k) {
                t.read(cx).detach();
                self.term_by_id.retain(|_, v| v.entity_id() != t.entity_id());
            }
        }
    }

    fn last_reply(t: &Transcript) -> Option<String> {
        t.items.iter().rev().find_map(|i| if let Item::Assistant { text } = i { Some(text.clone()) } else { None })
    }

    fn note_selection(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.selected.clone() else { return };
        let now = (path, self.current_chat().map(|c| c.id.clone()));
        if self.seen_select.as_ref() != Some(&now) {
            let first = self.seen_select.is_none();
            self.perf.borrow_mut().opening = now.1.clone().map(|c| {
                let cached = self.transcripts.contains_key(&c);
                (c, std::time::Instant::now(), cached)
            });
            self.seen_select = Some(now.clone());
            if let Some(c) = &now.1 {
                self.chat_seen.insert(c.clone(), std::time::Instant::now());
            }
            self.worktree_seen.insert(now.0.clone(), std::time::Instant::now());
            self.drop_old_transcripts(cx);
            self.drop_old_terminals(cx);
            if !first {
                self.pending_events.push(("select".into(), [("worktree".to_string(), json!(now.0)), ("chat".to_string(), json!(now.1))].into_iter().collect()));
            }
        }
    }

    /** What follows the context but lives outside the window: the Dock badge, the menu bar, and the usage
    gauge's level (`usage.level` fires when it changes). Rebuilt only when the context changed. */
    pub(crate) fn refresh_shown(&mut self, ctx: &Val, cx: &mut Context<Self>) {
        if self.shown_rev == self.ctx_rev {
            return;
        }
        self.shown_rev = self.ctx_rev;
        self.update_badge(ctx);
        self.refresh_menu_bar(ctx, cx);
        let level = crate::ui::expr::to_json(&self.exprs.first(".usage.plan.meter.level // null", ctx)).as_str().map(str::to_string);
        if level != self.seen_usage_level {
            let from = std::mem::replace(&mut self.seen_usage_level, level.clone());
            if from.is_some() || level.is_some() {
                self.pending_events.push(("usage.level".into(), [("level".to_string(), json!(level)), ("from".to_string(), json!(from))].into_iter().collect()));
                cx.notify();
            }
        }
    }

    /** Builds the menu bar from settings `menu_bar` against the context, when what it shows changed. */
    fn refresh_menu_bar(&mut self, ctx: &Val, cx: &mut Context<Self>) {
        let spec = resolve_menus(&self.settings, &self.exprs, ctx);
        let sig = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            spec.to_string().hash(&mut h);
            h.finish()
        };
        if sig != self.menu_sig {
            self.menu_sig = sig;
            build_menu_bar(&self.settings, &spec, cx);
        }
    }
}

/** A shell command for `shell` and `ab.shell`: on the Mac, or on `host`'s daemon. */
pub(crate) struct ShellJob {
    pub(crate) run: String,
    pub(crate) cwd: Option<String>,
    pub(crate) host: Option<String>,
    pub(crate) timeout: Option<f64>,
    pub(crate) env: Vec<(String, String)>,
}

fn kill_pid(pid: u32) {
    let _ = std::process::Command::new("/bin/kill").arg(pid.to_string()).status();
}

/** `results/<id>.json` in the settings folder, written whole (agents poll for it). */
fn write_result(id: &str, result: &Value) {
    let dir = settings::dir().join("results");
    let _ = std::fs::create_dir_all(&dir);
    let tmp = dir.join(format!(".{id}.json"));
    if std::fs::write(&tmp, serde_json::to_string_pretty(result).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(tmp, dir.join(format!("{id}.json")));
    }
}

fn letter(name: &str) -> String {
    name.chars().next().map(|c| c.to_ascii_uppercase().to_string()).unwrap_or_else(|| "·".into())
}

fn split_emoji(title: &str) -> (Option<String>, String) {
    let mut chars = title.chars();
    if let Some(first) = chars.next() {
        if !first.is_ascii() && !first.is_alphanumeric() {
            let rest: String = chars.collect();
            return (Some(first.to_string()), rest.trim_start_matches('\u{fe0f}').trim_start().to_string());
        }
    }
    (None, title.to_string())
}

/** An inbox item as settings see it: its fields plus `host` (whose inbox holds it), `uid` (`host:id`, what
`inbox.read` takes) and `age_text`. */
fn inbox_item_json(i: &agent_box_protocol::InboxItem, host: &str, age_fmt: &Value) -> Value {
    let mut v = serde_json::to_value(i).unwrap_or_default();
    if let Some(o) = v.as_object_mut() {
        o.insert("host".into(), json!(host));
        o.insert("uid".into(), json!(format!("{host}:{}", i.id)));
        o.insert("age_text".into(), json!(age_text(Some(i.updated_at), age_fmt)));
    }
    v
}

/** "now", "5m", "3h", "2d", "1w" (settings `format.age`: now, m, h, d, w, each with `{n}`). */
fn now_secs() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.)
}

/** Where a host's last model is kept between runs. */
fn model_cache_path(host: &str) -> std::path::PathBuf {
    settings::cache_dir().join(format!("model-{}.json", host.replace(['/', '.'], "_")))
}

fn load_cached_model(host: &str) -> Option<(Model, f64)> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(model_cache_path(host)).ok()?).ok()?;
    Some((serde_json::from_value(v.get("model")?.clone()).ok()?, v["at"].as_f64().unwrap_or(0.)))
}

fn age_text(ts: Option<f64>, fmt: &Value) -> String {
    let Some(ts) = ts else { return String::new() };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.);
    let s = now - ts;
    let unit = |k: &str, d: &str, n: u64| fmt.get(k).and_then(Value::as_str).unwrap_or(d).replace("{n}", &n.to_string());
    if s < 60. {
        unit("now", "now", 0)
    } else if s < 3600. {
        unit("m", "{n}m", (s / 60.) as u64)
    } else if s < 86400. {
        unit("h", "{n}h", (s / 3600.) as u64)
    } else if s < 604800. {
        unit("d", "{n}d", (s / 86400.) as u64)
    } else {
        unit("w", "{n}w", (s / 604800.) as u64)
    }
}

fn load_state() -> Map<String, Value> {
    std::fs::read_to_string(settings::dir().join("ui-state.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

impl TranscriptList {
    pub(crate) fn kinds(&self) -> Vec<String> {
        use crate::ui::trace_view::Block;
        self.blocks.iter().map(|b| match b { Block::One(i) => format!("one:{i}"), Block::Steps(v) => format!("steps:{}", v.len()), Block::Background(v) => format!("background:{}", v.len()), Block::Pending(t) => t.to_string() }).collect()
    }
}

pub(crate) struct OpenDialog {
    pub(crate) title: String,
    pub(crate) labels: Vec<String>,
    /** The built-in dialog's fields; empty when `views.dialog` draws it. */
    pub(crate) inputs: Vec<(SharedString, Entity<gpui_kit::component::input::InputState>)>,
    need_first: bool,
    done: std::rc::Rc<dyn Fn(&mut Workspace, Vec<String>, &mut Context<Workspace>)>,
    /** Drawn by `views.dialog` from settings (its fields are state `dialog_field:N`). */
    pub(crate) view: bool,
}

pub(crate) struct TranscriptList {
    pub(crate) state: ListState,
    blocks: std::rc::Rc<Vec<crate::ui::trace_view::Block>>,
    /** A fingerprint of each row's content, to tell which rows changed. */
    sigs: Vec<u64>,
    rev: u64,
    working: bool,
    items: usize,
    /** New messages came in while scrolled up. */
    unseen: bool,
}

pub enum Attachment {
    Bytes { bytes: Vec<u8>, ext: String },
    Path(std::path::PathBuf),
}

pub(crate) fn attachments_from_clipboard(item: &ClipboardItem) -> Vec<Attachment> {
    item.entries()
        .iter()
        .flat_map(|e| match e {
            ClipboardEntry::Image(i) => vec![Attachment::Bytes { bytes: i.bytes.clone(), ext: image_ext(i.format).into() }],
            ClipboardEntry::ExternalPaths(p) => p.paths().iter().cloned().map(Attachment::Path).collect(),
            ClipboardEntry::String(_) => vec![],
        })
        .collect()
}

fn image_ext(f: ImageFormat) -> &'static str {
    match f {
        ImageFormat::Png => "png",
        ImageFormat::Jpeg => "jpg",
        ImageFormat::Gif => "gif",
        ImageFormat::Webp => "webp",
        ImageFormat::Svg => "svg",
        ImageFormat::Bmp => "bmp",
        ImageFormat::Tiff => "tiff",
        _ => "img",
    }
}

/** Settings `uploads` (where pasted and dropped files are copied, here and on the other machine) and the
ssh options and PATH for the copy, kept where the upload functions (which run off the UI thread) see them. */
struct UploadOptions {
    dir: String,
    remote_dir: String,
    ssh_options: Vec<String>,
    path: String,
}

/** The host's sync error when it has no backup repo (daemon.state_repo_url unset). */
fn no_backup(m: &Model) -> Option<&str> {
    m.sync.as_ref()?.error.as_deref().filter(|e| e.starts_with("no backup repo"))
}

static UPLOADS: std::sync::RwLock<Option<UploadOptions>> = std::sync::RwLock::new(None);

fn set_upload_options(settings: &Settings) {
    let home = std::env::var("HOME").unwrap_or_default();
    let path = crate::behavior::path_env(&settings.strings("app.path", DEFAULT_PATH), &home);
    let o = UploadOptions {
        dir: settings.text_at("uploads.dir", "~/.agent-box/uploads"),
        remote_dir: settings.text_at("uploads.remote_dir", "~/.agent-box/uploads"),
        ssh_options: settings.connection.ssh_options.clone(),
        path,
    };
    *UPLOADS.write().unwrap() = Some(o);
}

fn upload_options<T>(f: impl FnOnce(&UploadOptions) -> T) -> T {
    let g = UPLOADS.read().unwrap();
    match g.as_ref() {
        Some(o) => f(o),
        None => f(&UploadOptions { dir: "~/.agent-box/uploads".into(), remote_dir: "~/.agent-box/uploads".into(), ssh_options: vec![], path: "/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin".into() }),
    }
}

fn uploads_dir() -> std::path::PathBuf {
    upload_options(|o| std::path::PathBuf::from(crate::script::expand(&o.dir)))
}

pub fn write_upload(bytes: &[u8], ext: &str) -> Result<std::path::PathBuf, String> {
    let dir = uploads_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let p = dir.join(format!("pasted-{stamp}.{ext}"));
    std::fs::write(&p, bytes).map_err(|e| e.to_string())?;
    Ok(p)
}

/** Copies a file into ~/.agent-box/uploads (and, given a host, the same folder there); returns the path
the agent should read. */
pub fn stage_one(path: &std::path::Path, host: Option<&str>) -> Result<String, String> {
    let dir = uploads_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let local = if path.starts_with(&dir) {
        path.to_path_buf()
    } else {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        let base = path.file_name().map(|n| n.to_string_lossy().replace([' ', '\'', '"'], "-")).unwrap_or_else(|| "file".into());
        let copy = dir.join(format!("{stamp}-{base}"));
        std::fs::copy(path, &copy).map_err(|e| format!("{}: {e}", path.display()))?;
        copy
    };
    let name = local.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let Some(host) = host else { return Ok(local.to_string_lossy().into_owned()) };
    let (remote_dir, ssh_options, path) = upload_options(|o| (o.remote_dir.clone(), o.ssh_options.clone(), o.path.clone()));
    // The remote folder is written relative to its home ("~/x" -> "$HOME/x") so the agent gets a full path back.
    let rel = remote_dir.strip_prefix("~/").map(|r| format!("$HOME/{r}")).unwrap_or_else(|| remote_dir.clone());
    let dir = rel.trim_end_matches('/').to_string();
    let target = format!("\"{dir}\"/{}", crate::behavior::sh_quote(&name));
    let file = std::fs::File::open(&local).map_err(|e| e.to_string())?;
    let o = std::process::Command::new("ssh")
        .env("PATH", path)
        .args(&ssh_options)
        .args([host, &format!("mkdir -p \"{dir}\" && cat > {target} && printf %s {target}")])
        .stdin(file)
        .output()
        .map_err(|e| e.to_string())?;
    if !o.status.success() {
        return Err(String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("upload failed").to_string());
    }
    Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/** A message's attached uploads (by their local copies) and its text with their paths removed. */
pub(crate) fn split_uploads(text: &str) -> (Vec<std::path::PathBuf>, String) {
    let dir = uploads_dir();
    let mut files = vec![];
    let mut words = vec![];
    let marker = upload_options(|o| format!("/{}/", o.remote_dir.trim_start_matches("~/").trim_matches('/')));
    for w in text.split(' ') {
        match w.split(marker.as_str()).nth(1).map(|name| dir.join(name.trim())) {
            Some(p) if p.exists() => files.push(p),
            _ => words.push(w),
        }
    }
    (files, words.join(" ").trim().to_string())
}

/** The image's size scaled down to fit within `max_w` x `max_h`. */
pub fn fit(path: &std::path::Path, max_w: f32, max_h: f32) -> (f32, f32) {
    imagesize::size(path).map_or((max_w, max_h), |d| fit_size(d, max_w, max_h))
}

pub fn fit_size(d: imagesize::ImageSize, max_w: f32, max_h: f32) -> (f32, f32) {
    let (w, h) = (d.width as f32, d.height as f32);
    let k = (max_w / w).min(max_h / h).min(1.);
    (w * k, h * k)
}

/** An attached image's bytes as GPUI decodes them (for the full-size viewer). */
pub(crate) fn image_of(media_type: &str, data: &[u8]) -> Option<std::sync::Arc<Image>> {
    let format = match media_type {
        "image/jpeg" => ImageFormat::Jpeg,
        "image/gif" => ImageFormat::Gif,
        "image/webp" => ImageFormat::Webp,
        _ => ImageFormat::Png,
    };
    Some(std::sync::Arc::new(Image::from_bytes(format, data.to_vec())))
}

/** Items from several machines (a merged project's worktrees, the sections) in `saved` order (kept by
dragging, since each machine orders only its own); `key` gives an item's id and machine. One not in it
stays by its machine's order: after the one its machine lists before it, else before the one after it. */
pub(crate) fn apply_order<T: Clone>(items: &mut Vec<T>, saved: &[&str], key: impl Fn(&T) -> (String, String)) {
    let keys: Vec<(String, String)> = items.iter().map(key).collect();
    let rank = |i: usize| saved.iter().position(|p| *p == keys[i].0);
    let mut placed: Vec<usize> = (0..items.len()).filter(|&i| rank(i).is_some()).collect();
    placed.sort_by_key(|&i| rank(i));
    for i in (0..items.len()).filter(|&i| rank(i).is_none()) {
        let same = |j: &usize| keys[*j].1 == keys[i].1;
        let after = (0..i).rev().filter(same).find_map(|p| placed.iter().position(|&x| x == p)).map(|at| at + 1);
        let before = || (i + 1..items.len()).filter(same).find_map(|n| placed.iter().position(|&x| x == n));
        let at = after.or_else(before).unwrap_or(placed.len());
        placed.insert(at, i);
    }
    let ordered = placed.into_iter().map(|i| items[i].clone()).collect();
    *items = ordered;
}

/** A list with each leaving worktree (a `branches` one not on `to`) swapped for its copy on `to`: in its
place when the copy is in the same list, left out when the copy is elsewhere (`arrived`). */
pub(crate) fn switch_rows(items: &mut Vec<Worktree>, to: &str, branches: &HashSet<&str>, arrived: &HashSet<(String, String)>) {
    let key = |w: &Worktree| w.branch.clone().filter(|b| branches.contains(b.as_str())).map(|b| (w.repo.clone(), b));
    let here: HashMap<(String, String), usize> = items.iter().enumerate().filter(|(_, w)| w.host == to).filter_map(|(i, w)| key(w).map(|k| (k, i))).collect();
    let mut placed = HashSet::new();
    let mut out = Vec::with_capacity(items.len());
    for (i, w) in items.iter().enumerate() {
        let at = match key(w) {
            Some(k) if w.host != to => match here.get(&k) {
                Some(&j) => j,
                None if arrived.contains(&k) => continue,
                None => i,
            },
            _ => i,
        };
        if placed.insert(at) {
            out.push(items[at].clone());
        }
    }
    *items = out;
}

/** What a waiting chat asks: its (first) question's text, else the agent's message, else the tool's name. */
pub(crate) fn waiting_message(w: &Value) -> Option<String> {
    w["tool_input"]["questions"][0]["question"].as_str().or(w["message"].as_str()).or(w["tool_name"].as_str()).map(str::to_string)
}

/** Each question a waiting chat asks, with its options' labels. */
pub(crate) fn waiting_questions(w: &Value) -> Vec<(String, Vec<String>)> {
    let qs = w["tool_input"]["questions"].as_array().cloned().unwrap_or_default();
    qs.iter()
        .map(|q| {
            let options = q["options"].as_array().map(|o| o.iter().filter_map(|x| x["label"].as_str().map(str::to_string)).collect()).unwrap_or_default();
            (q["question"].as_str().unwrap_or_default().to_string(), options)
        })
        .collect()
}

/** A waiting chat's choices: the first question's options (or `chat.waiting_options`), and the text of
every question when there are several (`chat.waiting_all_questions`). */
pub(crate) fn waiting_options(w: &Value, settings: &Settings) -> (Vec<String>, Vec<String>) {
    let qs = w["tool_input"]["questions"].as_array().cloned().unwrap_or_default();
    let options = qs
        .first()
        .and_then(|q| q["options"].as_array())
        .map(|o| o.iter().filter_map(|x| x["label"].as_str().map(str::to_string)).collect())
        .unwrap_or_else(|| {
            settings
                .get("chat.waiting_options")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
                .unwrap_or_else(|| vec!["Yes".into(), "Yes, don't ask again".into(), "No".into()])
        });
    let all = settings.get("chat.waiting_all_questions").and_then(Value::as_bool).unwrap_or(true);
    let questions = if all && qs.len() > 1 { qs.iter().filter_map(|q| q["question"].as_str().map(str::to_string)).collect() } else { vec![] };
    (options, questions)
}

/** A picture from a file (absolute, ~/, or relative to the settings folder) or a URL, fit "cover" |
"contain" | "fill" | "scale_down" | "none". */
/** A picture, an animated GIF/WebP, or (on macOS) a video, muted and looping: by the source's extension. */
pub(crate) fn media_element(src: &str, fit: &str) -> AnyElement {
    #[cfg(target_os = "macos")]
    if crate::video::is_video(src) {
        let path = if src.starts_with("http://") || src.starts_with("https://") {
            src.to_string()
        } else {
            let p = std::path::PathBuf::from(crate::script::expand(src));
            (if p.is_absolute() { p } else { settings::dir().join(p) }).to_string_lossy().into_owned()
        };
        let fit = match fit {
            "contain" => ObjectFit::Contain,
            "fill" => ObjectFit::Fill,
            "scale_down" => ObjectFit::ScaleDown,
            "none" => ObjectFit::None,
            _ => ObjectFit::Cover,
        };
        return crate::video::element(&path, fit);
    }
    image_element(src, fit).size_full().into_any_element()
}

pub(crate) fn image_element(src: &str, fit: &str) -> Img {
    let fit = match fit {
        "contain" => ObjectFit::Contain,
        "fill" => ObjectFit::Fill,
        "scale_down" => ObjectFit::ScaleDown,
        "none" => ObjectFit::None,
        _ => ObjectFit::Cover,
    };
    let el = if src.starts_with("http://") || src.starts_with("https://") {
        img(SharedUri::from(src.to_string()))
    } else {
        let p = std::path::PathBuf::from(crate::script::expand(src));
        img(if p.is_absolute() { p } else { settings::dir().join(p) })
    };
    el.object_fit(fit)
}

/** `app.path` when unset. */
const DEFAULT_PATH: &[&str] = &["~/.local/bin", "~/.cargo/bin", "~/.super.engineering/bin", "~/dev/redo-monorepo-worktrees/bin", "/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"];

/** One auto-update check in the source checkout (`app.source_dir`, as $AGENT_BOX_SOURCE): fast-forward to origin/main when the checkout is clean, then
rebuild and restart the daemon if its code changed and bundle the app if the app's did. Prints "updated"
and what it rebuilt ("daemon", "app"), or nothing. */
const UPDATE_SCRIPT: &str = r#"cd "${AGENT_BOX_SOURCE:-$HOME/dev/agent-box}" || exit 0
export PATH="$HOME/.cargo/bin:$PATH:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"
git fetch -q origin main 2>/dev/null || exit 0
[ "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)" ] && exit 0
[ "$(git branch --show-current)" = main ] || exit 0
[ -z "$(git status --porcelain --untracked-files=no)" ] || exit 0
git merge-base --is-ancestor HEAD origin/main || exit 0
changed=$(git diff --name-only HEAD origin/main)
git merge -q --ff-only origin/main || exit 0
what=""
if printf '%s\n' "$changed" | grep -q '^rust/daemon/\|^rust/protocol/'; then
  (cd rust && cargo build --release -q -p agent-boxd) >/dev/null 2>&1 && pkill -f '[a]gent-boxd run' && what="$what daemon"
fi
if printf '%s\n' "$changed" | grep -q '^rust/app/src/\|^rust/app/Cargo\|^rust/protocol/\|^rust/Cargo\|^rust/app/bundle'; then
  (cd rust/app && ./bundle.sh) >/dev/null 2>&1 && what="$what app"
fi
echo "updated$what""#;

/** After an update rebuilt the app: start the new build (`app.restart_command`). */
const RESTART_COMMAND: &str = "sleep 1; open \"$HOME/Applications/a.box.app\"";

/** Refreshes Claude Code's plan-usage cache from its OAuth endpoint (the token stays in the keychain). */
const PLAN_COMMAND: &str = r#"token=$(security find-generic-password -s 'Claude Code-credentials' -w 2>/dev/null | /usr/bin/python3 -c 'import json,sys; print(json.load(sys.stdin)["claudeAiOauth"]["accessToken"])' 2>/dev/null); [ -n "$token" ] && out=$(/usr/bin/curl -sS -m 10 https://api.anthropic.com/api/oauth/usage -H "Authorization: Bearer $token" -H "anthropic-beta: oauth-2025-04-20") && printf '%s' "$out" | grep -q '"limits"' && printf '%s' "$out" > "$HOME/.claude/oauth-usage-cache.json""#;

/** A cheap fingerprint of what a transcript row shows: its items' kinds, text lengths and results. */
/** The first transcript item a row shows (none for the "Working…" row). */
fn first_item(b: &crate::ui::trace_view::Block) -> Option<usize> {
    use crate::ui::trace_view::Block;
    match b {
        Block::One(i) => Some(*i),
        Block::Steps(v) | Block::Background(v) => v.first().copied(),
        Block::Pending(_) => None,
    }
}

fn block_sig(items: &[Item], b: &crate::ui::trace_view::Block) -> u64 {
    use crate::ui::trace_view::Block;
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut one = |i: usize| match items.get(i) {
        Some(Item::Tool { id, result, is_error, .. }) => (1u8, id.len(), result.as_ref().map(|r| r.len() + 1).unwrap_or(0), *is_error).hash(&mut h),
        Some(Item::User { text }) | Some(Item::Assistant { text }) | Some(Item::Thinking { text }) | Some(Item::Notice { text }) => (2u8, i, text.len()).hash(&mut h),
        Some(Item::Image { data, .. }) => (3u8, data.len()).hash(&mut h),
        Some(Item::Wake) => 4u8.hash(&mut h),
        None => 0u8.hash(&mut h),
    };
    match b {
        Block::One(i) => one(*i),
        Block::Steps(v) => v.iter().for_each(|i| one(*i)),
        Block::Background(v) => v.iter().for_each(|i| one(*i)),
        Block::Pending(t) => t.hash(&mut h),
    }
    h.finish()
}

#[cfg(test)]
mod order_tests {
    use super::apply_order;
    use agent_box_protocol::Worktree;

    fn w(path: &str, host: &str) -> Worktree {
        Worktree { path: path.into(), host: host.into(), ..Default::default() }
    }

    fn key(w: &Worktree) -> (String, String) {
        (w.path.clone(), w.host.clone())
    }

    fn paths(items: &[Worktree]) -> Vec<&str> {
        items.iter().map(|w| w.path.as_str()).collect()
    }

    #[test]
    fn a_moving_worktree_takes_its_copys_place_once_it_arrives() {
        let b = |path: &str, branch: &str, host: &str| Worktree { branch: Some(branch.into()), ..w(path, host) };
        let branches = ["x", "y"].into_iter().collect();
        let arrived = [("".to_string(), "x".to_string()), ("".to_string(), "z".to_string())].into_iter().collect();
        let mut items = vec![b("m-x", "x", "mac"), b("m-y", "y", "mac"), b("m-o", "o", "mac"), b("b-x", "x", "box")];
        super::switch_rows(&mut items, "box", &branches, &arrived);
        // x arrived: its box copy takes its row; y hasn't yet, so the Mac's stays.
        assert_eq!(paths(&items), ["b-x", "m-y", "m-o"]);
        let mut items = vec![b("m-x", "x", "mac")];
        super::switch_rows(&mut items, "box", &branches, &arrived);
        assert!(items.is_empty(), "its copy is in another list");
    }

    #[test]
    fn a_saved_order_mixes_machines_and_new_worktrees_keep_their_machines_place() {
        let mut items = vec![w("m1", "mac"), w("m2", "mac"), w("m3", "mac"), w("b1", "box"), w("b2", "box")];
        apply_order(&mut items, &["m1", "b1", "m3", "m2"], key);
        // m1 b1 m3 m2 saved; b2 follows b1 on the box.
        assert_eq!(paths(&items), ["m1", "b1", "b2", "m3", "m2"]);
        let mut items = vec![w("m0", "mac"), w("m1", "mac"), w("b1", "box")];
        apply_order(&mut items, &["b1", "m1", "gone"], key);
        assert_eq!(paths(&items), ["b1", "m0", "m1"]);
    }
}

/** A move under way: where to, and the worktrees (`move_key`) and chats it takes along. */
pub(crate) struct ChatMove {
    pub serial: u64,
    pub to: String,
    pub worktrees: HashSet<String>,
    pub chats: HashSet<String>,
}

fn move_key(w: &Worktree) -> String {
    w.branch.as_ref().map_or_else(|| w.path.clone(), |b| format!("{}\0{b}", w.repo))
}

/** A worktree move the sidebar shows before its machine confirms it: `before` is where it went in the sidebar,
`host_before` where its machine puts it (the next of its own worktrees there). */
pub(crate) struct PendingMove {
    pub path: String,
    pub host: String,
    pub section: Option<String>,
    pub before: Option<String>,
    pub host_before: Option<String>,
    pub at: std::time::Instant,
}
