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
use gpui_kit::component::text::TextView;
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
    _remote_guard: RemoteGuard,
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
    pending_select: Option<String>,
    transcripts: HashMap<String, Transcript>,
    watching: HashSet<String>,
    composers: HashMap<String, Entity<TextareaState>>,
    pub(crate) terminals: HashMap<String, Entity<TerminalView>>,
    term_by_id: HashMap<u64, Entity<TerminalView>>,
    expanded_tools: HashSet<String>,
    images: HashMap<String, Option<std::sync::Arc<Image>>>,
    focus: FocusHandle,
    _tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (settings, settings_error) = match settings::load() {
            Ok(s) => (s, None),
            Err(e) => (Settings::default(), Some(format!("{e:#}"))),
        };
        crate::ui::docs::write_reference();
        let (remote, incoming, guard) = Remote::start(
            settings.connection.ssh_host.clone(),
            settings.connection.daemon.clone(),
            Duration::from_secs(settings.connection.retry_seconds),
        );
        let listen = cx.spawn_in(window, async move |this, cx| {
            while let Ok(msg) = incoming.recv_async().await {
                if this.update_in(cx, |ws, window, cx| ws.on_incoming(msg, window, cx)).is_err() {
                    break;
                }
            }
        });
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
        let mut transcripts: HashMap<String, Transcript> = HashMap::new();
        transcripts.entry(crate::designer::CHAT_ID.into()).or_default().append(0, &crate::designer::past_entries());
        let user_state = load_state();
        let mut state = settings.state.clone();
        state.extend(user_state.clone());
        let ws = Self {
            _remote_guard: guard,
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
            transcripts,
            watching: HashSet::new(),
            composers: HashMap::new(),
            terminals: HashMap::new(),
            term_by_id: HashMap::new(),
            expanded_tools: HashSet::new(),
            images: HashMap::new(),
            focus: cx.focus_handle(),
            _tasks: vec![listen, watch, env_push, designer_listen],
            _subscriptions: vec![],
        };
        ws.configure_motion();
        ws
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
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res {
                for p in &ev.paths {
                    if p.file_name().map(|n| n.to_owned()) == name {
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
            while let Ok(first) = rx.recv_async().await {
                cx.background_executor().timer(Duration::from_millis(80)).await;
                let (mut reload, mut run) = (!first, first);
                while let Ok(r) = rx.try_recv() {
                    reload |= !r;
                    run |= r;
                }
                let ok = this.update(cx, |ws, cx| {
                    if reload {
                        ws.reload_settings(cx);
                    }
                    if run {
                        ws.run_inbox(cx);
                    }
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
        match settings::load() {
            Ok(s) => {
                self.state = s.state.clone();
                self.state.extend(self.user_state.clone());
                self.settings = s;
                self.settings_error = None;
                self.exprs = Exprs::default();
                self.configure_motion();
                self.pending_motion = true;
            }
            Err(e) => self.settings_error = Some(format!("{e:#}")),
        }
        cx.notify();
    }

    /** problems.txt lets an agent that just edited settings see what broke. */
    fn write_problems(&mut self) {
        let mut lines: Vec<String> = self.settings_error.iter().cloned().collect();
        lines.extend(self.exprs.errors.borrow().iter().cloned());
        let text = if lines.is_empty() { "No problems.\n".to_string() } else { lines.join("\n") + "\n" };
        if text != self.problems_written {
            let _ = std::fs::write(settings::dir().join("problems.txt"), &text);
            self.problems_written = text;
        }
    }

    pub(crate) fn set_state(&mut self, key: String, value: Value) {
        self.state.insert(key.clone(), value.clone());
        self.user_state.insert(key, value);
        self.save_state();
    }

    fn save_state(&self) {
        let _ = std::fs::write(settings::dir().join("ui-state.json"), serde_json::to_string_pretty(&self.user_state).unwrap_or_default());
    }

    fn on_incoming(&mut self, msg: Incoming, window: &mut Window, cx: &mut Context<Self>) {
        match msg {
            Incoming::Connected => {
                self.connected = true;
                self.status = "Connected".into();
                self.watching.clear();
                let remote = self.remote.clone();
                cx.spawn(async move |_, _| {
                    let _ = remote.call(Call::Hello { client: "agent-box-app".into() }).await;
                    let _ = remote.call(Call::Subscribe { from_seq: None }).await;
                })
                .detach();
            }
            Incoming::Disconnected(why) => {
                self.connected = false;
                self.status = format!("Reconnecting… ({why})");
                self.terminals.clear();
                self.term_by_id.clear();
            }
            Incoming::Event(Event::Model(m)) => {
                self.model = m;
                self.pending_motion = true;
                if let Some(id) = self.pending_select.clone() {
                    if let Some(path) = self.all_worktrees().iter().find(|w| w.chat_ids.contains(&id)).map(|w| w.path.clone()) {
                        self.active_chat.insert(path.clone(), id);
                        self.selected = Some(path);
                        self.pending_select = None;
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
                if let Some(t) = self.term_by_id.get(&term_id).cloned() {
                    t.update(cx, |t, cx| t.output(&data, cx));
                }
                return;
            }
            Incoming::Event(_) => {}
        }
        cx.notify();
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

    fn chats_of(&self, w: &Worktree) -> Vec<&Chat> {
        w.chat_ids.iter().filter_map(|id| self.chat(id)).collect()
    }

    pub(crate) fn current_worktree(&self) -> Option<&Worktree> {
        self.selected.as_deref().and_then(|p| self.worktree(p))
    }

    pub(crate) fn current_chat(&self) -> Option<&Chat> {
        let w = self.current_worktree()?;
        let chats = self.chats_of(w);
        let active = self.active_chat.get(&w.path);
        active.and_then(|id| chats.iter().find(|c| &c.id == id).copied()).or_else(|| chats.first().copied())
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
            .map(|p| {
                json!({"name": p.name, "repo": p.repo, "hue": p.hue, "letter": letter(&p.name),
                       "items": p.items.iter().map(|w| self.worktree_json(w, Value::Null)).collect::<Vec<_>>()})
            })
            .collect();
        let sections: Vec<Value> = self
            .model
            .sections
            .iter()
            .map(|s| {
                json!({"id": s.id, "name": s.name, "collapsed": s.collapsed,
                       "items": s.items.iter().map(|w| self.worktree_json(w, json!({"id": s.id, "name": s.name}))).collect::<Vec<_>>()})
            })
            .collect();
        let selected_w = self.current_worktree().map(|w| self.worktree_json(w, Value::Null));
        let selected_c = self.current_chat().map(|c| self.chat_json(c));
        let chats_here: Vec<Value> = self.current_worktree().map(|w| self.chats_of(w).into_iter().map(|c| self.chat_json(c)).collect()).unwrap_or_default();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.);
        to_val(&json!({
            "projects": projects,
            "sections": sections,
            "worktrees": worktrees,
            "chats": self.model.chats.iter().map(|c| self.chat_json(c)).collect::<Vec<_>>(),
            "selected": {"worktree": selected_w, "chat": selected_c, "chats": chats_here},
            "state": self.state,
            "theme": self.settings.theme,
            "app": {
                "connected": self.connected,
                "status": self.status,
                "ssh_host": self.model.ssh_host,
                "settings_error": self.settings_error,
                "errors": self.exprs.errors.borrow().clone(),
            },
            "now": now,
            "designer": {
                "working": self.designer.working,
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
            "chats": chats.iter().map(|c| self.chat_json(c)).collect::<Vec<_>>(),
        })
    }

    fn chat_json(&self, c: &Chat) -> Value {
        let mut v = serde_json::to_value(c).unwrap_or(Value::Null);
        if let Value::Object(o) = &mut v {
            o.insert("active".into(), Value::Bool(self.current_chat().map(|x| x.id == c.id).unwrap_or(false)));
            o.insert("age_text".into(), Value::String(age_text(c.last_activity)));
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
                let remote = self.remote.clone();
                let id = chat.id.clone();
                cx.spawn(async move |this, cx| {
                    if let Ok(v) = remote.call(Call::TranscriptRead { chat_id: id.clone(), from }).await {
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
            let remote = self.remote.clone();
            let ts = self.settings.terminal.clone();
            let path = w.path.clone();
            let view = cx.new(|cx| TerminalView::new(remote, None, Some(path.clone()), &ts, cx));
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
        let state = cx.new(|cx| TextareaState::new(window, cx).auto_grow(1, 10).placeholder(placeholder));
        let id = chat_id.to_string();
        self._subscriptions.push(cx.subscribe_in(&state, window, move |ws, state, ev: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { secondary, .. } = ev {
                let enter_sends = ws.settings.chat.get("enter_sends").and_then(Value::as_bool).unwrap_or(true);
                if *secondary != enter_sends {
                    let _ = state;
                    ws.send_composer(&id, window, cx);
                }
            }
        }));
        self.composers.insert(chat_id.to_string(), state);
    }

    pub(crate) fn send_composer(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.composers.get(id).cloned() else { return };
        let text = state.read(cx).value().trim().to_string();
        if text.is_empty() {
            return;
        }
        if id == crate::designer::CHAT_ID {
            self.designer_send(text, cx);
        } else {
            self.remote.send(Call::ChatSend { chat_id: id.to_string(), text });
        }
        state.update(cx, |s, cx| s.set_value("", window, cx));
    }

    fn register_terminals(&mut self, cx: &mut Context<Self>) {
        for t in self.terminals.values() {
            if let Some(id) = t.read(cx).term_id.filter(|id| *id != 0) {
                self.term_by_id.entry(id).or_insert_with(|| t.clone());
            }
        }
    }

    pub(crate) fn call(&self, call: Call, cx: &mut Context<Self>) {
        let remote = self.remote.clone();
        cx.spawn(async move |_, _| {
            let _ = remote.call(call).await;
        })
        .detach();
    }

    pub(crate) fn start_chat(&mut self, call: Call, cx: &mut Context<Self>) {
        let remote = self.remote.clone();
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
                    Err(e) => ws.status = format!("Couldn't start the chat: {e}"),
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
            let remote = ws.remote.clone();
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
            return div().size_full().flex().items_center().justify_center().child(style::apply(div(), &st("empty"), &theme).child(empty)).into_any_element();
        };
        let show_thinking = cs.get("show_thinking").and_then(Value::as_bool).unwrap_or(false);
        let collapse = cs.get("collapse_tool_calls").and_then(Value::as_bool).unwrap_or(true);
        let mut list = style::apply(v_flex(), &st("list"), &theme);
        for (i, item) in items.iter().enumerate() {
            let key = format!("{chat_id}:{i}");
            let el: Option<AnyElement> = match item {
                Item::User { text } => {
                    let thumbs: Vec<AnyElement> = local_uploads(text)
                        .into_iter()
                        .map(|p| style::apply(img(p).object_fit(ObjectFit::Contain), &st("image"), &theme).into_any_element())
                        .collect();
                    Some(style::apply(v_flex(), &st("user"), &theme).child(text.clone()).children(thumbs).into_any_element())
                }
                Item::Image { media_type, data } => {
                    let image = self.images.entry(key.clone()).or_insert_with(|| decode_image(media_type, data)).clone();
                    image.map(|i| style::apply(img(i).object_fit(ObjectFit::Contain), &st("image"), &theme).into_any_element())
                }
                Item::Assistant { text } => {
                    Some(style::apply(div(), &st("assistant"), &theme).child(TextView::markdown(SharedString::from(key), text.clone())).into_any_element())
                }
                Item::Thinking { text } if show_thinking => Some(style::apply(div(), &st("thinking"), &theme).child(text.clone()).into_any_element()),
                Item::Thinking { .. } => None,
                Item::Tool { id, name, summary, input, result, is_error } => {
                    let open = self.expanded_tools.contains(id) || !collapse;
                    let tid = id.clone();
                    let dot = style::color(&Value::from(if result.is_none() { "@warning" } else if *is_error { "@danger" } else { "@success" }), &theme)
                        .unwrap_or(gpui_kit::white());
                    let mut col = v_flex().gap(px(6.)).child(
                        style::apply(h_flex().id(SharedString::from(format!("tool:{id}"))).cursor_pointer(), &st("tool"), &theme)
                            .child(div().size(px(7.)).rounded_full().bg(dot))
                            .child(style::apply(div(), &st("tool_name"), &theme).child(name.clone()))
                            .child(style::apply(div().flex_1().truncate(), &st("tool_summary"), &theme).child(summary.clone()))
                            .on_click(cx.listener(move |ws, _, _, cx| {
                                if !ws.expanded_tools.remove(&tid) {
                                    ws.expanded_tools.insert(tid.clone());
                                }
                                cx.notify();
                            })),
                    );
                    if open {
                        let detail = format!(
                            "{}{}",
                            serde_json::to_string_pretty(input).unwrap_or_default(),
                            result.as_ref().map(|r| format!("\n\n{}", r.chars().take(4000).collect::<String>())).unwrap_or_default()
                        );
                        col = col.child(style::apply(div(), &st("tool_detail"), &theme).child(detail));
                    }
                    Some(col.into_any_element())
                }
                Item::Notice { text } => Some(style::apply(div(), &st("notice"), &theme).child(text.clone()).into_any_element()),
            };
            if let Some(el) = el {
                list = list.child(el);
            }
        }
        let handle_key = if designer { "designer" } else { "transcript" };
        let fresh = !self.scroll_handles.contains_key(handle_key);
        let handle = self.scroll_handles.entry(handle_key.into()).or_default().clone();
        if fresh {
            handle.scroll_to_bottom();
        }
        div()
            .id(SharedString::from(format!("transcript:{chat_id}")))
            .size_full()
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .track_scroll(&handle)
            .child(list)
            .into_any_element()
    }

    pub(crate) fn composer_view(&mut self, chat: Option<String>, _: &Map<String, Value>, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let chat = chat?;
        if chat == crate::designer::CHAT_ID {
            self.ensure_composer(&chat, window, cx);
        }
        let c = self.composers.get(&chat)?.clone();
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
        let theme = self.settings.theme.clone();
        let over = self.settings.chat.get("drop_target").and_then(Value::as_object).cloned().unwrap_or_default();
        Some(
            div()
                .id(SharedString::from(format!("composer-drop:{chat}")))
                .child(textarea)
                .drag_over::<ExternalPaths>(move |s, _, _, _| {
                    let mut s = s;
                    style::refine(&mut s, &over, &theme);
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

    /** Puts pasted or dropped files where the chat's agent can read them (the box, for box chats) and
    inserts their paths into the composer. */
    pub(crate) fn attach(&mut self, chat: String, files: Vec<Attachment>, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::WindowExt as _;
        let local = chat == crate::designer::CHAT_ID || self.chat(&chat).map(|c| c.host == "mac").unwrap_or(false);
        let host = self.settings.connection.ssh_host.clone();
        let Some(composer) = self.composers.get(&chat).cloned() else { return };
        if !local {
            window.push_notification("Uploading to the box…", cx);
        }
        cx.spawn_in(window, async move |_, cx| {
            let result = cx.background_spawn(async move { stage_attachments(files, local.then_some(()).map_or(Some(host), |_| None)) }).await;
            cx.update(|window, cx| match result {
                Ok(paths) => composer.update(cx, |s, cx| s.insert(format!("{} ", paths.join(" ")), window, cx)),
                Err(e) => window.push_notification(format!("Couldn't attach: {e}"), cx),
            })
            .ok();
        })
        .detach();
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
        for a in std::mem::take(&mut self.pending_actions) {
            self.run_action(&a, window, cx);
        }
        self.render_generation += 1;
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
        let ctx = self.context();
        let body = self.render_view("main", &ctx, window, cx);
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

/** Copies each file into ~/.agent-box/uploads (and, given a host, the same folder on it); returns
the paths the agent should read. */
fn stage_attachments(files: Vec<Attachment>, host: Option<String>) -> Result<Vec<String>, String> {
    let dir = uploads_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let mut out = vec![];
    for (i, f) in files.into_iter().enumerate() {
        let (name, local) = match f {
            Attachment::Bytes { bytes, ext } => {
                let name = format!("{stamp}-{i}.{ext}");
                let p = dir.join(&name);
                std::fs::write(&p, bytes).map_err(|e| e.to_string())?;
                (name, p)
            }
            Attachment::Path(p) => {
                let base = p.file_name().map(|n| n.to_string_lossy().replace(' ', "-")).unwrap_or_else(|| "file".into());
                let name = format!("{stamp}-{base}");
                let copy = dir.join(&name);
                std::fs::copy(&p, &copy).map_err(|e| format!("{}: {e}", p.display()))?;
                (name, copy)
            }
        };
        match &host {
            None => out.push(local.to_string_lossy().into_owned()),
            Some(host) => {
                let home = std::env::var("HOME").unwrap_or_default();
                let file = std::fs::File::open(&local).map_err(|e| e.to_string())?;
                let o = std::process::Command::new("ssh")
                    .env("PATH", format!("{home}/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"))
                    .args([host.as_str(), &format!("mkdir -p ~/.agent-box/uploads && cat > ~/.agent-box/uploads/{name} && printf %s \"$HOME\"")])
                    .stdin(file)
                    .output()
                    .map_err(|e| e.to_string())?;
                if !o.status.success() {
                    return Err(String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("upload failed").to_string());
                }
                let remote_home = String::from_utf8_lossy(&o.stdout).trim().to_string();
                out.push(format!("{remote_home}/.agent-box/uploads/{name}"));
            }
        }
    }
    Ok(out)
}

/** Images this app uploaded that a message mentions, as their local copies (for thumbnails). */
fn local_uploads(text: &str) -> Vec<std::path::PathBuf> {
    let dir = uploads_dir();
    text.split_whitespace()
        .filter_map(|w| w.split("/.agent-box/uploads/").nth(1))
        .map(|name| dir.join(name))
        .filter(|p| p.exists() && matches!(p.extension().and_then(|e| e.to_str()), Some("png" | "jpg" | "jpeg" | "gif" | "webp")))
        .collect()
}

fn decode_image(media_type: &str, data: &str) -> Option<std::sync::Arc<Image>> {
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
