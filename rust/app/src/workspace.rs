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

/** A settings `menu_bar` item, by its place in the list built from settings. */
#[derive(Action, Clone, PartialEq, serde::Deserialize)]
#[action(namespace = agent_box, no_json)]
pub struct MenuBarItem {
    pub index: usize,
}

thread_local! {
    static MENU_ACTIONS: std::cell::RefCell<Vec<Value>> = const { std::cell::RefCell::new(vec![]) };
}

/** The macOS menu bar: the a.box menu (Settings, Command Palette, settings `menu_bar.app` items, Quit),
then each of `menu_bar.menus` ({name, items: [{label, do} | {separator: true}]}). */
pub fn set_menu_bar(settings: &Settings, cx: &mut App) {
    let mut actions: Vec<Value> = vec![];
    let items_of = |list: Option<&Value>, actions: &mut Vec<Value>| -> Vec<MenuItem> {
        list.and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|it| {
                        if it.get("separator").and_then(Value::as_bool) == Some(true) {
                            MenuItem::separator()
                        } else {
                            actions.push(it.get("do").cloned().unwrap_or(Value::Null));
                            let label = it.get("label").and_then(Value::as_str).unwrap_or("").to_string();
                            MenuItem::action(label, MenuBarItem { index: actions.len() - 1 })
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let app_name = settings.text("app_name", "a.box");
    let mut app_items = vec![
        MenuItem::action(settings.text("menu_settings", "Settings…"), OpenSettings),
        MenuItem::action(settings.text("menu_palette", "Command Palette"), Palette),
    ];
    let extra = items_of(settings.get("menu_bar.app"), &mut actions);
    if !extra.is_empty() {
        app_items.push(MenuItem::separator());
        app_items.extend(extra);
    }
    app_items.push(MenuItem::separator());
    app_items.push(MenuItem::action(settings.text("menu_quit", "Quit {app}").replace("{app}", &app_name), Quit));
    let mut menus = vec![Menu { name: app_name.into(), items: app_items, disabled: false }];
    for m in settings.get("menu_bar.menus").and_then(Value::as_array).cloned().unwrap_or_default() {
        let name = m.get("name").and_then(Value::as_str).unwrap_or("").to_string();
        let items = items_of(m.get("items"), &mut actions);
        menus.push(Menu { name: name.into(), items, disabled: false });
    }
    MENU_ACTIONS.with(|a| *a.borrow_mut() = actions);
    cx.set_menus(menus);
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

/** The machine the app runs on (its daemon is local); every other host is reached over ssh. */
pub(crate) const LOCAL_HOST: &str = "mac";

pub struct Workspace {
    _remote_guards: Vec<RemoteGuard>,
    /** One daemon connection per host: "box" over ssh, "mac" run locally. */
    pub(crate) remotes: HashMap<String, Remote>,
    /** Each host's latest model; `model` is them merged (this Mac's first). */
    pub(crate) models: HashMap<String, Model>,
    pub(crate) host_up: HashMap<String, bool>,
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
    loading_earlier: std::collections::HashSet<String>,
    /** The open completion list under a composer: (chat, trigger, entries, selected). */
    pub(crate) completion: Option<(String, String, Vec<crate::completions::Entry>, usize)>,
    /** Messages typed while a chat was working, sent one per turn as it finishes (`chat.queue`). */
    pub(crate) queued: HashMap<String, Vec<String>>,
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
    /** Recent in-window notifications: (unix time, text). */
    pub(crate) notices: Vec<(f64, String)>,
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
    pub(crate) settings: Settings,
    pub(crate) settings_error: Option<String>,
    pub(crate) remote: Remote,
    pub(crate) connected: bool,
    pub(crate) status: String,
    pub(crate) model: Model,
    pub(crate) selected: Option<String>,
    pub(crate) active_chat: HashMap<String, String>,
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
    pub(crate) terminals: HashMap<String, Entity<TerminalView>>,
    term_by_id: HashMap<(String, u64), Entity<TerminalView>>,
    pub(crate) expanded_tools: HashSet<String>,
    pub(crate) images: HashMap<String, Option<std::sync::Arc<Image>>>,
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
    pub(crate) drop_preview: bool,
    pub(crate) inspect_at: Option<std::time::Instant>,
    pub(crate) inspect_digest: u64,
    /** When the app's files last went to the other machines, and what each held. */
    pub(crate) mirror_at: Option<std::time::Instant>,
    pub(crate) mirrored: HashMap<String, u64>,
    /** A chat being moved and where to: selected there once it shows up. */
    pub(crate) pending_move: Option<(String, String)>,
    pub(crate) perf: std::rc::Rc<std::cell::RefCell<crate::ui::inspect::Perf>>,
    /** The redraw a looping animation (spinner, pulse) asked for, at `motion.animation_fps`. */
    pub(crate) anim_tick: Option<Task<()>>,
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
    pub(crate) focused: bool,
    pending_events: Vec<String>,
    pub(crate) pending_script: Vec<Value>,
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
        let (settings, settings_error) = match settings::load() {
            Ok(s) => (s, None),
            Err(e) => (Settings::default(), Some(format!("{e:#}"))),
        };
        crate::ui::docs::write_reference();
        crate::transcript::set_options(&settings.chat);
        let retry = Duration::from_secs(settings.connection.retry_seconds);
        let mut conns = vec![("box".to_string(), settings.connection.ssh_host.clone(), settings.connection.daemon.clone())];
        if settings.local.enabled && !settings.local.daemon.is_empty() {
            conns.insert(0, ("mac".to_string(), String::new(), settings.local.daemon.clone()));
        }
        let (mut remotes, mut guards, mut listeners) = (HashMap::new(), vec![], vec![]);
        for (host, ssh, daemon) in conns {
            let (r, incoming, guard) = Remote::start(ssh, daemon, retry, settings.connection.ssh_options.clone());
            remotes.insert(host.clone(), r);
            guards.push(guard);
            listeners.push(cx.spawn_in(window, async move |this, cx| {
                while let Ok(msg) = incoming.recv_async().await {
                    if this.update_in(cx, |ws, window, cx| ws.on_incoming(&host, msg, window, cx)).is_err() {
                        break;
                    }
                }
            }));
        }
        let remote = remotes.get("box").cloned().expect("the box connection");
        let watch = Self::watch_settings(window, cx);
        // Usage and plan limits: refreshed every `usage.refresh_secs`.
        let usage_loop = cx.spawn(async move |this, cx| loop {
            let every = this.update(cx, |ws, cx| {
                ws.refresh_usage(false, cx);
                ws.settings.num("usage.refresh_secs", 120.).max(15.)
            });
            let Ok(every) = every else { break };
            cx.background_executor().timer(Duration::from_secs_f64(every)).await;
        });
        // `app.auto_update`: pull a.box's main from its remote and rebuild what changed (the daemon, the app);
        // a new app build restarts the app. So a change pushed from the box ships to the Mac on its own.
        let update_loop = cx.spawn(async move |this, cx| loop {
            let every = this.update(cx, |ws, _| (ws.settings.get("app.auto_update").and_then(Value::as_bool) != Some(false)).then(|| ws.settings.num("app.update_check_secs", 60.).max(15.)));
            let Ok(every) = every else { break };
            cx.background_executor().timer(Duration::from_secs_f64(every.unwrap_or(300.))).await;
            if every.is_none() {
                continue;
            }
            let out = cx.background_spawn(async move { std::process::Command::new("/bin/sh").arg("-c").arg(UPDATE_SCRIPT).output() }).await;
            let text = out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
            if !text.starts_with("updated") {
                continue;
            }
            let app = text.split_whitespace().any(|w| w == "app");
            let ok = this.update(cx, |ws, cx| {
                ws.log_action(&json!({"do": "app.updated", "what": text}));
                if app {
                    ws.save_drafts(cx);
                }
            });
            if ok.is_err() {
                break;
            }
            if app {
                let _ = std::process::Command::new("/bin/sh").arg("-c").arg("sleep 1; open \"$HOME/Applications/a.box.app\"").spawn();
                let _ = this.update(cx, |_, cx| cx.quit());
                break;
            }
        });
        let env_every = settings.connection.env_push_seconds;
        let env_push = cx.spawn(async move |_, cx| loop {
            if env_every == 0 {
                break;
            }
            let home = std::env::var("HOME").unwrap_or_default();
            let _ = cx
                .background_spawn(async move {
                    std::process::Command::new(format!("{home}/dev/agent-box/bin/env-push"))
                        .env("PATH", "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin")
                        .output()
                })
                .await;
            cx.background_executor().timer(Duration::from_secs(env_every)).await;
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
                    let chat = id.split('#').next().unwrap_or("").to_string();
                    if this.update_in(cx, |ws, window, cx| ws.open_chat_from_notification(&chat, window, cx)).is_err() {
                        break;
                    }
                }
            })
        });
        #[cfg(not(target_os = "macos"))]
        let clicks: Option<Task<()>> = None;
        let mut transcripts: HashMap<String, Transcript> = HashMap::new();
        transcripts.entry(crate::designer::CHAT_ID.into()).or_default().append(0, &crate::designer::past_entries());
        let user_state = load_state();
        let mut state = settings.state.clone();
        state.extend(user_state.clone());
        let ws = Self {
            _remote_guards: guards,
            remotes,
            models: HashMap::new(),
            host_up: HashMap::new(),
            project_hosts: vec![],
            section_hosts: HashMap::new(),
            history: vec![],
            history_loading: false,
            inputs: HashMap::new(),
            input_submits: HashMap::new(),
            loading_earlier: Default::default(),
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
            view_handle: None,
            placeholders: HashMap::new(),
            queue_sent: HashMap::new(),
            settings,
            settings_error,
            remote,
            connected: false,
            status: "Connecting to the box…".into(),
            model: Model::default(),
            selected: None,
            active_chat: HashMap::new(),
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
            images: HashMap::new(),
            diffs: HashMap::new(),
            attachments: HashMap::new(),
            send_when_ready: HashSet::new(),
            next_attachment: 0,
            dragging: false,
            drop_hint: None,
            drag_item: Default::default(),
            drag_note: Default::default(),
            drop_preview: false,
            inspect_at: None,
            inspect_digest: 0,
            mirror_at: None,
            mirrored: HashMap::new(),
            pending_move: None,
            perf: Default::default(),
            anim_tick: None,
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
            focused: true,
            pending_events: vec![],
            pending_script: vec![],
            notify_count: 0,
            badge: None,
            dock_icon: None,
            diff_collapsed: HashSet::new(),
            diff_geom: Default::default(),
            diff_scroll: HashMap::new(),
            focus: cx.focus_handle(),
            _tasks: [watch, env_push, designer_listen, usage_loop, update_loop].into_iter().chain(listeners).chain(clicks).collect(),
            _subscriptions: vec![],
        };
        let mut ws = ws;
        ws.configure_motion();
        ws.load_scripts();
        ws.update_dock_icon();
        ws
    }

    /** (Re)starts the scripts: a fresh JS context, so edits take effect at once and old timers stop. */
    pub(crate) fn load_scripts(&mut self) {
        self.script_timers.clear();
        self.script_data.clear();
        self.scripts = crate::script::Scripts::load(&self.settings.scripts);
        self.pending_script.push(json!({"type": "event", "name": "start", "payload": null}));
    }

    /** Runs queued script messages, events, and actions now, so the app keeps working while its window
    is hidden and not rendering (agents drive it then too). */
    pub(crate) fn flush(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending_script.is_empty() && self.pending_events.is_empty() && self.pending_actions.is_empty() {
            return;
        }
        for _ in 0..8 {
            if self.pending_script.is_empty() && self.pending_events.is_empty() && self.pending_actions.is_empty() {
                break;
            }
            for m in std::mem::take(&mut self.pending_script) {
                self.script_deliver(m, window, cx);
            }
            for e in std::mem::take(&mut self.pending_events) {
                self.fire_event(&e, vec![], window, cx);
            }
            for a in std::mem::take(&mut self.pending_actions) {
                self.run_action(&a, window, cx);
            }
        }
        let ctx = self.context();
        self.inspect_at = None;
        self.write_inspect(&ctx, window, cx);
    }

    /** The context as JSON for scripts, reused until something changes it (`ctx_rev`) or 250ms pass. */
    fn script_context(&mut self) -> std::rc::Rc<str> {
        if let Some((rev, at, text)) = &self.ctx_cache {
            if *rev == self.ctx_rev && (at.elapsed().as_millis() as f64) < self.settings.num("scripts_options.context_cache_ms", 250.) {
                return text.clone();
            }
        }
        let text: std::rc::Rc<str> = std::rc::Rc::from(crate::ui::expr::to_json(&self.context()).to_string());
        self.ctx_cache = Some((self.ctx_rev, std::time::Instant::now(), text.clone()));
        text
    }

    pub(crate) fn script_deliver(&mut self, message: Value, window: &mut Window, cx: &mut Context<Self>) {
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
                let cmd = m["cmd"].as_str().unwrap_or("").to_string();
                let home = std::env::var("HOME").unwrap_or_default();
                cx.spawn_in(window, async move |this, cx| {
                    let out = cx
                        .background_spawn(async move {
                            std::process::Command::new("/bin/sh")
                                .arg("-c")
                                .arg(cmd)
                                .env("PATH", format!("{home}/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"))
                                .output()
                        })
                        .await;
                    let result = match out {
                        Ok(o) => json!({"code": o.status.code(), "stdout": String::from_utf8_lossy(&o.stdout), "stderr": String::from_utf8_lossy(&o.stderr)}),
                        Err(e) => json!({"code": -1, "stdout": "", "stderr": e.to_string()}),
                    };
                    this.update_in(cx, |ws, window, cx| ws.script_deliver(json!({"type": "reply", "id": id, "result": result}), window, cx)).ok();
                })
                .detach();
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
                let _ = std::fs::write(path, m["text"].as_str().unwrap_or(""));
            }
            "emit" => {
                let name = m["event"].as_str().unwrap_or("").to_string();
                let payload = to_val(&m["payload"]);
                self.fire_event(&name, vec![("payload".into(), payload)], window, cx);
                self.pending_script.push(json!({"type": "event", "name": name, "payload": m["payload"]}));
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
            _ => {}
        }
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
                self.pending_events.push("designer.finished".into());
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
                    } else if p.extension().is_some_and(|e| e == "js") && p.parent().is_some_and(|d| d.ends_with("scripts")) {
                        let _ = tx.send(false);
                    } else if p.file_name().and_then(|n| n.to_str()) == Some("run.json") {
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

    /** `run.json` in the settings folder: actions written there run once, then the file is removed. */
    fn run_inbox(&mut self, cx: &mut Context<Self>) {
        let p = settings::dir().join("run.json");
        let Ok(text) = std::fs::read_to_string(&p) else { return };
        let _ = std::fs::remove_file(&p);
        match serde_json_lenient::from_str::<Value>(&text) {
            Ok(v) => self.pending_actions.push(v),
            Err(e) => self.exprs.note(format!("run.json: {e}")),
        }
        cx.notify();
    }

    pub(crate) fn reload_settings(&mut self, cx: &mut Context<Self>) {
        crate::ui::docs::write_reference();
        match settings::load() {
            Ok(s) => {
                self.state = s.state.clone();
                self.state.extend(self.user_state.clone());
                self.settings = s;
                crate::transcript::set_options(&self.settings.chat);
                set_menu_bar(&self.settings, cx);
                self.settings_error = None;
                self.exprs = Exprs::default();
                self.configure_motion();
                self.load_scripts();
                for l in self.lists.values() {
                    l.state.remeasure();
                }
                self.pending_motion = true;
                let ctx = self.context();
                self.update_badge(&ctx);
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

    /** Runs `events` from settings for chat state changes: chat.waiting (now needs you), chat.finished
    (working to idle), chat.stopped. The chat is `.chat` in the action's context. */
    fn fire_chat_events(&mut self, before: &HashMap<String, String>, window: &mut Window, cx: &mut Context<Self>) {
        let mut fired = vec![];
        for c in &self.model.chats {
            let Some(old) = before.get(&c.id) else { continue };
            if old != &c.state {
                // Any change, for settings `events` that want more than the three below: `.from` and `.chat.state`.
                fired.push(("chat.state", c.clone(), old.clone()));
            }
            let event = match (old.as_str(), c.state.as_str()) {
                (o, "waiting") if o != "waiting" => "chat.waiting",
                ("working", "idle") => "chat.finished",
                ("working" | "waiting" | "idle", "stopped") => "chat.stopped",
                _ => continue,
            };
            fired.push((event, c.clone(), old.clone()));
        }
        for (event, chat, from) in fired {
            self.fire_event(event, vec![("chat".into(), to_val(&self.chat_json(&chat))), ("from".into(), to_val(&Value::String(from)))], window, cx);
        }
    }

    fn open_chat_from_notification(&mut self, chat: &str, window: &mut Window, cx: &mut Context<Self>) {
        window.activate_window();
        cx.activate(true);
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
        if self.scripts.is_some() && event != "model" {
            let payload: Map<String, Value> = extra.iter().map(|(k, v)| (k.clone(), crate::ui::expr::to_json(v))).collect();
            self.pending_script.push(json!({"type": "event", "name": event, "payload": payload}));
        }
        self.play_event_sound(event, &extra);
        let Some(spec) = self.settings.events.get(event).cloned() else { return };
        let ctx = crate::ui::expr::with(&self.context(), extra);
        if let Some(cond) = spec.get("if").and_then(Value::as_str) {
            if !self.exprs.truthy(cond, &ctx) {
                return;
            }
        }
        let action = self.exprs.resolve_action(spec.get("do").unwrap_or(&Value::Null), &ctx);
        self.run_action(&action, window, cx);
    }

    /** `sounds` from settings: a sound per event (a finished turn that ends in a question plays
    `chat.question`), whether or not the window is focused unless `when` is "unfocused". */
    fn play_event_sound(&self, event: &str, extra: &[(String, Val)]) {
        let sounds = &self.settings.sounds;
        if sounds.get("enabled") == Some(&Value::Bool(false)) || (sounds.get("when").and_then(Value::as_str) == Some("unfocused") && self.focused) {
            return;
        }
        let asks = event == "chat.finished" && extra.iter().any(|(k, v)| k == "chat" && crate::ui::expr::to_json(v).get("asks") == Some(&Value::Bool(true)));
        let key = if asks { "chat.question" } else { event };
        if let Some(name) = sounds.get(key).and_then(Value::as_str) {
            #[cfg(target_os = "macos")]
            crate::macos::play_sound(name, sounds.get("volume").and_then(Value::as_f64).unwrap_or(1.) as f32);
        }
    }

    /** problems.txt lets an agent that just edited settings see what broke. */
    fn write_problems(&mut self) {
        let mut lines: Vec<String> = self.settings_error.iter().cloned().collect();
        lines.extend(self.settings.warnings.iter().cloned());
        lines.extend(self.scripts.iter().flat_map(|s| s.errors.iter().cloned()));
        lines.extend(self.exprs.errors.borrow().iter().cloned());
        let text = if lines.is_empty() { "No problems.\n".to_string() } else { lines.join("\n") + "\n" };
        if text != self.problems_written {
            let _ = std::fs::write(settings::dir().join("problems.txt"), &text);
            self.problems_written = text;
        }
    }

    pub(crate) fn set_state(&mut self, key: String, value: Value) {
        self.ctx_rev += 1;
        let regroup = key == "sidebar_projects";
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
        self.ctx_rev += 1;
        match msg {
            Incoming::Connected => {
                self.host_up.insert(host.to_string(), true);
                self.connected = self.host_up.values().any(|u| *u);
                self.status = self.tr("connected", "Connected", &[]);
                self.watching.clear();
                self.configure_host(host);
                let remote = self.remote_for_host(host);
                cx.spawn(async move |_, _| {
                    let _ = remote.call(Call::Hello { client: "agent-box-app".into() }).await;
                    let _ = remote.call(Call::Subscribe { from_seq: None }).await;
                })
                .detach();
            }
            Incoming::Disconnected(why) => {
                self.host_up.insert(host.to_string(), false);
                self.connected = self.host_up.values().any(|u| *u);
                self.status = self.tr("reconnecting", "Reconnecting to {host}… ({why})", &[("host", host), ("why", &why)]);
                self.terminals.retain(|_, t| t.read(cx).host != host);
                self.term_by_id.retain(|(h, _), _| h != host);
            }
            Incoming::Event(Event::Model(m)) => {
                let before: HashMap<String, String> = self.model.chats.iter().map(|c| (c.id.clone(), c.state.clone())).collect();
                self.models.insert(host.to_string(), m);
                self.merge_models();
                if !before.is_empty() {
                    self.fire_chat_events(&before, window, cx);
                    self.flush_queue();
                }
                let ctx = self.context();
                self.update_badge(&ctx);
                self.pending_motion = true;
                self.mark_diffs_stale();
                // A moved chat: follow it to its worktree on the machine it went to, tab open.
                if let Some((id, to)) = self.pending_move.clone() {
                    if let Some(path) = self.all_worktrees().iter().find(|w| w.host == to && w.chat_ids.contains(&id)).map(|w| w.path.clone()) {
                        self.opened.insert(id.clone());
                        self.closed.remove(&id);
                        self.active_chat.insert(path.clone(), id);
                        self.selected = Some(path);
                        self.pending_move = None;
                    }
                }
                if let Some(id) = self.pending_select.clone() {
                    if let Some(path) = self.all_worktrees().iter().find(|w| w.chat_ids.contains(&id)).map(|w| w.path.clone()) {
                        self.opened.insert(id.clone());
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
                        .or_else(|| self.all_worktrees().first().map(|w| w.path.clone()));
                }
                self.sync_selection(window, cx);
            }
            Incoming::Event(Event::Transcript { chat_id, from, entries }) => {
                self.transcripts.entry(chat_id.clone()).or_insert_with(|| crate::transcript::Transcript::for_chat(&chat_id)).append(from, &entries);
            }
            Incoming::Event(Event::TermOutput { term_id, data }) => {
                if let Some(t) = self.term_by_id.get(&(host.to_string(), term_id)).cloned() {
                    t.update(cx, |t, cx| t.output(&data, cx));
                }
                return;
            }
            Incoming::Event(Event::AppAction { action }) => {
                self.log_action(&json!({"from": host, "action": "relayed"}));
                self.run_action(&action, window, cx);
            }
            Incoming::Event(Event::Hook(e)) => {
                let keep = self.settings.num("inspect.hook_events", 50.) as usize;
                self.hooks.push(json!({"host": host, "seq": e.seq, "at": e.at, "event": e.event, "session_id": e.session_id}));
                if self.hooks.len() > keep {
                    let extra = self.hooks.len() - keep;
                    self.hooks.drain(..extra);
                }
            }
            Incoming::Event(_) => {}
        }
        self.flush(window, cx);
        cx.notify();
    }

    /** Every host's model in one: this Mac's projects and sections first, then the box's. */
    fn merge_models(&mut self) {
        let mut hosts: Vec<&String> = self.models.keys().collect();
        // `sidebar.host_order`: whose worktrees come first in a merged project.
        let order: Vec<String> = self.settings.get("sidebar.host_order").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_else(|| vec!["mac".into(), "box".into()]);
        hosts.sort_by_key(|h| order.iter().position(|x| x == *h).unwrap_or(order.len()));
        let mut merged = Model { host: "all".into(), ..Default::default() };
        let (mut project_hosts, mut section_hosts): (Vec<Vec<String>>, HashMap<String, String>) = (vec![], HashMap::new());
        // state.sidebar_projects: "merged" (default) lists each repo once with every machine's worktrees;
        // "by_host" keeps a project per machine.
        let by_host = self.state.get("sidebar_projects").and_then(Value::as_str) == Some("by_host");
        for h in hosts {
            let m = &self.models[h];
            if h == "box" {
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
                section_hosts.insert(s.id.clone(), h.clone());
                merged.sections.push(s);
            }
            merged.chats.extend(m.chats.iter().cloned());
        }
        self.model = merged;
        self.apply_just_sent();
        self.project_hosts = project_hosts;
        self.section_hosts = section_hosts;
    }

    /** Asks every connected host for its past chats; `.history` holds them newest first. */
    pub(crate) fn load_history(&mut self, cx: &mut Context<Self>) {
        self.history_loading = true;
        let remotes: Vec<(String, Remote)> = self.remotes.iter().filter(|(h, _)| self.host_up.get(*h) == Some(&true)).map(|(h, r)| (h.clone(), r.clone())).collect();
        let limit = self.settings.num("history.limit", 300.) as usize;
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

    pub(crate) fn host_of_path(&self, path: &str) -> String {
        self.worktree(path).map(|w| w.host.clone()).unwrap_or_else(|| "box".into())
    }

    /** A chat's daemon: the host whose worktree holds it, else the chat's own host if connected. */
    pub(crate) fn host_of_chat(&self, id: &str) -> String {
        if let Some(w) = self.all_worktrees().into_iter().find(|w| w.chat_ids.iter().any(|c| c == id)) {
            return w.host.clone();
        }
        self.chat(id).map(|c| c.host.clone()).filter(|h| self.remotes.contains_key(h)).unwrap_or_else(|| "box".into())
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
            | Call::ChatSend { chat_id, .. }
            | Call::ChatKeys { chat_id, .. } => self.host_of_chat(chat_id),
            Call::ChatNew { dir: Some(d), .. } => self.host_of_path(d),
            Call::SectionRename { id, .. } | Call::SectionDelete { id } | Call::SectionMove { id, .. } => {
                self.section_hosts.get(id).cloned().unwrap_or_else(|| "box".into())
            }
            Call::SectionAssign { path, .. } | Call::WorktreeDelete { path } | Call::WorktreeMove { path, .. } | Call::WorktreeLabel { path, .. } | Call::WorktreeRenameBranch { path, .. } | Call::WorktreeDiff { path, .. } => {
                self.host_of_path(path)
            }
            Call::TermOpen { cwd: Some(c), .. } => self.host_of_path(c),
            _ => "box".into(),
        };
        self.remote_for_host(&host)
    }

    pub(crate) fn all_worktrees(&self) -> Vec<&Worktree> {
        self.model.projects.iter().flat_map(|p| p.items.iter()).chain(self.model.sections.iter().flat_map(|s| s.items.iter())).collect()
    }

    pub(crate) fn worktree(&self, path: &str) -> Option<&Worktree> {
        self.all_worktrees().into_iter().find(|w| w.path == path)
    }

    pub(crate) fn chat(&self, id: &str) -> Option<&Chat> {
        self.model.chats.iter().find(|c| c.id == id).or_else(|| self.sub_chats.iter().find(|(_, c)| c.id == id).map(|(_, c)| c))
    }

    /** A worktree's terminal tabs (`state.terms:PATH`): [{id, label, command?}]; the first shell has id "1". */
    pub(crate) fn term_tabs(&self, path: &str) -> Vec<Value> {
        match self.state.get(&format!("terms:{path}")).and_then(Value::as_array) {
            Some(t) if !t.is_empty() => t.clone(),
            _ => vec![json!({"id": "1", "label": "Terminal"})],
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
        let (remote, ts) = (self.remote_for_host(&w.host), self.settings.terminal.clone());
        let host = w.host.clone();
        let view = cx.new(|cx| TerminalView::shell(remote, host, None, Some(w.path.clone()), Some(id.clone()), command, &ts, cx));
        self.terminals.insert(key, view.clone());
        Some(view)
    }

    /** A worktree's chats in tab order (`state.tab_order[path]`, set by dragging tabs), the rest after. */
    fn chats_of(&self, w: &Worktree) -> Vec<&Chat> {
        let mut chats: Vec<&Chat> = w.chat_ids.iter().filter_map(|id| self.chat(id)).collect();
        chats.extend(self.sub_chats.iter().filter(|(p, _)| *p == w.path).map(|(_, c)| c));
        if let Some(order) = self.state.get("tab_order").and_then(|o| o.get(&w.path)).and_then(Value::as_array) {
            let rank = |id: &str| order.iter().position(|x| x.as_str() == Some(id)).unwrap_or(usize::MAX);
            chats.sort_by_key(|c| rank(&c.id));
        }
        chats
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
    }

    pub(crate) fn current_worktree(&self) -> Option<&Worktree> {
        self.selected.as_deref().and_then(|p| self.worktree(p))
    }

    pub(crate) fn current_chat(&self) -> Option<&Chat> {
        let w = self.current_worktree()?;
        let chats = self.chats_of(w);
        let active = self.active_chat.get(&w.path);
        let recent = self.settings.chat.get("open_on_select").and_then(Value::as_str) == Some("recent");
        active
            .and_then(|id| chats.iter().find(|c| &c.id == id && !self.closed.contains(&c.id)).copied())
            .or_else(|| chats.iter().find(|c| self.chat_open(c)).copied())
            .or_else(|| if recent { chats.first().copied() } else { None })
    }

    /** Everything expressions can read, rebuilt each frame: model, state, theme, selection, app. */
    pub(crate) fn context(&self) -> Val {
        let worktrees: Vec<Value> = self
            .model
            .projects
            .iter()
            .flat_map(|p| p.items.iter().map(move |w| (w, Value::Null)))
            .chain(self.model.sections.iter().flat_map(|s| s.items.iter().map(move |w| (w, json!({"id": s.id, "name": s.name})))))
            .map(|(w, section)| self.worktree_json(w, section))
            .collect();
        let project_ids: Vec<String> = self.model.projects.iter().filter(|p| !self.project_removed(&p.repo)).map(|p| p.repo.clone()).collect();
        let project_gap = self.card_gap("project", "projcard", &project_ids);
        let section_ids: Vec<String> = self.model.sections.iter().map(|s| s.id.clone()).collect();
        let section_gap = self.card_gap("section", "seccard", &section_ids);
        let projects: Vec<Value> = self
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
                let gaps = self.drop_gaps(&shown);
                let primary = p.items.iter().find(|w| w.is_primary).or(p.items.first());
                json!({"name": name, "repo": p.repo, "hue": hue, "letter": letter,
                       "hosts": self.project_hosts.get(i).cloned().unwrap_or_else(|| vec!["box".into()]),
                       "host": self.project_hosts.get(i).and_then(|h| h.first().cloned()).unwrap_or_else(|| "box".into()),
                       "show_external": show_external, "show_primary": show_primary,
                       "external_count": p.items.iter().filter(|w| w.external).count(),
                       "hidden_count": p.items.iter().chain(self.model.sections.iter().flat_map(|s| s.items.iter())).filter(|w| w.repo == p.repo && self.flag(&format!("hidden:{}", w.path))).count(),
                       "primary_path": primary.map(|w| w.path.clone()), "primary_host": primary.map(|w| w.host.clone()),
                       "drop_gap": gap.as_deref() == Some(p.repo.as_str()), "drop_gap_end": gap_end.as_deref() == Some(p.repo.as_str()),
                       "items": shown.into_iter().map(|w| self.row_json(w, Value::Null, &gaps)).collect::<Vec<_>>()})
            })
            .collect();
        let sections: Vec<Value> = self
            .model
            .sections
            .iter()
            .map(|s| {
                // A section's orchestrator leads it; its workers follow.
                let mut shown = self.visible_items(&s.items, |_| true);
                if self.settings.get("sidebar.orchestrators_first").and_then(Value::as_bool) != Some(false) {
                    shown.sort_by_key(|w| !self.chats_of(w).iter().any(|c| c.kind == "orchestrator"));
                }
                let gaps = self.drop_gaps(&shown);
                let (gap, gap_end) = &section_gap;
                json!({"id": s.id, "name": s.name, "collapsed": s.collapsed,
                       "drop_gap": gap.as_deref() == Some(s.id.as_str()), "drop_gap_end": gap_end.as_deref() == Some(s.id.as_str()),
                       "host": self.section_hosts.get(&s.id).cloned().unwrap_or_else(|| "box".into()),
                       "items": shown.into_iter().map(|w| self.row_json(w, json!({"id": s.id, "name": s.name}), &gaps)).collect::<Vec<_>>()})
            })
            .collect();
        let into = self.drop_into();
        let removed: Vec<Value> = self.model.projects.iter().filter(|p| self.project_removed(&p.repo)).map(|p| json!({"repo": p.repo, "name": self.project_look(&p.repo, &p.name, p.hue).0})).collect();
        let selected_w = self.current_worktree().map(|w| self.worktree_json(w, Value::Null));
        let selected_diff = self.selected.as_deref().map(|p| self.diff_summary(p)).unwrap_or(Value::Null);
        let selected_c = self.current_chat().map(|c| self.chat_json(c));
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
        let chats_here: Vec<Value> = self.current_worktree().map(|w| self.chats_of(w).into_iter().map(|c| self.chat_json(c)).collect()).unwrap_or_default();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.);
        // Usage is filed under the agent's own session id (Codex picks its own; Claude's is the chat id).
        let usage_id = |c: &Chat| c.session.clone().unwrap_or_else(|| c.id.clone());
        let tabs: Vec<(String, String)> = self.current_worktree().map(|w| self.chats_of(w).into_iter().filter(|c| self.chat_open(c)).map(|c| (usage_id(c), c.name.clone())).collect()).unwrap_or_default();
        let current_usage_id = self.current_chat().map(usage_id);
        let usage = self.usage.context(current_usage_id.as_deref(), self.current_worktree().map(|w| (w.host.as_str(), w.path.as_str())), &tabs, &self.usage_providers(), &self.meter_options());
        to_val(&json!({
            "projects": projects,
            "sections": sections,
            "worktrees": worktrees,
            "chats": self.model.chats.iter().chain(self.sub_chats.iter().map(|(_, c)| c)).map(|c| self.chat_json(c)).collect::<Vec<_>>(),
            "selected": {"worktree": selected_w, "chat": selected_c, "chats": chats_here, "diff": selected_diff,
                         "terminals": terminals, "background": background, "move": self.move_plan()},
            "state": self.state,
            "theme": self.settings.theme,
            "hosts": self.settings.hosts,
            "agents": self.settings.get("daemon.agents").cloned().unwrap_or(Value::Null),
            "history": self.history,
            "app": {
                "confirm": self.confirm.as_ref().map(|c| { let mut c = c.clone(); if let Value::Object(o) = &mut c { o.remove("then"); } c }),
                "starting": self.starting,
                "hosts_up": self.host_up,
                "completion": self.completion.as_ref().map(|(chat, trigger, items, selected)| json!({
                    "chat": chat, "trigger": trigger, "items": crate::completions::to_json(items), "selected": selected
                })),
                "history_loading": self.history_loading,
                "connected": self.connected,
                "removed_projects": removed,
                "notifications": self.notices.iter().map(|(at, text)| json!({"at": at, "text": text})).collect::<Vec<_>>(),
                "dialog": self.dialog.as_ref().map(|d| json!({"title": d.title, "fields": d.inputs.iter().map(|(l, _)| l.to_string()).collect::<Vec<_>>()})),
                "palette_open": self.palette_open,
                "focus_area": self.focus_now,
                "nav": {"back": self.can_navigate(-1), "forward": self.can_navigate(1)},
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
        }))
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

    fn row_json(&self, w: &Worktree, section: Value, gaps: &[String]) -> Value {
        let mut v = self.worktree_json(w, section);
        if let Value::Object(o) = &mut v {
            // gaps[0] opens the room; a target whose room opens on the row below (gaps[1]) hides its drop line.
            o.insert("drop_gap".into(), Value::Bool(gaps.first() == Some(&w.path)));
            o.insert("drop_room".into(), Value::Bool(gaps.contains(&w.path)));
            o.insert("drag_source".into(), Value::Bool(self.drag_item.borrow().as_ref().is_some_and(|(k, v)| k == "worktree" && v.as_str() == Some(&w.path))));
        }
        v
    }

    fn worktree_json(&self, w: &Worktree, section: Value) -> Value {
        let chats = self.chats_of(w);
        // Which status wins when several apply: `sidebar.status_order`.
        let applies = |s: &str| match s {
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
            .unwrap_or_else(|| ["working", "waiting", "merged", "failing", "pending", "dirty", "behind"].map(String::from).to_vec());
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
            "host": w.host,
            "chats": chats.iter().map(|c| self.chat_json(c)).collect::<Vec<_>>(),
        })
    }

    fn chat_json(&self, c: &Chat) -> Value {
        let mut v = serde_json::to_value(c).unwrap_or(Value::Null);
        if let Value::Object(o) = &mut v {
            o.insert("active".into(), Value::Bool(self.current_chat().map(|x| x.id == c.id).unwrap_or(false)));
            o.insert("open".into(), Value::Bool(self.chat_open(c)));
            o.insert("queued".into(), json!(self.queued.get(&c.id).cloned().unwrap_or_default()));
            o.insert("age_text".into(), Value::String(age_text(c.last_activity, &self.age_format())));
            o.insert("attachments".into(), self.attachments_json(&c.id));
            let last = self.transcripts.get(&c.id).and_then(|t| t.items.iter().rev().find_map(|i| if let Item::Assistant { text } = i { Some(text.clone()) } else { None }));
            o.insert("asks".into(), Value::Bool(last.as_deref().is_some_and(|t| self.asks(t))));
            o.insert("last_message".into(), last.map(|t| Value::String(t.chars().take(self.settings.num("chat.last_message_chars", 280.) as usize).collect())).unwrap_or(Value::Null));
            let activity = self.transcripts.get(&c.id).filter(|_| c.state == "working").map(|t| t.activity()).unwrap_or_default();
            o.insert("activity".into(), Value::String(activity));
        }
        v
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
            if !self.watching.contains(&chat.id) {
                self.watching.insert(chat.id.clone());
                let from = self.transcripts.get(&chat.id).map(|t| t.lines).unwrap_or(0);
                let remote = self.route(&Call::TranscriptUnwatch { chat_id: chat.id.clone() });
                let id = chat.id.clone();
                // Only the newest `chat.transcript_tail` entries on first load: dispatcher transcripts run to 80MB.
                let tail = self.settings.chat.get("transcript_tail").and_then(Value::as_u64).map(|n| n as usize).filter(|_| from == 0);
                cx.spawn(async move |this, cx| {
                    if let Ok(v) = remote.call(Call::TranscriptRead { chat_id: id.clone(), from, tail, before: None }).await {
                        let entries = v["entries"].as_array().cloned().unwrap_or_default();
                        let start = v["from"].as_u64().unwrap_or(from as u64) as usize;
                        this.update(cx, |ws, cx| {
                            ws.transcripts.entry(id.clone()).or_insert_with(|| crate::transcript::Transcript::for_chat(&id)).append(start, &entries);
                            cx.notify();
                        })
                        .ok();
                    }
                })
                .detach();
            }
            self.ensure_composer(&chat.id, window, cx);
        }
        if !self.terminals.contains_key(&w.path) && self.connected {
            let remote = self.remote_for_host(&w.host);
            let ts = self.settings.terminal.clone();
            let path = w.path.clone();
            let host = w.host.clone();
            let view = cx.new(|cx| TerminalView::new(remote, host, None, Some(path.clone()), &ts, cx));
            self.terminals.insert(w.path.clone(), view);
        }
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
                    if ws.completion.as_ref().is_some_and(|c| c.0 == id) && !*shift {
                        ws.accept_completion(None, window, cx);
                    } else if (*secondary && cmd_enter_sends) || (enter_sends && !*shift && !*secondary) {
                        ws.send_composer(&id, window, cx);
                    }
                }
                InputEvent::Change => ws.update_completion(&id, cx),
                _ => {}
            }
        }));
        self.composers.insert(chat_id.to_string(), state);
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
            let local = w.as_ref().filter(|w| w.host != "mac").map(|w| format!("~/dev/{}", w.project));
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

    /** Tab: takes the highlighted completion, or fills in Claude Code's suggestion when the box is empty. */
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
    pub(crate) fn flush_queue(&mut self) {
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
            self.send_chat(&id, text);
            self.ctx_rev += 1;
        }
    }

    /** Sends settings `daemon` to a host's daemon (its timings, limits, and parsing rules). */
    fn configure_host(&self, host: &str) {
        let options = self.settings.get("daemon").cloned().unwrap_or(Value::Object(Map::new()));
        self.remote_for_host(host).send(Call::Configure { options });
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

    /** An in-window notification, kept in `.app.notifications` (the newest `notifications.keep`) for agents. */
    pub(crate) fn toast(&mut self, text: impl Into<String>, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::WindowExt as _;
        let text: String = text.into();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.);
        self.notices.push((now, text.clone()));
        let keep = self.settings.num("notifications.keep", 20.) as usize;
        if self.notices.len() > keep {
            let extra = self.notices.len() - keep;
            self.notices.drain(..extra);
        }
        self.ctx_rev += 1;
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
    pub(crate) fn send_chat(&mut self, id: &str, text: String) {
        self.just_sent.insert(id.to_string(), std::time::Instant::now());
        self.apply_just_sent();
        self.transcripts.entry(id.to_string()).or_default().echo(&text);
        let call = Call::ChatSend { chat_id: id.to_string(), text };
        self.route(&call).send(call);
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

    /** Fetches every connected machine's token usage, reloads the plan's limits (refreshing them from
    Claude Code's endpoint when stale or `force`), and models.dev rates once a day. */
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

    /** Records a press for the innermost node that sees it (the first to call this for that event). */
    pub(crate) fn note_press(&mut self, id: &str, at: Point<Pixels>, count: usize) {
        let now = std::time::Instant::now();
        if self.press.as_ref().is_some_and(|(_, t, p)| *p == at && now.duration_since(*t) < std::time::Duration::from_millis(50)) {
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
            Some((who, t, p)) if *p == at && t.elapsed() < std::time::Duration::from_millis(50) => who == id,
            _ => true,
        }
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

    pub(crate) fn refresh_usage(&mut self, force: bool, cx: &mut Context<Self>) {
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
        // Plans read from a file (Claude Code's status line keeps ~/.claude/oauth-usage-cache.json fresh): when
        // it's older than the provider's `plan_max_age_secs` (or on Refresh), run its `plan_command`.
        for p in usage_providers.values() {
            if p["plan"].as_str() != Some("claude_oauth") {
                continue;
            }
            let file = p["plan_file"].as_str().unwrap_or("~/.claude/oauth-usage-cache.json").to_string();
            self.usage.load_plan(&file);
            let plan_age = self.usage.plan_updated.map(|t| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|n| n.as_secs_f64() - t).unwrap_or(0.)).unwrap_or(f64::MAX);
            let max_age = p["plan_max_age_secs"].as_f64().unwrap_or_else(|| self.settings.num("usage.plan_max_age_secs", 300.));
            if !(force || plan_age > max_age) {
                continue;
            }
            let script = p["plan_command"].as_str().or(self.settings.get("usage.plan_command").and_then(Value::as_str)).unwrap_or(PLAN_COMMAND).to_string();
            cx.spawn(async move |this, cx| {
                let _ = cx.background_spawn(async move { std::process::Command::new("/bin/sh").arg("-c").arg(script).output() }).await;
                this.update(cx, |ws, cx| {
                    ws.usage.load_plan(&file);
                    ws.ctx_rev += 1;
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        let hosts: Vec<(String, Remote)> = self.remotes.iter().filter(|(h, _)| self.host_up.get(*h) == Some(&true)).map(|(h, r)| (h.clone(), r.clone())).collect();
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
            self.send_chat(id, text);
        }
        self.jump_to_latest(id);
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
        cx.spawn(async move |_, _| {
            let _ = remote.call(call).await;
        })
        .detach();
    }

    pub(crate) fn start_chat(&mut self, call: Call, cx: &mut Context<Self>) {
        let remote = self.route(&call);
        self.status = self.tr("starting_chat", "Starting a chat…", &[]);
        cx.spawn(async move |this, cx| {
            let res = remote.call(call).await;
            this.update(cx, |ws, cx| {
                match res {
                    Ok(v) => {
                        if let Some(id) = v["stdout"].as_str().and_then(|o| o.split_whitespace().next()) {
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
        for (_, input) in &inputs {
            let s2 = submit.clone();
            self._subscriptions.push(cx.subscribe_in(input, window, move |_, _, ev: &InputEvent, window, cx| {
                if matches!(ev, InputEvent::PressEnter { .. }) {
                    s2(window, cx);
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
        self.dialog = Some(OpenDialog { title: title_text, inputs, need_first, done });
        if let Some(f) = focus {
            f.update(cx, |s, cx| s.focus(window, cx));
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
        if let Some(values) = values {
            for ((_, input), v) in d.inputs.iter().zip(values) {
                input.update(cx, |s, cx| s.set_value(v, window, cx));
            }
        }
        let vals: Vec<String> = d.inputs.iter().map(|(_, i)| i.read(cx).value().trim().to_string()).collect();
        if d.need_first && vals.first().is_none_or(|v| v.is_empty()) {
            return;
        }
        let done = d.done.clone();
        self.dialog = None;
        done(self, vals, cx);
        window.close_dialog(cx);
    }

    pub(crate) fn cancel_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::WindowExt as _;
        if self.dialog.take().is_some() {
            window.close_dialog(cx);
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
        })
    }

    /** The open dialog for inspect.json: title and each field's label and current text. */
    pub(crate) fn dialog_json(&self, cx: &App) -> Value {
        self.dialog.as_ref().map(|d| json!({
            "title": d.title,
            "fields": d.inputs.iter().map(|(l, i)| json!({"label": l.to_string(), "value": i.read(cx).value().to_string()})).collect::<Vec<_>>(),
        })).unwrap_or(Value::Null)
    }

    pub(crate) fn new_section(&mut self, assign: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let title = self.tr("new_section_title", "New section", &[]);
        self.name_dialog(&title, "", window, cx, move |ws, name, cx| {
            let remote = assign.as_deref().map(|p| ws.remote_for_host(&ws.host_of_path(p))).unwrap_or_else(|| ws.remote.clone());
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
        });
    }

    pub(crate) fn run_local(&mut self, args: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        let home = std::env::var("HOME").unwrap_or_default();
        let mut cmd = std::process::Command::new(format!("{home}/dev/agent-box/bin/{}", args[0]));
        cmd.args(&args[1..]);
        self.run_command(cmd, true, window, cx);
    }

    pub(crate) fn run_shell(&mut self, script: String, env: Vec<(String, String)>, notify: bool, window: &mut Window, cx: &mut Context<Self>) {
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.arg("-c").arg(script).envs(env);
        self.run_command(cmd, notify, window, cx);
    }

    /** Runs a Mac command off the UI thread with a usable PATH; `notify` shows its last output line. */
    fn run_command(&mut self, mut cmd: std::process::Command, notify: bool, window: &mut Window, cx: &mut Context<Self>) {
        let home = std::env::var("HOME").unwrap_or_default();
        cmd.env("PATH", format!("{home}/.local/bin:{home}/.cargo/bin:{home}/dev/redo-monorepo-worktrees/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"));
        if notify {
            let t = self.tr("working", "Working…", &[]);
            self.toast(t, window, cx);
        }
        let (done, failed) = (self.tr("done", "Done", &[]), self.tr("failed", "Failed", &[]));
        cx.spawn_in(window, async move |this, cx| {
            let out = cx.background_spawn(async move { cmd.output() }).await;
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
    (`palette.worktrees`, labeled by `palette.worktree_label`). */
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
                let label = fmt.replace("{title}", &w.title).replace("{branch}", w.branch.as_deref().unwrap_or("")).replace("{project}", &w.project);
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

    pub(crate) fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::command::{Command, CommandItem, CommandState};
        use gpui_kit::component::WindowExt as _;
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
                        .child(gpui_kit::component::button::Button::new("docs").label("Reference").on_click(move |_, _, _| {
                            let _ = std::process::Command::new("open").arg(&docs).spawn();
                        }))
                        .child(gpui_kit::component::button::Button::new("save-settings").label("Save").on_click(move |_, _, cx| {
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
            self.load_earlier(&chat_id, cx);
        }
        let loading_earlier = self.loading_earlier.contains(&chat_id).then(|| {
            let st = self.settings.chat.get("loading_chip").or_else(|| self.settings.chat.get("jump")).and_then(Value::as_object).cloned().unwrap_or_default();
            let label = self.settings.chat.get("jump_labels").and_then(|l| l.get("loading")).and_then(Value::as_str).unwrap_or("Loading earlier messages…").to_string();
            div().absolute().top(px(self.settings.num("chat.loading_chip_top", 10.) as f32)).left_0().right_0().flex().justify_center()
                .child(style::apply(h_flex().gap(px(6.)).items_center(), &st, &theme).child(self.spinner(Some("xsmall"), None, cx)).child(label))
                .into_any_element()
        });
        let row_view = chat_id.clone();
        let rows = list(state.clone(), cx.processor(move |ws: &mut Workspace, ix: usize, window, cx| ws.transcript_row(&row_view, ix, window, cx))).size_full();
        let jump = self.jump_chip(&chat_id, &state, &theme, window, cx);
        let overlay = self.drop_overlay(&chat_id, cx);
        let shift_holds = cs.get("code_scroll").and_then(|c| c.get("shift_holds_transcript")).and_then(Value::as_bool) != Some(false);
        // Files dropped anywhere on the transcript attach to its chat, whether or not the overlay that
        // names the chat has rendered yet.
        let (drop_view, drop_chat) = (cx.entity().downgrade(), chat_id.to_string());
        div()
            .id(SharedString::from(format!("transcript-drop:{chat_id}")))
            .relative()
            .size_full()
            .on_drop(move |paths: &ExternalPaths, window, cx| {
                let files = paths.paths().iter().cloned().map(Attachment::Path).collect();
                let chat = drop_chat.clone();
                let _ = drop_view.update(cx, |ws, cx| ws.attach(chat, files, window, cx));
            })
            .child(rows)
            .when_some(crate::ui::render::scrollbar_mode(cs.get("scrollbar")), |d, m| d.child(gpui_kit::component::scroll::Scrollbar::vertical(&state).mode(m)))
            .when(shift_holds, |d| d.child(crate::ui::trace_view::shift_scroll_layer(None, 1.)))
            .children(jump)
            .children(loading_earlier)
            .children(overlay)
            .into_any_element()
    }

    /** Loads `chat.transcript_page` more of a chat's older lines (when scrolled to the top), keeping
    the view where it was. */
    pub(crate) fn load_earlier(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        let Some(t) = self.transcripts.get(chat_id) else { return };
        if t.start == 0 || !self.loading_earlier.insert(chat_id.to_string()) {
            return;
        }
        let page = self.settings.chat.get("transcript_page").and_then(Value::as_u64).unwrap_or(600) as usize;
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
            let mut restore = false;
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
                    Block::Steps(v) => v.contains(&item),
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
        let Some(t) = self.transcripts.remove(chat_id) else { return div().into_any_element() };
        let row = self.render_block(chat_id, &t.items, &blocks, ix, working, window, cx);
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
        let textarea = Textarea::new(&c).appearance(false).on_paste(move |item, window, cx| {
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
                    move |_: &gpui_kit::component::input::MoveUp, _, cx| {
                        let _ = v.update(cx, |ws, cx| {
                            if ws.completion.as_ref().is_some_and(|x| x.0 == c) {
                                ws.move_completion(-1, cx);
                                cx.stop_propagation();
                            }
                        });
                    }
                })
                .capture_action({
                    let (v, c) = (key_view.clone(), key_chat.clone());
                    move |_: &gpui_kit::component::input::MoveDown, _, cx| {
                        let _ = v.update(cx, |ws, cx| {
                            if ws.completion.as_ref().is_some_and(|x| x.0 == c) {
                                ws.move_completion(1, cx);
                                cx.stop_propagation();
                            }
                        });
                    }
                })
                .capture_action({
                    let (v, c) = (key_view.clone(), key_chat.clone());
                    move |_: &gpui_kit::component::input::Escape, _, cx| {
                        let _ = v.update(cx, |ws, cx| {
                            if ws.completion.as_ref().is_some_and(|x| x.0 == c) {
                                ws.completion = None;
                                ws.ctx_rev += 1;
                                cx.notify();
                                cx.stop_propagation();
                            }
                        });
                    }
                })
                .capture_action({
                    let (v, c) = (key_view.clone(), key_chat.clone());
                    move |e: &gpui_kit::component::input::Enter, window, cx| {
                        let _ = v.update(cx, |ws, cx| {
                            if !e.shift && !e.secondary && ws.completion.as_ref().is_some_and(|x| x.0 == c) {
                                ws.accept_completion(None, window, cx);
                                cx.stop_propagation();
                            }
                        });
                    }
                })
                .capture_action({
                    let (v, c) = (key_view.clone(), key_chat.clone());
                    move |_: &gpui_kit::component::input::IndentInline, window, cx| {
                        let _ = v.update(cx, |ws, cx| ws.on_tab(&c, window, cx));
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
        let message = w["message"].as_str().or(w["tool_name"].as_str()).map(str::to_string).unwrap_or(fallback);
        let (options, questions) = waiting_options(&w, &self.settings);
        let mut row = style::apply(h_flex().gap(px(6.)).flex_wrap(), &st("waiting_row"), theme);
        let cancel = self.tr("waiting_cancel", "Cancel", &[]);
        for (i, label) in options.iter().enumerate().chain(std::iter::once((usize::MAX, &cancel))) {
            let id = chat.id.clone();
            let key = if i == usize::MAX { "Escape".to_string() } else { (i + 1).to_string() };
            let option = if i == usize::MAX { json!("cancel") } else { json!(i) };
            self.clickables.push((format!("opt:{}:{i}", chat.id), label.clone(), json!({"do": "chat.answer", "chat": chat.id, "option": option})));
            row = row.child(
                style::apply(div().id(SharedString::from(format!("opt:{}:{i}", chat.id))).cursor_pointer(), &st("waiting_option"), theme)
                    .child(label.clone())
                    .on_click(cx.listener(move |ws, _, _, cx| ws.call(Call::ChatKeys { chat_id: id.clone(), keys: vec![key.clone()] }, cx))),
            );
        }
        style::apply(v_flex().gap(px(10.)), &st("waiting"), theme)
            .child(div().child(message))
            .children(questions.into_iter().map(|q| div().child(q)))
            .child(row)
            .into_any_element()
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

    fn handle_keys(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let pressed = e.keystroke.unparse();
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
        // A key's `not_in` (["terminal", "composer", "input"]) leaves it to whatever has focus there.
        let found = self.settings.keys.iter().rev().find(|k| {
            k.get("key").and_then(Value::as_str) == Some(pressed.as_str())
                && !k.get("not_in").and_then(Value::as_array).is_some_and(|l| l.iter().any(|x| x.as_str() == Some(area)))
        }).cloned();
        if let Some(k) = found {
            let ctx = self.context();
            if let Some(cond) = k.get("if").and_then(Value::as_str) {
                if !self.exprs.truthy(cond, &ctx) {
                    return;
                }
            }
            let action = self.exprs.resolve_action(k.get("do").unwrap_or(&Value::Null), &ctx);
            self.run_action(&action, window, cx);
            cx.stop_propagation();
        }
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let started = std::time::Instant::now();
        self.register_terminals(cx);
        let t = std::time::Instant::now();
        self.flush(window, cx);
        self.track_nav();
        self.save_queue();
        self.save_drafts(cx);
        self.refresh_sub_chats();
        self.perf.borrow_mut().spans.insert("flush", t.elapsed().as_secs_f32() * 1000.);
        self.render_generation += 1;
        self.clickables_last = std::mem::take(&mut self.clickables);
        self.menus_last = std::mem::take(&mut self.menus);
        let generation = self.render_generation;
        self.entered.retain(|_, (_, seen, _)| *seen + 2 >= generation);
        self.transitions.retain(|_, t| t.seen + 2 >= generation);
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
            // A dialog closed by Escape or a click outside is gone from the state too.
            if (self.dialog.is_some() || self.palette_open) && !window.has_active_dialog(cx) {
                self.dialog = None;
                self.palette_open = false;
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
        self.focused = window.is_window_active();
        let t = std::time::Instant::now();
        let ctx = self.context();
        self.perf.borrow_mut().spans.insert("context", t.elapsed().as_secs_f32() * 1000.);
        self.view_handle = Some(cx.entity().downgrade());
        let t = std::time::Instant::now();
        let body = self.render_view("main", &ctx, window, cx);
        self.perf.borrow_mut().spans.insert("tree", t.elapsed().as_secs_f32() * 1000.);
        let t = std::time::Instant::now();
        self.write_inspect(&ctx, window, cx);
        self.perf.borrow_mut().spans.insert("inspect", t.elapsed().as_secs_f32() * 1000.);
        self.write_problems();
        let rendered = started.elapsed();
        let perf = self.perf.clone();
        let frame_end = canvas(|_, _, _| {}, move |_, _, _, _| perf.borrow_mut().record(started, rendered)).absolute().size_0();
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
            .on_key_down(cx.listener(Self::type_to_focus))
            .on_action(cx.listener(|ws, _: &OpenSettings, w, cx| ws.open_settings(w, cx)))
            .on_action(cx.listener(|ws, _: &Palette, w, cx| ws.open_palette(w, cx)))
            .on_action(cx.listener(|ws, item: &MenuBarItem, w, cx| {
                if let Some(action) = MENU_ACTIONS.with(|a| a.borrow().get(item.index).cloned()) {
                    let ctx = ws.context();
                    let action = ws.exprs.resolve_action(&action, &ctx);
                    ws.run_action(&action, w, cx);
                }
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
            .child(frame_end)
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

/** "now", "5m", "3h", "2d", "1w" (settings `format.age`: now, m, h, d, w, each with `{n}`). */
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
        self.blocks.iter().map(|b| match b { Block::One(i) => format!("one:{i}"), Block::Steps(v) => format!("steps:{}", v.len()), Block::Pending(t) => t.to_string() }).collect()
    }
}

pub(crate) struct OpenDialog {
    pub(crate) title: String,
    pub(crate) inputs: Vec<(SharedString, Entity<gpui_kit::component::input::InputState>)>,
    need_first: bool,
    done: std::rc::Rc<dyn Fn(&mut Workspace, Vec<String>, &mut Context<Workspace>)>,
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

fn attachments_from_clipboard(item: &ClipboardItem) -> Vec<Attachment> {
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

fn uploads_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".agent-box/uploads")
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
    let home = std::env::var("HOME").unwrap_or_default();
    let file = std::fs::File::open(&local).map_err(|e| e.to_string())?;
    let o = std::process::Command::new("ssh")
        .env("PATH", format!("{home}/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"))
        .args([host, &format!("mkdir -p ~/.agent-box/uploads && cat > ~/.agent-box/uploads/'{name}' && printf %s \"$HOME\"")])
        .stdin(file)
        .output()
        .map_err(|e| e.to_string())?;
    if !o.status.success() {
        return Err(String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("upload failed").to_string());
    }
    Ok(format!("{}/.agent-box/uploads/{name}", String::from_utf8_lossy(&o.stdout).trim()))
}

/** A message's attached uploads (by their local copies) and its text with their paths removed. */
pub(crate) fn split_uploads(text: &str) -> (Vec<std::path::PathBuf>, String) {
    let dir = uploads_dir();
    let mut files = vec![];
    let mut words = vec![];
    for w in text.split(' ') {
        match w.split("/.agent-box/uploads/").nth(1).map(|name| dir.join(name.trim())) {
            Some(p) if p.exists() => files.push(p),
            _ => words.push(w),
        }
    }
    (files, words.join(" ").trim().to_string())
}

/** The image's size scaled down to fit within `max_w` x `max_h`. */
pub fn fit(path: &std::path::Path, max_w: f32, max_h: f32) -> (f32, f32) {
    let Ok(d) = imagesize::size(path) else { return (max_w, max_h) };
    let (w, h) = (d.width as f32, d.height as f32);
    let k = (max_w / w).min(max_h / h).min(1.);
    (w * k, h * k)
}

pub(crate) fn decode_image(media_type: &str, data: &str) -> Option<std::sync::Arc<Image>> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD.decode(data).ok()?;
    let format = match media_type {
        "image/jpeg" => ImageFormat::Jpeg,
        "image/gif" => ImageFormat::Gif,
        "image/webp" => ImageFormat::Webp,
        _ => ImageFormat::Png,
    };
    Some(std::sync::Arc::new(Image::from_bytes(format, bytes)))
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

/** Refreshes Claude Code's plan-usage cache from its OAuth endpoint (the token stays in the keychain). */
/** One auto-update check in ~/dev/agent-box: fast-forward to origin/main when the checkout is clean, then
rebuild and restart the daemon if its code changed and bundle the app if the app's did. Prints "updated"
and what it rebuilt ("daemon", "app"), or nothing. */
const UPDATE_SCRIPT: &str = r#"cd "$HOME/dev/agent-box" || exit 0
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"
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

const PLAN_COMMAND: &str = r#"token=$(security find-generic-password -s 'Claude Code-credentials' -w 2>/dev/null | /usr/bin/python3 -c 'import json,sys; print(json.load(sys.stdin)["claudeAiOauth"]["accessToken"])' 2>/dev/null); [ -n "$token" ] && out=$(/usr/bin/curl -sS -m 10 https://api.anthropic.com/api/oauth/usage -H "Authorization: Bearer $token" -H "anthropic-beta: oauth-2025-04-20") && printf '%s' "$out" | grep -q '"limits"' && printf '%s' "$out" > "$HOME/.claude/oauth-usage-cache.json""#;

/** A cheap fingerprint of what a transcript row shows: its items' kinds, text lengths and results. */
/** The first transcript item a row shows (none for the "Working…" row). */
fn first_item(b: &crate::ui::trace_view::Block) -> Option<usize> {
    use crate::ui::trace_view::Block;
    match b {
        Block::One(i) => Some(*i),
        Block::Steps(v) => v.first().copied(),
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
        None => 0u8.hash(&mut h),
    };
    match b {
        Block::One(i) => one(*i),
        Block::Steps(v) => v.iter().for_each(|i| one(*i)),
        Block::Pending(t) => t.hash(&mut h),
    }
    h.finish()
}
