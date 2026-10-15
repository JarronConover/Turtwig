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

pub struct Drag {
    pub key: String,
    pub vertical: bool,
    pub invert: bool,
    pub start: f32,
    pub start_size: f32,
    pub min: f32,
    pub max: f32,
}

pub struct Workspace {
    _remote_guards: Vec<RemoteGuard>,
    /** One daemon connection per host: "box" over ssh, "mac" run locally. */
    pub(crate) remotes: HashMap<String, Remote>,
    /** Each host's latest model; `model` is them merged (this Mac's first). */
    pub(crate) models: HashMap<String, Model>,
    pub(crate) host_up: HashMap<String, bool>,
    /** Where each merged project and section came from, by index and id. */
    pub(crate) project_hosts: Vec<String>,
    pub(crate) section_hosts: HashMap<String, String>,
    /** Past chats from every host's `History`, for the history panel. */
    pub(crate) history: Vec<Value>,
    pub(crate) history_loading: bool,
    pub(crate) inputs: HashMap<String, Entity<gpui_kit::component::input::InputState>>,
    /** The open completion list under a composer: (chat, trigger, entries, selected). */
    pub(crate) completion: Option<(String, String, Vec<crate::completions::Entry>, usize)>,
    /** Messages typed while a chat was working, sent one per turn as it finishes (`chat.queue`). */
    pub(crate) queued: HashMap<String, Vec<String>>,
    /** This view, for click handlers built without a `cx` (message actions). */
    pub(crate) view_handle: Option<WeakEntity<Workspace>>,
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
    pub(crate) drop_preview: bool,
    pub(crate) inspect_at: Option<std::time::Instant>,
    pub(crate) inspect_digest: u64,
    pub(crate) activity: Vec<String>,
    /** What's clickable this frame: (node id, tooltip, action), for `ui.click` and inspect.json. */
    pub(crate) clickables: Vec<(String, String, Value)>,
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
    pub(crate) diff_scroll: UniformListScrollHandle,
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
        let retry = Duration::from_secs(settings.connection.retry_seconds);
        let mut conns = vec![("box".to_string(), settings.connection.ssh_host.clone(), settings.connection.daemon.clone())];
        if settings.local.enabled && !settings.local.daemon.is_empty() {
            conns.insert(0, ("mac".to_string(), String::new(), settings.local.daemon.clone()));
        }
        let (mut remotes, mut guards, mut listeners) = (HashMap::new(), vec![], vec![]);
        for (host, ssh, daemon) in conns {
            let (r, incoming, guard) = Remote::start(ssh, daemon, retry);
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
        let env_push = cx.spawn(async move |_, cx| loop {
            let home = std::env::var("HOME").unwrap_or_default();
            let _ = cx
                .background_spawn(async move {
                    std::process::Command::new(format!("{home}/dev/agent-box/bin/env-push"))
                        .env("PATH", "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin")
                        .output()
                })
                .await;
            cx.background_executor().timer(Duration::from_secs(60)).await;
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
            completion: None,
            queued: HashMap::new(),
            view_handle: None,
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
            drop_preview: false,
            inspect_at: None,
            inspect_digest: 0,
            activity: vec![],
            clickables: vec![],
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
            diff_scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            _tasks: [watch, env_push, designer_listen].into_iter().chain(listeners).chain(clicks).collect(),
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
            if *rev == self.ctx_rev && at.elapsed() < Duration::from_millis(250) {
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
                let ms = m["ms"].as_u64().unwrap_or(1000).max(16);
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
        store.easing = crate::ui::motion::Easing::parse(m.get("move").and_then(|x| x.get("easing")).and_then(Value::as_str));
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
        if let Some(h) = self.scroll_handles.get("designer") {
            h.scroll_to_bottom();
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
        if let Some(h) = self.scroll_handles.get("designer") {
            h.scroll_to_bottom();
        }
        cx.notify();
    }

    pub(crate) fn designer_reset(&mut self, cx: &mut Context<Self>) {
        self.designer.reset();
        self.transcripts.remove(crate::designer::CHAT_ID);
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
                cx.background_executor().timer(Duration::from_millis(80)).await;
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
                self.settings_error = None;
                self.exprs = Exprs::default();
                self.configure_motion();
                self.load_scripts();
                self.pending_motion = true;
                let ctx = self.context();
                self.update_badge(&ctx);
                crate::logo::clear_cache();
                self.update_dock_icon();
                let keys = self.settings.terminal.keys.clone();
                for t in self.terminals.values() {
                    t.update(cx, |t, _| t.set_keys(&keys));
                }
                self.pending_script.push(json!({"type": "event", "name": "model", "payload": null}));
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
            let event = match (old.as_str(), c.state.as_str()) {
                (o, "waiting") if o != "waiting" => "chat.waiting",
                ("working", "idle") => "chat.finished",
                ("working" | "waiting" | "idle", "stopped") => "chat.stopped",
                _ => continue,
            };
            fired.push((event, c.clone()));
        }
        for (event, chat) in fired {
            self.fire_event(event, vec![("chat".into(), to_val(&self.chat_json(&chat)))], window, cx);
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
        self.state.insert(key.clone(), value.clone());
        self.user_state.insert(key, value);
        self.save_state();
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
                self.status = "Connected".into();
                self.watching.clear();
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
                self.status = format!("Reconnecting to {host}… ({why})");
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
                self.transcripts.entry(chat_id.clone()).or_default().append(from, &entries);
                if self.current_chat().map(|c| c.id == chat_id).unwrap_or(false) {
                    if let Some(h) = self.scroll_handles.get("transcript") {
                        h.scroll_to_bottom();
                    }
                }
            }
            Incoming::Event(Event::TermOutput { term_id, data }) => {
                if let Some(t) = self.term_by_id.get(&(host.to_string(), term_id)).cloned() {
                    t.update(cx, |t, cx| t.output(&data, cx));
                }
                return;
            }
            Incoming::Event(_) => {}
        }
        self.flush(window, cx);
        cx.notify();
    }

    /** Every host's model in one: this Mac's projects and sections first, then the box's. */
    fn merge_models(&mut self) {
        let mut hosts: Vec<&String> = self.models.keys().collect();
        hosts.sort_by_key(|h| if h.as_str() == "mac" { 0 } else { 1 });
        let mut merged = Model { host: "all".into(), ..Default::default() };
        let (mut project_hosts, mut section_hosts) = (vec![], HashMap::new());
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
                merged.projects.push(p);
                project_hosts.push(h.clone());
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
        self.project_hosts = project_hosts;
        self.section_hosts = section_hosts;
    }

    /** Asks every connected host for its past chats; `.history` holds them newest first. */
    pub(crate) fn load_history(&mut self, cx: &mut Context<Self>) {
        self.history_loading = true;
        let remotes: Vec<(String, Remote)> = self.remotes.iter().filter(|(h, _)| self.host_up.get(*h) == Some(&true)).map(|(h, r)| (h.clone(), r.clone())).collect();
        cx.spawn(async move |this, cx| {
            let mut all: Vec<Value> = vec![];
            for (host, r) in remotes {
                if let Ok(v) = r.call(Call::History { limit: Some(300) }).await {
                    for mut c in v["chats"].as_array().cloned().unwrap_or_default() {
                        if let Value::Object(o) = &mut c {
                            o.entry("host").or_insert(Value::String(host.clone()));
                            let age = o.get("last_activity").and_then(Value::as_f64);
                            o.insert("age_text".into(), Value::String(age_text(age)));
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
            Call::SectionAssign { path, .. } | Call::WorktreeDelete { path } | Call::WorktreeMove { path, .. } | Call::WorktreeDiff { path, .. } => {
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
        self.model.chats.iter().find(|c| c.id == id)
    }

    /** A worktree's chats in tab order (`state.tab_order[path]`, set by dragging tabs), the rest after. */
    fn chats_of(&self, w: &Worktree) -> Vec<&Chat> {
        let mut chats: Vec<&Chat> = w.chat_ids.iter().filter_map(|id| self.chat(id)).collect();
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
        !self.closed.contains(&c.id) && (self.opened.contains(&c.id) || c.alive || c.state == "working" || c.state == "waiting")
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
        let projects: Vec<Value> = self
            .model
            .projects
            .iter()
            .enumerate()
            .map(|(i, p)| {
                json!({"name": p.name, "repo": p.repo, "hue": p.hue, "letter": letter(&p.name),
                       "host": self.project_hosts.get(i).cloned().unwrap_or_else(|| "box".into()),
                       "items": p.items.iter().map(|w| self.worktree_json(w, Value::Null)).collect::<Vec<_>>()})
            })
            .collect();
        let sections: Vec<Value> = self
            .model
            .sections
            .iter()
            .map(|s| {
                json!({"id": s.id, "name": s.name, "collapsed": s.collapsed,
                       "host": self.section_hosts.get(&s.id).cloned().unwrap_or_else(|| "box".into()),
                       "items": s.items.iter().map(|w| self.worktree_json(w, json!({"id": s.id, "name": s.name}))).collect::<Vec<_>>()})
            })
            .collect();
        let selected_w = self.current_worktree().map(|w| self.worktree_json(w, Value::Null));
        let selected_diff = self.selected.as_deref().map(|p| self.diff_summary(p)).unwrap_or(Value::Null);
        let selected_c = self.current_chat().map(|c| self.chat_json(c));
        let chats_here: Vec<Value> = self.current_worktree().map(|w| self.chats_of(w).into_iter().map(|c| self.chat_json(c)).collect()).unwrap_or_default();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.);
        to_val(&json!({
            "projects": projects,
            "sections": sections,
            "worktrees": worktrees,
            "chats": self.model.chats.iter().map(|c| self.chat_json(c)).collect::<Vec<_>>(),
            "selected": {"worktree": selected_w, "chat": selected_c, "chats": chats_here, "diff": selected_diff},
            "state": self.state,
            "theme": self.settings.theme,
            "hosts": self.settings.hosts,
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
                "dragging": self.dragging,
                "drop": self.drop_hint.as_ref().map(|h| json!({"key": h.key, "zone": h.zone, "kind": h.kind})),
                "focused": self.focused,
                "status": self.status,
                "ssh_host": self.model.ssh_host,
                "settings_error": self.settings_error,
                "errors": self.exprs.errors.borrow().iter().chain(self.settings.warnings.iter()).cloned().collect::<Vec<_>>(),
            },
            "now": now,
            "data": self.script_data,
            "designer": {
                "working": self.designer.working,
                "attachments": self.attachments_json(crate::designer::CHAT_ID),
                "activity": if self.designer.working { self.transcripts.get(crate::designer::CHAT_ID).map(|t| t.activity()).unwrap_or_default() } else { String::new() },
                "has_messages": self.transcripts.get(crate::designer::CHAT_ID).map(|t| !t.items.is_empty()).unwrap_or(false),
            },
        }))
    }

    fn worktree_json(&self, w: &Worktree, section: Value) -> Value {
        let chats = self.chats_of(w);
        let status = if chats.iter().any(|c| c.state == "working") {
            "working"
        } else if chats.iter().any(|c| c.state == "waiting") {
            "waiting"
        } else if w.mr.as_ref().map(|m| m.state == "merged").unwrap_or(false) {
            "merged"
        } else if w.mr.as_ref().and_then(|m| m.checks.as_deref()) == Some("failing") {
            "failing"
        } else if w.mr.as_ref().and_then(|m| m.checks.as_deref()) == Some("pending") {
            "pending"
        } else if w.uncommitted > 0 || w.unpushed > 0 {
            "dirty"
        } else if w.behind {
            "behind"
        } else {
            "idle"
        };
        let (emoji, label) = split_emoji(&w.title);
        json!({
            "id": w.id, "path": w.path, "branch": w.branch, "title": w.title, "emoji": emoji, "label": label,
            "project": w.project, "repo": w.repo, "hue": w.hue, "letter": letter(&w.project), "is_primary": w.is_primary,
            "uncommitted": w.uncommitted, "unpushed": w.unpushed, "behind": w.behind, "mr": w.mr,
            "age": w.age, "age_text": age_text(w.age), "status": status, "section": section,
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
            o.insert("age_text".into(), Value::String(age_text(c.last_activity)));
            o.insert("attachments".into(), self.attachments_json(&c.id));
            let last = self.transcripts.get(&c.id).and_then(|t| t.items.iter().rev().find_map(|i| if let Item::Assistant { text } = i { Some(text.clone()) } else { None }));
            o.insert("asks".into(), Value::Bool(last.as_deref().is_some_and(|t| t.trim_end().ends_with('?'))));
            o.insert("last_message".into(), last.map(|t| Value::String(t.chars().take(280).collect())).unwrap_or(Value::Null));
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
                    if let Ok(v) = remote.call(Call::TranscriptRead { chat_id: id.clone(), from, tail }).await {
                        let entries = v["entries"].as_array().cloned().unwrap_or_default();
                        let start = v["from"].as_u64().unwrap_or(from as u64) as usize;
                        this.update(cx, |ws, cx| {
                            ws.transcripts.entry(id).or_default().append(start, &entries);
                            if let Some(h) = ws.scroll_handles.get("transcript") {
                                h.scroll_to_bottom();
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
        let state = cx.new(|cx| TextareaState::new(window, cx).auto_grow(1, 10).placeholder(placeholder).submit_on_enter(enter_sends));
        let id = chat_id.to_string();
        self._subscriptions.push(cx.subscribe_in(&state, window, move |ws, _, ev: &InputEvent, window, cx| {
            match ev {
                InputEvent::PressEnter { secondary, shift } => {
                    if ws.completion.as_ref().is_some_and(|c| c.0 == id) && !*shift {
                        ws.accept_completion(None, window, cx);
                    } else if *secondary || (enter_sends && !*shift) {
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
            let all = crate::completions::entries(&self.settings, w.as_ref().map(|w| w.path.as_str()), local.as_deref());
            let items = crate::completions::filter(&all, &query, 12);
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
        self.completion = None;
        self.ctx_rev += 1;
        cx.notify();
    }

    pub(crate) fn move_completion(&mut self, by: i64, cx: &mut Context<Self>) {
        if let Some(c) = &mut self.completion {
            let n = c.2.len() as i64;
            c.3 = ((c.3 as i64 + by).rem_euclid(n.max(1))) as usize;
            self.ctx_rev += 1;
            cx.notify();
        }
    }

    /** Sends the next queued message to each chat that's idle again. */
    pub(crate) fn flush_queue(&mut self) {
        let ready: Vec<String> = self
            .queued
            .iter()
            .filter(|(_, q)| !q.is_empty())
            .filter(|(id, _)| self.chat(id).is_some_and(|c| c.alive && c.state == "idle"))
            .filter(|(id, _)| self.queue_sent.get(*id).map_or(true, |t| t.elapsed() > Duration::from_secs(8)))
            .map(|(id, _)| id.clone())
            .collect();
        for id in ready {
            let Some(text) = self.queued.get_mut(&id).filter(|q| !q.is_empty()).map(|q| q.remove(0)) else { continue };
            self.queue_sent.insert(id.clone(), std::time::Instant::now());
            let call = Call::ChatSend { chat_id: id.clone(), text };
            self.route(&call).send(call);
            self.ctx_rev += 1;
        }
    }

    pub(crate) fn send_composer(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.composers.get(id).cloned() else { return };
        let typed = state.read(cx).value().trim().to_string();
        let Some(paths) = self.take_attachments(id) else {
            use gpui_kit::component::WindowExt as _;
            self.send_when_ready.insert(id.to_string());
            window.push_notification("Sending when the upload finishes", cx);
            return;
        };
        let text = [typed, paths.join(" ")].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
        if text.is_empty() {
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
            let call = Call::ChatSend { chat_id: id.to_string(), text };
            self.route(&call).send(call);
        }
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
        self.status = "Starting a chat…".into();
        cx.spawn(async move |this, cx| {
            let res = remote.call(call).await;
            this.update(cx, |ws, cx| {
                match res {
                    Ok(v) => {
                        if let Some(id) = v["stdout"].as_str().and_then(|o| o.split_whitespace().next()) {
                            ws.pending_select = Some(id.to_string());
                        }
                        ws.status = "Connected".into();
                    }
                    Err(e) => {
                        ws.status = format!("Couldn't start the chat: {e}");
                        ws.starting = None;
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn name_dialog(
        &mut self,
        title: &str,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
        done: impl Fn(&mut Workspace, String, &mut Context<Workspace>) + 'static,
    ) {
        use gpui_kit::component::input::{Input, InputState};
        use gpui_kit::component::WindowExt as _;
        let input = cx.new(|cx| InputState::new(window, cx).default_value(value.to_string()));
        let view = cx.entity();
        let title: SharedString = title.to_string().into();
        let done = std::rc::Rc::new(done);
        let submit = {
            let (input, view, done) = (input.clone(), view.clone(), done.clone());
            std::rc::Rc::new(move |window: &mut Window, cx: &mut App| {
                let v = input.read(cx).value().trim().to_string();
                if v.is_empty() {
                    return;
                }
                let done = done.clone();
                view.update(cx, |ws, cx| done(ws, v, cx));
                window.close_dialog(cx);
            })
        };
        let s2 = submit.clone();
        self._subscriptions.push(cx.subscribe_in(&input, window, move |_, _, ev: &InputEvent, window, cx| {
            if matches!(ev, InputEvent::PressEnter { .. }) {
                s2(window, cx);
            }
        }));
        let focus = input.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            let (input, submit) = (input.clone(), submit.clone());
            dialog.title(title.clone()).child(Input::new(&input)).footer(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(gpui_kit::component::button::Button::new("ok").label("Save").on_click(move |_, window, cx| submit(window, cx))),
            )
        });
        focus.update(cx, |s, cx| s.focus(window, cx));
    }

    pub(crate) fn new_section(&mut self, assign: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.name_dialog("New section", "", window, cx, move |ws, name, cx| {
            let remote = assign.as_deref().map(|p| ws.remote_for_host(&ws.host_of_path(p))).unwrap_or_else(|| ws.remote.clone());
            let assign = assign.clone();
            cx.spawn(async move |this, cx| {
                if remote.call(Call::SectionCreate { name: name.clone() }).await.is_ok() {
                    if let Some(path) = assign {
                        for _ in 0..10 {
                            let id = this.update(cx, |ws, _| ws.model.sections.iter().find(|s| s.name == name).map(|s| s.id.clone())).ok().flatten();
                            if let Some(id) = id {
                                let _ = remote.call(Call::SectionAssign { path, section: Some(id) }).await;
                                break;
                            }
                            cx.background_executor().timer(Duration::from_millis(300)).await;
                        }
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
        use gpui_kit::component::WindowExt as _;
        let home = std::env::var("HOME").unwrap_or_default();
        cmd.env("PATH", format!("{home}/.local/bin:{home}/.cargo/bin:{home}/dev/redo-monorepo-worktrees/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"));
        if notify {
            window.push_notification("Working…", cx);
        }
        cx.spawn_in(window, async move |_, cx| {
            let out = cx.background_spawn(async move { cmd.output() }).await;
            if !notify {
                return;
            }
            let msg = match out {
                Ok(o) => {
                    let text = String::from_utf8_lossy(if o.status.success() { &o.stdout } else { &o.stderr }).trim().to_string();
                    text.lines().last().unwrap_or(if o.status.success() { "Done" } else { "Failed" }).to_string()
                }
                Err(e) => e.to_string(),
            };
            cx.update(|window, cx| window.push_notification(msg, cx)).ok();
        })
        .detach();
    }

    pub(crate) fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::command::{Command, CommandItem, CommandState};
        use gpui_kit::component::WindowExt as _;
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
        for w in self.all_worktrees() {
            entries.push((format!("Go to {}", w.title), json!({"do": "select", "worktree": w.path})));
        }
        let state = cx.new(|cx| CommandState::new(window, cx));
        let view = cx.entity();
        let entries = std::rc::Rc::new(entries);
        window.open_dialog(cx, move |dialog, _, _| {
            let (state, view, entries) = (state.clone(), view.clone(), entries.clone());
            dialog.close_button(false).p_0().content(move |content, _, _| {
                let (view, entries) = (view.clone(), entries.clone());
                content.child(
                    Command::new(&state)
                        .bordered(false)
                        .placeholder("Type a command or a worktree…")
                        .items(entries.iter().map(|(label, _)| CommandItem::new().label(label.clone())).collect::<Vec<_>>())
                        .on_confirm(move |index, window, cx| {
                            if let Some((_, action)) = entries.get(index.row) {
                                let action = action.clone();
                                window.close_dialog(cx);
                                view.update(cx, |ws, cx| {
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
                .title("Settings")
                .size(px(760.))
                .child(
                    v_flex()
                        .size_full()
                        .gap_2()
                        .child(div().text_size(px(12.)).text_color(rgb(0x898a8e)).child(format!(
                            "Everything about the app is here. Your file is merged over the built-in defaults; set a key to null to remove a default. Reference for you and agents: {}",
                            docs.display()
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

    pub(crate) fn transcript_view(&mut self, chat: Option<String>, props: &Map<String, Value>, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.settings.theme.clone();
        let cs = self.settings.chat.clone();
        let st = |k: &str| match cs.get(k) {
            Some(Value::Object(o)) => o.clone(),
            _ => Map::new(),
        };
        let designer = chat.as_deref() == Some(crate::designer::CHAT_ID);
        let items = chat.as_ref().and_then(|id| self.transcripts.get(id)).map(|t| t.items.clone()).unwrap_or_default();
        let Some(chat_id) = chat.filter(|id| self.chat(id).is_some() || (designer && !items.is_empty())) else {
            let empty = props.get("empty").map(value_text).unwrap_or_else(|| "No chat here yet.".into());
            let overlay = if designer { self.drop_overlay(crate::designer::CHAT_ID, cx) } else { None };
            return div().relative().size_full().flex().items_center().justify_center().child(style::apply(div(), &st("empty"), &theme).child(empty)).children(overlay).into_any_element();
        };
        let working = if designer { self.designer.working } else { self.chat(&chat_id).map(|c| c.state == "working").unwrap_or(false) };
        let list = style::apply(v_flex(), &st("list"), &theme).children(self.render_items(&chat_id, &items, working, cx));
        let handle_key = if designer { "designer" } else { "transcript" };
        let fresh = !self.scroll_handles.contains_key(handle_key);
        let handle = self.scroll_handles.entry(handle_key.into()).or_default().clone();
        if fresh {
            handle.scroll_to_bottom();
        }
        let overlay = self.drop_overlay(&chat_id, cx);
        let scroll = div()
            .id(SharedString::from(format!("transcript:{chat_id}")))
            .size_full()
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .restrict_scroll_to_axis()
            .track_scroll(&handle)
            .child(list);
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
            .child(scroll)
            .when(shift_holds, |d| d.child(crate::ui::trace_view::shift_scroll_layer(None, 1.)))
            .children(overlay)
            .into_any_element()
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
        let drop_style = self.settings.chat.get("composer_drop").and_then(Value::as_object).cloned().unwrap_or_default();
        let drop_theme = self.settings.theme.clone();
        let chips = if props.get("attachments").and_then(Value::as_bool) == Some(false) { None } else { self.attachments_view(Some(chat.clone()), props, cx) };
        let key_view = cx.entity().downgrade();
        let key_chat = chat.clone();
        Some(
            div()
                .id(SharedString::from(format!("composer:{chat}")))
                .capture_key_down(move |e: &KeyDownEvent, window, cx| {
                    let _ = key_view.update(cx, |ws, cx| {
                        if !ws.completion.as_ref().is_some_and(|c| c.0 == key_chat) || e.keystroke.modifiers.modified() {
                            return;
                        }
                        match e.keystroke.key.as_str() {
                            "up" => ws.move_completion(-1, cx),
                            "down" => ws.move_completion(1, cx),
                            "tab" | "enter" => ws.accept_completion(None, window, cx),
                            "escape" => {
                                ws.completion = None;
                                ws.ctx_rev += 1;
                                cx.notify();
                            }
                            _ => return,
                        }
                        cx.stop_propagation();
                    });
                })
                .flex()
                .flex_col()
                .gap(px(6.))
                .children(chips)
                .child(textarea)
                .rounded(px(6.))
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
        if !self.dragging {
            return None;
        }
        let theme = self.settings.theme.clone();
        let st = self.settings.chat.get("drop_overlay").and_then(Value::as_object).cloned().unwrap_or_default();
        let label = if chat_id == crate::designer::CHAT_ID { "the designer".to_string() } else { self.chat(chat_id).map(|c| format!("“{}”", c.name)).unwrap_or_else(|| "this chat".into()) };
        let accent = style::color(&Value::from("@accent"), &theme).unwrap_or(blue());
        let (view, chat) = (cx.entity().downgrade(), chat_id.to_string());
        Some(
            style::apply(div().id(SharedString::from(format!("drop:{chat_id}"))).absolute().inset(px(8.)).flex().flex_col().items_center().justify_center(), &st, &theme)
                .opacity(0.)
                .drag_over::<ExternalPaths>(|s, _, _, _| s.opacity(1.))
                .child(gpui_kit::component::Icon::empty().path("icons/paperclip.svg").size(px(22.)).text_color(accent))
                .child(format!("Drop to attach to {label}"))
                .on_drop(move |paths: &ExternalPaths, window, cx| {
                    let files = paths.paths().iter().cloned().map(Attachment::Path).collect();
                    let chat = chat.clone();
                    let _ = view.update(cx, |ws, cx| ws.attach(chat, files, window, cx));
                })
                .into_any_element(),
        )
    }

    pub(crate) fn waiting_card(&self, chat: &Chat, cx: &mut Context<Self>) -> AnyElement {
        let theme = &self.settings.theme;
        let cs = &self.settings.chat;
        let st = |k: &str| match cs.get(k) {
            Some(Value::Object(o)) => o.clone(),
            _ => Map::new(),
        };
        let w = chat.waiting_on.clone().unwrap_or_default();
        let message = w["message"].as_str().or(w["tool_name"].as_str()).unwrap_or("The agent is waiting for you").to_string();
        let options: Vec<String> = w["tool_input"]["questions"][0]["options"]
            .as_array()
            .map(|o| o.iter().filter_map(|x| x["label"].as_str().map(str::to_string)).collect())
            .unwrap_or_else(|| vec!["Yes".into(), "Yes, don't ask again".into(), "No".into()]);
        let mut row = h_flex().gap(px(6.)).flex_wrap();
        for (i, label) in options.iter().enumerate().chain(std::iter::once((usize::MAX, &"Cancel".to_string()))) {
            let id = chat.id.clone();
            let key = if i == usize::MAX { "Escape".to_string() } else { (i + 1).to_string() };
            row = row.child(
                style::apply(div().id(SharedString::from(format!("opt:{}:{i}", chat.id))).cursor_pointer(), &st("waiting_option"), theme)
                    .child(label.clone())
                    .on_click(cx.listener(move |ws, _, _, cx| ws.call(Call::ChatKeys { chat_id: id.clone(), keys: vec![key.clone()] }, cx))),
            );
        }
        style::apply(v_flex().gap(px(10.)), &st("waiting"), theme).child(div().child(message)).child(row).into_any_element()
    }

    fn handle_keys(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let pressed = e.keystroke.unparse();
        let found = self.settings.keys.iter().rev().find(|k| k.get("key").and_then(Value::as_str) == Some(pressed.as_str())).cloned();
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
        self.register_terminals(cx);
        self.flush(window, cx);
        self.render_generation += 1;
        self.clickables.clear();
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
        self.dragging = cx.has_active_drag();
        if self.dragging {
            self.drop_preview = false;
        } else if !self.drop_preview {
            self.drop_hint = None;
        }
        self.focused = window.is_window_active();
        let ctx = self.context();
        self.view_handle = Some(cx.entity().downgrade());
        let body = self.render_view("main", &ctx, window, cx);
        self.write_inspect(&ctx, window, cx);
        self.write_problems();
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
            .on_mouse_up(MouseButton::Left, cx.listener(|ws, _, _, _| {
                if ws.drag.take().is_some() {
                    ws.save_state();
                }
            }))
            .child(body)
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
    name.chars().next().unwrap_or('R').to_ascii_uppercase().to_string()
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

fn age_text(ts: Option<f64>) -> String {
    let Some(ts) = ts else { return String::new() };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.);
    let s = now - ts;
    if s < 60. {
        "now".into()
    } else if s < 3600. {
        format!("{}m", (s / 60.) as u64)
    } else if s < 86400. {
        format!("{}h", (s / 3600.) as u64)
    } else if s < 604800. {
        format!("{}d", (s / 86400.) as u64)
    } else {
        format!("{}w", (s / 604800.) as u64)
    }
}

fn load_state() -> Map<String, Value> {
    std::fs::read_to_string(settings::dir().join("ui-state.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
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
