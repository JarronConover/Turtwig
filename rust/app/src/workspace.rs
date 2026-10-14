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
    pub(crate) entered: HashMap<String, (std::time::Instant, u64)>,
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
        let user_state = load_state();
        let mut state = settings.state.clone();
        state.extend(user_state.clone());
        Self {
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
            render_generation: 0,
            scroll_handles: HashMap::new(),
            drag: None,
            pending_actions: vec![],
            pending_select: None,
            transcripts: HashMap::new(),
            watching: HashSet::new(),
            composers: HashMap::new(),
            terminals: HashMap::new(),
            term_by_id: HashMap::new(),
            expanded_tools: HashSet::new(),
            focus: cx.focus_handle(),
            _tasks: vec![listen, watch, env_push],
            _subscriptions: vec![],
        }
    }

    fn watch_settings(window: &mut Window, cx: &mut Context<Self>) -> Task<()> {
        use notify::{RecursiveMode, Watcher};
        let (tx, rx) = flume::unbounded();
        let name = settings::path().file_name().map(|n| n.to_owned());
        let dir = settings::dir();
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res {
                if ev.paths.iter().any(|p| p.file_name().map(|n| n.to_owned()) == name) {
                    let _ = tx.send(());
                }
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(mut watcher) = watcher else { return };
            let _ = std::fs::create_dir_all(&dir);
            if watcher.watch(&dir, RecursiveMode::NonRecursive).is_err() {
                return;
            }
            while rx.recv_async().await.is_ok() {
                cx.background_executor().timer(Duration::from_millis(80)).await;
                while rx.try_recv().is_ok() {}
                if this.update(cx, |ws, cx| ws.reload_settings(cx)).is_err() {
                    break;
                }
            }
            drop(watcher);
        })
    }

    pub(crate) fn reload_settings(&mut self, cx: &mut Context<Self>) {
        match settings::load() {
            Ok(s) => {
                self.state = s.state.clone();
                self.state.extend(self.user_state.clone());
                self.settings = s;
                self.settings_error = None;
                self.exprs = Exprs::default();
            }
            Err(e) => self.settings_error = Some(format!("{e:#}")),
        }
        cx.notify();
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
            if !self.composers.contains_key(&chat.id) {
                let placeholder = self.settings.chat.get("placeholder").and_then(Value::as_str).unwrap_or("Message the agent…").to_string();
                let state = cx.new(|cx| TextareaState::new(window, cx).auto_grow(1, 10).placeholder(placeholder));
                let id = chat.id.clone();
                self._subscriptions.push(cx.subscribe_in(&state, window, move |ws, state, ev: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { secondary, .. } = ev {
                        let enter_sends = ws.settings.chat.get("enter_sends").and_then(Value::as_bool).unwrap_or(true);
                        if *secondary != enter_sends {
                            let text = state.read(cx).value().trim().to_string();
                            if !text.is_empty() {
                                ws.remote.send(Call::ChatSend { chat_id: id.clone(), text });
                                state.update(cx, |s, cx| s.set_value("", window, cx));
                            }
                        }
                    }
                }));
                self.composers.insert(chat.id.clone(), state);
            }
        }
        if !self.terminals.contains_key(&w.path) && self.connected {
            let remote = self.remote.clone();
            let ts = self.settings.terminal.clone();
            let path = w.path.clone();
            let view = cx.new(|cx| TerminalView::new(remote, None, Some(path.clone()), &ts, cx));
            self.terminals.insert(w.path.clone(), view);
        }
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
        let Some(chat) = chat.and_then(|id| self.chat(&id).cloned()) else {
            let empty = props.get("empty").map(value_text).unwrap_or_else(|| "No chat here yet.".into());
            return style::apply(div().size_full().flex().items_center().justify_center(), &st("empty"), &theme).child(empty).into_any_element();
        };
        let items = self.transcripts.get(&chat.id).map(|t| t.items.clone()).unwrap_or_default();
        let show_thinking = cs.get("show_thinking").and_then(Value::as_bool).unwrap_or(false);
        let collapse = cs.get("collapse_tool_calls").and_then(Value::as_bool).unwrap_or(true);
        let mut list = style::apply(v_flex(), &st("list"), &theme);
        for (i, item) in items.iter().enumerate() {
            let key = format!("{}:{i}", chat.id);
            let el: Option<AnyElement> = match item {
                Item::User { text } => Some(style::apply(div(), &st("user"), &theme).child(text.clone()).into_any_element()),
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
        let handle = self.scroll_handles.entry("transcript".into()).or_default().clone();
        div()
            .id(SharedString::from(format!("transcript:{}", chat.id)))
            .size_full()
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .track_scroll(&handle)
            .child(list)
            .into_any_element()
    }

    pub(crate) fn composer_view(&mut self, chat: Option<String>, _: &Map<String, Value>, _: &mut Window, _: &mut Context<Self>) -> Option<AnyElement> {
        let c = self.composers.get(&chat?)?.clone();
        Some(Textarea::new(&c).appearance(false).into_any_element())
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
        self.entered.retain(|_, (_, seen)| *seen + 2 >= generation);
        let ctx = self.context();
        let body = self.render_view("main", &ctx, window, cx);
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
