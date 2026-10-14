/*! The window: interprets the layout tree from settings and draws each widget from the daemon's model.
All state the widgets need (selection, transcripts, composers, terminals, sizes) lives here. */

use crate::remote::{Incoming, Remote};
use crate::settings::{self, parse_color, Node, Settings, Style};
use crate::terminal::TerminalView;
use crate::transcript::{Item, Transcript};
use agent_box_protocol::{Call, Chat, Event, Model, Worktree};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{h_flex, v_flex, Icon, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

actions!(agent_box, [Palette, ToggleSidebar, OpenSettings, NewChat, ToggleTerminal, Reconnect, NextWorktree, PreviousWorktree, ReloadSettings]);

pub fn bind_keys(settings: &Settings, cx: &mut App) {
    cx.clear_key_bindings();
    let mut bindings = Vec::new();
    for k in &settings.keys {
        let b = match k.action.as_str() {
            "palette" => KeyBinding::new(&k.key, Palette, None),
            "toggle_sidebar" => KeyBinding::new(&k.key, ToggleSidebar, None),
            "open_settings" => KeyBinding::new(&k.key, OpenSettings, None),
            "new_chat" => KeyBinding::new(&k.key, NewChat, None),
            "toggle_terminal" => KeyBinding::new(&k.key, ToggleTerminal, None),
            "reconnect" => KeyBinding::new(&k.key, Reconnect, None),
            "next_worktree" => KeyBinding::new(&k.key, NextWorktree, None),
            "previous_worktree" => KeyBinding::new(&k.key, PreviousWorktree, None),
            "reload_settings" => KeyBinding::new(&k.key, ReloadSettings, None),
            _ => continue,
        };
        bindings.push(b);
    }
    cx.bind_keys(bindings);
}

struct Resize {
    id: String,
    vertical: bool,
    start: f32,
    start_size: f32,
}

pub struct Workspace {
    settings: Settings,
    settings_error: Option<String>,
    remote: Remote,
    connected: bool,
    status: SharedString,
    model: Model,
    selected: Option<String>,
    active_chat: HashMap<String, String>,
    transcripts: HashMap<String, Transcript>,
    watching: HashSet<String>,
    composers: HashMap<String, Entity<TextareaState>>,
    terminals: HashMap<String, Entity<TerminalView>>,
    term_by_id: HashMap<u64, Entity<TerminalView>>,
    expanded_tools: HashSet<String>,
    collapsed: HashSet<String>,
    sidebar_visible: bool,
    terminal_visible: bool,
    sizes: HashMap<String, f32>,
    resizing: Option<Resize>,
    chat_scroll: ScrollHandle,
    sidebar_scroll: ScrollHandle,
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
        let _ = settings::write_schema();
        bind_keys(&settings, cx);
        let (remote, incoming) = Remote::start(
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
        // Keeps the box's copy of skills, memory, and Cursor config current while the app is open.
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
        let sizes = load_sizes();
        Self {
            settings,
            settings_error,
            remote,
            connected: false,
            status: "Connecting to the box…".into(),
            model: Model::default(),
            selected: None,
            active_chat: HashMap::new(),
            transcripts: HashMap::new(),
            watching: HashSet::new(),
            composers: HashMap::new(),
            terminals: HashMap::new(),
            term_by_id: HashMap::new(),
            expanded_tools: HashSet::new(),
            collapsed: HashSet::new(),
            sidebar_visible: true,
            terminal_visible: true,
            sizes,
            resizing: None,
            chat_scroll: ScrollHandle::new(),
            sidebar_scroll: ScrollHandle::new(),
            focus: cx.focus_handle(),
            _tasks: vec![listen, watch, env_push],
            _subscriptions: vec![],
        }
    }

    /** Re-reads the settings file whenever it changes on disk, so edits apply live. */
    fn watch_settings(window: &mut Window, cx: &mut Context<Self>) -> Task<()> {
        use notify::{RecursiveMode, Watcher};
        let (tx, rx) = flume::unbounded();
        let path = settings::path();
        let dir = path.parent().unwrap().to_path_buf();
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res {
                if ev.paths.iter().any(|p| p.file_name() == path.file_name()) {
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
                while rx.try_recv().is_ok() {}
                if this.update(cx, |ws, cx| ws.reload_settings(cx)).is_err() {
                    break;
                }
            }
            drop(watcher);
        })
    }

    fn reload_settings(&mut self, cx: &mut Context<Self>) {
        match settings::load() {
            Ok(s) => {
                bind_keys(&s, cx);
                self.settings = s;
                self.settings_error = None;
            }
            Err(e) => self.settings_error = Some(format!("{e:#}")),
        }
        cx.notify();
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
                self.status = format!("Reconnecting… ({why})").into();
                self.terminals.clear();
                self.term_by_id.clear();
            }
            Incoming::Event(Event::Model(m)) => {
                self.model = m;
                if self.selected.as_ref().map_or(true, |p| self.worktree(p).is_none()) {
                    self.selected = self.all_worktrees().first().map(|w| w.path.clone());
                }
                self.sync_selection(window, cx);
            }
            Incoming::Event(Event::Transcript { chat_id, from, entries }) => {
                let t = self.transcripts.entry(chat_id.clone()).or_default();
                t.append(from, &entries);
                if self.current_chat().map(|c| c.id == chat_id).unwrap_or(false) {
                    self.chat_scroll.scroll_to_bottom();
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

    fn all_worktrees(&self) -> Vec<&Worktree> {
        self.model.projects.iter().flat_map(|p| p.items.iter()).chain(self.model.sections.iter().flat_map(|s| s.items.iter())).collect()
    }

    fn worktree(&self, path: &str) -> Option<&Worktree> {
        self.all_worktrees().into_iter().find(|w| w.path == path)
    }

    fn chat(&self, id: &str) -> Option<&Chat> {
        self.model.chats.iter().find(|c| c.id == id)
    }

    fn chats_of(&self, w: &Worktree) -> Vec<&Chat> {
        w.chat_ids.iter().filter_map(|id| self.chat(id)).collect()
    }

    fn current_worktree(&self) -> Option<&Worktree> {
        self.selected.as_deref().and_then(|p| self.worktree(p))
    }

    fn current_chat(&self) -> Option<&Chat> {
        let w = self.current_worktree()?;
        let chats = self.chats_of(w);
        let active = self.active_chat.get(&w.path);
        active.and_then(|id| chats.iter().find(|c| &c.id == id).copied()).or_else(|| chats.first().copied())
    }

    fn select(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = Some(path);
        self.sync_selection(window, cx);
        cx.notify();
    }

    /** Makes sure the selected worktree's chat is being watched and has a composer and a terminal. */
    fn sync_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
                            ws.chat_scroll.scroll_to_bottom();
                            cx.notify();
                        })
                        .ok();
                    }
                })
                .detach();
            }
            if !self.composers.contains_key(&chat.id) {
                let state = cx.new(|cx| TextareaState::new(window, cx).auto_grow(1, 10).placeholder("Message the agent…"));
                let id = chat.id.clone();
                self._subscriptions.push(cx.subscribe_in(&state, window, move |ws, state, ev: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { secondary, .. } = ev {
                        let enter_sends = ws.settings.chat.enter_sends;
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
            let settings = self.settings.clone();
            let path = w.path.clone();
            let view = cx.new(|cx| TerminalView::new(remote, None, Some(path.clone()), &settings, cx));
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

    fn call(&self, call: Call, cx: &mut Context<Self>) {
        let remote = self.remote.clone();
        cx.spawn(async move |_, _| {
            let _ = remote.call(call).await;
        })
        .detach();
    }

    // ---------- actions ----------

    fn toggle_sidebar(&mut self, _: &ToggleSidebar, _: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_visible = !self.sidebar_visible;
        cx.notify();
    }

    fn toggle_terminal(&mut self, _: &ToggleTerminal, _: &mut Window, cx: &mut Context<Self>) {
        self.terminal_visible = !self.terminal_visible;
        cx.notify();
    }

    fn open_settings(&mut self, _: &OpenSettings, _: &mut Window, _: &mut Context<Self>) {
        let _ = settings::write_schema();
        let _ = std::process::Command::new("open").arg("-t").arg(settings::path()).spawn();
    }

    fn reload(&mut self, _: &ReloadSettings, _: &mut Window, cx: &mut Context<Self>) {
        self.reload_settings(cx);
    }

    fn step_worktree(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let paths: Vec<String> = self.all_worktrees().iter().map(|w| w.path.clone()).collect();
        if paths.is_empty() {
            return;
        }
        let i = self.selected.as_ref().and_then(|s| paths.iter().position(|p| p == s)).unwrap_or(0) as isize;
        let next = (i + delta).rem_euclid(paths.len() as isize) as usize;
        self.select(paths[next].clone(), window, cx);
    }

    fn new_chat_dialog(&mut self, dir: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::input::{Input, InputState};
        use gpui_kit::component::WindowExt as _;
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("What this chat is for"));
        let branch = cx.new(|cx| InputState::new(window, cx).placeholder("Branch for a new worktree (optional)"));
        let prompt = cx.new(|cx| TextareaState::new(window, cx).auto_grow(2, 6).placeholder("First message, e.g. /ship-feature AIOPSUP-…"));
        let remote = self.remote.clone();
        let repo = dir.as_ref().and_then(|d| self.worktree(d)).map(|w| w.repo.clone());
        window.open_dialog(cx, move |dialog, _, _| {
            let (name, branch, prompt, remote, dir, repo) =
                (name.clone(), branch.clone(), prompt.clone(), remote.clone(), dir.clone(), repo.clone());
            dialog
                .title("New chat on the box")
                .child(
                    v_flex()
                        .gap_2()
                        .child(Input::new(&name))
                        .child(Input::new(&branch))
                        .child(Textarea::new(&prompt)),
                )
                .footer(
                    h_flex().gap_2().justify_end().child(
                        gpui_kit::component::button::Button::new("start")
                            .label("Start")
                            .on_click(move |_, window, cx| {
                                let n = name.read(cx).value().trim().to_string();
                                if n.is_empty() {
                                    return;
                                }
                                let b = branch.read(cx).value().trim().to_string();
                                let p = prompt.read(cx).value().trim().to_string();
                                let call = Call::ChatNew {
                                    name: n,
                                    kind: if b.is_empty() { "primary".into() } else { "feature".into() },
                                    branch: (!b.is_empty()).then_some(b.clone()),
                                    base: None,
                                    dir: if b.is_empty() { dir.clone() } else { None },
                                    repo: repo.clone(),
                                    prompt: (!p.is_empty()).then_some(p),
                                    model: None,
                                };
                                let r = remote.clone();
                                cx.spawn(async move |_| {
                                    let _ = r.call(call).await;
                                })
                                .detach();
                                window.close_dialog(cx);
                            }),
                    ),
                )
        });
    }

    fn name_dialog(&mut self, title: &str, value: &str, window: &mut Window, cx: &mut Context<Self>, done: impl Fn(&mut Workspace, String, &mut Context<Workspace>) + 'static) {
        use gpui_kit::component::input::{Input, InputState};
        use gpui_kit::component::WindowExt as _;
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Name").default_value(value.to_string()));
        let view = cx.entity();
        let title: SharedString = title.to_string().into();
        let done = std::rc::Rc::new(done);
        window.open_dialog(cx, move |dialog, _, _| {
            let (input, view, done) = (input.clone(), view.clone(), done.clone());
            dialog.title(title.clone()).child(Input::new(&input)).footer(
                h_flex().gap_2().justify_end().child(
                    gpui_kit::component::button::Button::new("ok").label("Save").on_click(move |_, window, cx| {
                        let v = input.read(cx).value().trim().to_string();
                        if v.is_empty() {
                            return;
                        }
                        let done = done.clone();
                        view.update(cx, |ws, cx| done(ws, v, cx));
                        window.close_dialog(cx);
                    }),
                ),
            )
        });
    }

    fn new_section(&mut self, assign: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.name_dialog("New section", "", window, cx, move |ws, name, cx| {
            let remote = ws.remote.clone();
            let assign = assign.clone();
            cx.spawn(async move |this, cx| {
                if remote.call(Call::SectionCreate { name: name.clone() }).await.is_ok() {
                    if let Some(path) = assign {
                        let id = this.update(cx, |ws, _| ws.model.sections.iter().find(|s| s.name == name).map(|s| s.id.clone())).ok().flatten();
                        let id = match id {
                            Some(id) => Some(id),
                            None => {
                                cx.background_executor().timer(Duration::from_millis(600)).await;
                                this.update(cx, |ws, _| ws.model.sections.iter().find(|s| s.name == name).map(|s| s.id.clone())).ok().flatten()
                            }
                        };
                        if let Some(id) = id {
                            let _ = remote.call(Call::SectionAssign { path, section: Some(id) }).await;
                        }
                    }
                }
            })
            .detach();
        });
    }

    /** Runs one of agent-box's own scripts on the Mac (moving chats, testing a branch locally). */
    fn run_local(&mut self, args: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::WindowExt as _;
        let home = std::env::var("HOME").unwrap_or_default();
        let tool = format!("{home}/dev/agent-box/bin/{}", args[0]);
        let rest = args[1..].to_vec();
        window.push_notification("Working…", cx);
        cx.spawn_in(window, async move |_, cx| {
            let out = cx
                .background_spawn(async move {
                    std::process::Command::new(tool)
                        .args(rest)
                        .env("PATH", format!("{home}/.local/bin:{home}/dev/redo-monorepo-worktrees/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"))
                        .output()
                })
                .await;
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

    fn palette(&mut self, _: &Palette, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::command::{Command, CommandItem, CommandState};
        use gpui_kit::component::WindowExt as _;
        type Run = std::rc::Rc<dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>)>;
        let mut entries: Vec<(String, Run)> = vec![
            ("New chat".into(), std::rc::Rc::new(|ws: &mut Workspace, w: &mut Window, cx: &mut Context<Workspace>| { let d = ws.selected.clone(); ws.new_chat_dialog(d, w, cx) })),
            ("New section".into(), std::rc::Rc::new(|ws: &mut Workspace, w: &mut Window, cx: &mut Context<Workspace>| ws.new_section(None, w, cx))),
            ("Toggle sidebar".into(), std::rc::Rc::new(|ws: &mut Workspace, w: &mut Window, cx: &mut Context<Workspace>| ws.toggle_sidebar(&ToggleSidebar, w, cx))),
            ("Toggle terminal".into(), std::rc::Rc::new(|ws: &mut Workspace, w: &mut Window, cx: &mut Context<Workspace>| ws.toggle_terminal(&ToggleTerminal, w, cx))),
            ("Open settings".into(), std::rc::Rc::new(|ws: &mut Workspace, w: &mut Window, cx: &mut Context<Workspace>| ws.open_settings(&OpenSettings, w, cx))),
            ("Reload settings".into(), std::rc::Rc::new(|ws: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>| ws.reload_settings(cx))),
        ];
        let (path, branch, mr_url) = self
            .current_worktree()
            .map(|w| (w.path.clone(), w.branch.clone().unwrap_or_default(), w.mr.as_ref().map(|m| m.url.clone()).unwrap_or_default()))
            .unwrap_or_default();
        let host = self.model.ssh_host.clone();
        for a in self.settings.actions.clone() {
            let cmd = a.run.replace("{path}", &path).replace("{branch}", &branch).replace("{ssh_host}", &host).replace("{mr_url}", &mr_url);
            entries.push((a.name.clone(), std::rc::Rc::new(move |_: &mut Workspace, _: &mut Window, _: &mut Context<Workspace>| {
                let _ = std::process::Command::new("/bin/sh").arg("-c").arg(&cmd).spawn();
            })));
        }
        for w in self.all_worktrees().into_iter().cloned().collect::<Vec<_>>() {
            let p = w.path.clone();
            entries.push((format!("Go to {}", w.title), std::rc::Rc::new(move |ws: &mut Workspace, win: &mut Window, cx: &mut Context<Workspace>| ws.select(p.clone(), win, cx))));
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
                        .placeholder("Type a command, a worktree, or an action…")
                        .items(entries.iter().map(|(label, _)| CommandItem::new().label(label.clone())).collect::<Vec<_>>())
                        .on_confirm(move |index, window, cx| {
                            if let Some((_, run)) = entries.get(index.row) {
                                let run = run.clone();
                                window.close_dialog(cx);
                                let v = view.clone();
                                window.defer(cx, move |window, cx| {
                                    v.update(cx, |ws, cx| run(ws, window, cx));
                                });
                            }
                        }),
                )
            })
        });
    }

    // ---------- layout ----------

    fn styled(&self, mut d: Div, s: &Style) -> Div {
        if let Some(id) = &s.id {
            if let Some(v) = self.sizes.get(id) {
                d.style().flex_grow = Some(0.);
                d.style().flex_shrink = Some(0.);
                return self.styled_rest(d.w(px(*v)).h(px(*v)), s, true);
            }
        }
        self.styled_rest(d, s, false)
    }

    fn styled_rest(&self, mut d: Div, s: &Style, sized: bool) -> Div {
        if !sized {
            if let Some(w) = s.width {
                d = d.w(px(w)).flex_shrink_0();
            }
            if let Some(h) = s.height {
                d = d.h(px(h)).flex_shrink_0();
            }
            if let Some(g) = s.grow {
                d.style().flex_grow = Some(g);
                d.style().flex_basis = Some(px(0.).into());
            }
        }
        if let Some(w) = s.min_width {
            d = d.min_w(px(w));
        }
        if let Some(h) = s.min_height {
            d = d.min_h(px(h));
        }
        if let Some([t, r, b, l]) = s.padding {
            d = d.pt(px(t)).pr(px(r)).pb(px(b)).pl(px(l));
        }
        if let Some(g) = s.gap {
            d = d.gap(px(g));
        }
        if let Some(bg) = &s.background {
            d = d.bg(parse_color(bg));
        }
        d
    }

    fn node(&mut self, n: &Node, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = self.settings.theme.clone();
        let el = match n {
            Node::Row { children, style } | Node::Column { children, style } | Node::Card { children, style } => {
                if style.hidden {
                    return None;
                }
                let kids: Vec<AnyElement> = children.iter().filter_map(|c| self.node(c, window, cx)).collect();
                let base = match n {
                    Node::Row { .. } => h_flex().items_stretch().size_full().min_h_0().min_w_0(),
                    Node::Column { .. } => v_flex().h_full().min_h_0().min_w_0(),
                    _ => v_flex()
                        .bg(parse_color(&t.card))
                        .border_1()
                        .border_color(parse_color(&t.line))
                        .rounded(px(t.card_radius))
                        .overflow_hidden(),
                };
                let is_term_card = matches!(n, Node::Card { .. }) && children.iter().any(|c| matches!(c, Node::Terminal { .. }));
                if is_term_card && !self.terminal_visible {
                    return None;
                }
                let mut d = self.styled_card(base, style);
                if matches!(n, Node::Card { .. }) && style.id.is_some() && style.resizable {
                    d = d.h_auto();
                }
                d.children(kids).into_any_element()
            }
            Node::Sidebar { style } => {
                if !self.sidebar_visible || style.hidden {
                    return None;
                }
                let inner = self.sidebar(window, cx);
                let id = style.id.clone().unwrap_or("sidebar".into());
                let width = self.sizes.get(&id).copied().or(style.width).unwrap_or(339.);
                let handle = div()
                    .id("sidebar-resize")
                    .absolute()
                    .top_0()
                    .right(px(-3.))
                    .w(px(6.))
                    .h_full()
                    .cursor_col_resize()
                    .on_mouse_down(MouseButton::Left, cx.listener(move |ws, e: &MouseDownEvent, _, cx| {
                        ws.resizing = Some(Resize { id: "sidebar".into(), vertical: false, start: f32::from(e.position.x), start_size: width });
                        cx.stop_propagation();
                    }));
                div()
                    .relative()
                    .w(px(width))
                    .flex_shrink_0()
                    .h_full()
                    .child(inner)
                    .when(style.resizable, |d| d.child(handle))
                    .into_any_element()
            }
            Node::Toolbar { style } => { let el = self.toolbar(window, cx); self.styled(el, style) }.into_any_element(),
            Node::ChatTabs { style } => { let el = self.chat_tabs(window, cx); self.styled(el, style) }.into_any_element(),
            Node::Chat { style } => { let el = self.chat_view(window, cx); self.styled(el, style) }.into_any_element(),
            Node::BranchBar { style } => { let el = self.branch_bar(window, cx); self.styled(el, style) }.into_any_element(),
            Node::TerminalTabs { style } => {
                if !self.terminal_visible {
                    return None;
                }
                { let el = self.terminal_tabs(cx); self.styled(el, style) }.into_any_element()
            }
            Node::Terminal { style } => {
                if !self.terminal_visible {
                    return None;
                }
                let term = self.selected.as_ref().and_then(|p| self.terminals.get(p)).cloned();
                self.styled(div().size_full().min_h_0(), style)
                    .when_some(term, |d, t| d.child(t))
                    .into_any_element()
            }
            Node::Spacer { style } => self.styled(div().flex_1(), style).into_any_element(),
        };
        Some(el)
    }

    /** Cards: a size from a drag wins over the layout's grow. */
    fn styled_card(&self, d: Div, s: &Style) -> Div {
        if let Some(v) = s.id.as_ref().and_then(|id| self.sizes.get(id)) {
            let mut d = self.styled_rest(d, &Style { grow: None, ..s.clone() }, true);
            d = d.h(px(*v)).flex_shrink_0();
            return d;
        }
        self.styled_rest(d, s, false)
    }

    // ---------- widgets ----------

    fn status_icon(&self, w: &Worktree) -> AnyElement {
        let t = &self.settings.theme;
        let chats = self.chats_of(w);
        if chats.iter().any(|c| c.state == "working") {
            return Spinner::new().xsmall().color(parse_color(&t.age)).into_any_element();
        }
        if chats.iter().any(|c| c.state == "waiting") {
            return Icon::new(IconName::Bell).xsmall().text_color(parse_color(&t.warning)).into_any_element();
        }
        if let Some(mr) = &w.mr {
            if mr.state == "merged" {
                return Icon::new(IconName::CircleCheck).xsmall().text_color(parse_color(&t.merged)).into_any_element();
            }
            if mr.checks.as_deref() == Some("failing") {
                return Icon::new(IconName::CircleX).xsmall().text_color(parse_color(&t.danger)).into_any_element();
            }
            if mr.checks.as_deref() == Some("pending") {
                return Icon::new(IconName::Loader).xsmall().text_color(parse_color(&t.warning)).into_any_element();
            }
        }
        if w.uncommitted > 0 || w.unpushed > 0 {
            return Icon::new(IconName::ArrowUp).xsmall().text_color(parse_color(&t.age)).into_any_element();
        }
        if w.behind {
            return Icon::new(IconName::ArrowDown).xsmall().text_color(parse_color(&t.age)).into_any_element();
        }
        div().into_any_element()
    }

    fn badge(&self, letter: &str, hue: f32, size_px: f32) -> Div {
        div()
            .flex_shrink_0()
            .size(px(size_px))
            .rounded(px(if size_px > 18. { 5. } else { 4. }))
            .flex()
            .items_center()
            .justify_center()
            .bg(hsla(hue / 360., 0.24, 0.26, 1.))
            .text_color(hsla(hue / 360., 0.25, 0.76, 1.))
            .text_size(px(if size_px > 18. { 13. } else { 9.5 }))
            .child(letter.to_string())
    }

    fn row(&self, w: &Worktree, in_section: bool, cx: &mut Context<Self>) -> AnyElement {
        let t = &self.settings.theme;
        let sb = &self.settings.sidebar;
        let selected = self.selected.as_deref() == Some(&w.path);
        let path = w.path.clone();
        let path_for_menu = w.path.clone();
        let (emoji, text) = split_emoji(&w.title);
        let letter = w.project.chars().next().unwrap_or('R').to_ascii_uppercase().to_string();
        let sections: Vec<(String, String)> = self.model.sections.iter().map(|s| (s.id.clone(), s.name.clone())).collect();
        let view = cx.entity();
        let is_primary = w.is_primary;
        let chats: Vec<(String, String)> = self.chats_of(w).iter().map(|c| (c.id.clone(), c.name.clone())).collect();
        h_flex()
            .id(SharedString::from(format!("row:{}", w.path)))
            .h(px(sb.row_height))
            .pl(px(if in_section { 8. } else { 27. }))
            .pr(px(12.))
            .gap(px(8.))
            .rounded(px(t.row_radius))
            .border_1()
            .border_color(if selected { parse_color(&t.selection_border) } else { transparent_black() })
            .when(selected, |d| d.bg(parse_color(&t.selection)))
            .when(!selected, |d| d.hover(|s| s.bg(parse_color(&t.hover))))
            .when(in_section && sb.show_badges, |d| d.child(self.badge(&letter, w.hue, 16.)))
            .when_some(emoji.filter(|_| in_section), |d, e| d.child(div().w(px(16.)).flex_shrink_0().text_size(px(14.)).child(e)))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(6.))
                    .child(div().truncate().text_size(px(sb.row_text_size)).text_color(parse_color(&t.text)).child(if in_section { text } else { w.title.clone() }))
                    .when(w.is_primary, |d| d.child(Icon::new(IconName::StarFill).xsmall().text_color(parse_color(&t.text_muted)))),
            )
            .when(sb.show_status, |d| d.child(self.status_icon(w)))
            .when(sb.show_age, |d| {
                d.child(div().min_w(px(22.)).text_right().text_size(px(12.5)).text_color(parse_color(&t.age)).child(age(w.age)))
            })
            .on_click(cx.listener(move |ws, _, window, cx| ws.select(path.clone(), window, cx)))
            .context_menu(move |menu, window, _| {
                let mut m = menu;
                for (sid, sname) in sections.iter() {
                    let (p, sid) = (path_for_menu.clone(), sid.clone());
                    m = m.item(PopupMenuItem::new(format!("Move to {sname}")).on_click(window.listener_for(&view, move |ws: &mut Workspace, _, _, cx| {
                        ws.call(Call::SectionAssign { path: p.clone(), section: Some(sid.clone()) }, cx)
                    })));
                }
                if !is_primary {
                    let p = path_for_menu.clone();
                    m = m.item(PopupMenuItem::new("Remove from section").on_click(window.listener_for(&view, move |ws: &mut Workspace, _, _, cx| {
                        ws.call(Call::SectionAssign { path: p.clone(), section: None }, cx)
                    })));
                }
                let p = path_for_menu.clone();
                m = m.item(PopupMenuItem::new("New section…").on_click(window.listener_for(&view, move |ws: &mut Workspace, _, window, cx| {
                    ws.new_section(Some(p.clone()), window, cx)
                })));
                m = m.separator();
                let p = path_for_menu.clone();
                m = m.item(PopupMenuItem::new("New chat here").on_click(window.listener_for(&view, move |ws: &mut Workspace, _, window, cx| {
                    ws.new_chat_dialog(Some(p.clone()), window, cx)
                })));
                for (cid, cname) in chats.iter() {
                    let cid = cid.clone();
                    m = m.item(PopupMenuItem::new(format!("Archive chat \u{201c}{cname}\u{201d}")).on_click(window.listener_for(&view, move |ws: &mut Workspace, _, _, cx| {
                        ws.call(Call::ChatArchive { chat_id: cid.clone() }, cx)
                    })));
                }
                if !is_primary {
                    let p = path_for_menu.clone();
                    m = m.separator().item(PopupMenuItem::new("Delete worktree").on_click(window.listener_for(&view, move |ws: &mut Workspace, _, _, cx| {
                        ws.call(Call::WorktreeDelete { path: p.clone() }, cx)
                    })));
                }
                m
            })
            .into_any_element()
    }

    fn sidebar(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = self.settings.theme.clone();
        let sb = self.settings.sidebar.clone();
        let mut list = v_flex().px(px(6.)).pb(px(10.));
        list = list.child(
            div().h(px(30.)).pt(px(6.)).px(px(8.)).text_size(px(sb.title_size)).font_weight(FontWeight::SEMIBOLD).text_color(parse_color(&t.text)).child(sb.workspace_title.clone()),
        );
        if sb.show_projects {
            list = list.child(self.label("Projects", None, cx));
            for p in self.model.projects.clone() {
                let primary = p.items.iter().find(|i| i.is_primary).map(|i| i.path.clone());
                list = list.child(
                    h_flex()
                        .id(SharedString::from(format!("proj:{}", p.name)))
                        .h(px(sb.project_row_height))
                        .px(px(8.))
                        .gap(px(8.))
                        .child(self.badge(&p.name.chars().next().unwrap_or('R').to_ascii_uppercase().to_string(), p.hue, 22.))
                        .child(div().flex_1().text_size(px(14.)).text_color(parse_color(&t.text)).child(p.name.clone()))
                        .child(
                            div()
                                .id(SharedString::from(format!("proj-add:{}", p.name)))
                                .size(px(22.))
                                .rounded(px(5.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .hover(|s| s.bg(parse_color(&t.hover)))
                                .child(Icon::new(IconName::Plus).xsmall().text_color(parse_color(&t.text_muted)))
                                .on_click(cx.listener(move |ws, _, window, cx| ws.new_chat_dialog(primary.clone(), window, cx))),
                        ),
                );
                for w in &p.items {
                    list = list.child(self.row(w, false, cx));
                }
            }
        }
        if sb.show_sections {
            for s in self.model.sections.clone() {
                if s.items.is_empty() && !sb.show_empty_sections {
                    continue;
                }
                let key = format!("section:{}", s.id);
                let collapsed = self.collapsed.contains(&key) != s.collapsed;
                list = list.child(self.label(&s.name, Some((key, collapsed)), cx));
                if !collapsed {
                    for w in &s.items {
                        list = list.child(self.row(w, true, cx));
                    }
                }
            }
        }
        v_flex()
            .size_full()
            .bg(parse_color(&t.window))
            .child(
                h_flex()
                    .h(px(44.))
                    .flex_shrink_0()
                    .pl(px(88.))
                    .child(
                        div()
                            .id("toggle-sidebar")
                            .size(px(24.))
                            .rounded(px(5.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|s| s.bg(parse_color(&t.hover)))
                            .child(Icon::new(IconName::PanelLeft).small().text_color(parse_color(&t.text_muted)))
                            .on_click(cx.listener(|ws, _, w, cx| ws.toggle_sidebar(&ToggleSidebar, w, cx))),
                    )
                    .child(div().flex_1().h_full().id("sidebar-drag").on_mouse_down(MouseButton::Left, |_, w, _| w.start_window_move())),
            )
            .child(div().id("sidebar-scroll").flex_1().min_h_0().overflow_y_scroll().track_scroll(&self.sidebar_scroll).child(list))
            .child(
                h_flex()
                    .h(px(46.))
                    .flex_shrink_0()
                    .px(px(18.))
                    .gap(px(4.))
                    .child(self.footer_button("settings", IconName::Settings, cx.listener(|ws, _, w, cx| ws.open_settings(&OpenSettings, w, cx))))
                    .child(self.footer_button("new-section", IconName::Folder, cx.listener(|ws, _, w, cx| ws.new_section(None, w, cx))))
                    .child(self.footer_button("new-chat", IconName::Plus, cx.listener(|ws, _, w, cx| ws.new_chat_dialog(None, w, cx))))
                    .child(div().flex_1())
                    .when_some(self.settings_error.clone(), |d, e| {
                        d.child(div().id("settings-error").text_size(px(11.)).text_color(parse_color(&t.danger)).child("settings error").tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(e.clone()).build(w, cx)))
                    })
                    .child(
                        div()
                            .size(px(7.))
                            .rounded_full()
                            .bg(if self.connected { parse_color(&t.success) } else { parse_color(&t.warning) }),
                    ),
            )
            .into_any_element()
    }

    fn footer_button(&self, id: &'static str, icon: IconName, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Stateful<Div> {
        let t = &self.settings.theme;
        div()
            .id(id)
            .size(px(26.))
            .rounded(px(6.))
            .flex()
            .items_center()
            .justify_center()
            .hover(|s| s.bg(parse_color(&t.hover)))
            .child(Icon::new(icon).small().text_color(parse_color(&t.text_muted)))
            .on_click(f)
    }

    fn label(&self, text: &str, toggle: Option<(String, bool)>, cx: &mut Context<Self>) -> AnyElement {
        let t = &self.settings.theme;
        let sb = &self.settings.sidebar;
        let mut d = h_flex()
            .id(SharedString::from(format!("label:{text}")))
            .h(px(34.))
            .pt(px(12.))
            .px(px(8.))
            .gap(px(5.))
            .text_size(px(sb.label_size))
            .text_color(parse_color(&t.text_muted))
            .child(text.to_string());
        if let Some((key, collapsed)) = toggle {
            if collapsed {
                d = d.child(Icon::new(IconName::ChevronRight).xsmall());
            }
            let section_id = key.trim_start_matches("section:").to_string();
            let name = text.to_string();
            let view = cx.entity();
            let d = d.cursor_pointer().on_click(cx.listener(move |ws, _, _, cx| {
                if !ws.collapsed.remove(&key) {
                    ws.collapsed.insert(key.clone());
                }
                cx.notify();
            }));
            return d
                .context_menu(move |menu, window, _| {
                    let (id1, id2, n) = (section_id.clone(), section_id.clone(), name.clone());
                    menu.item(PopupMenuItem::new("Rename…").on_click(window.listener_for(&view, move |ws: &mut Workspace, _, window, cx| {
                        let id = id1.clone();
                        ws.name_dialog("Rename section", &n, window, cx, move |ws, new, cx| ws.call(Call::SectionRename { id: id.clone(), name: new }, cx));
                    })))
                    .separator()
                    .item(PopupMenuItem::new("Delete section").on_click(window.listener_for(&view, move |ws: &mut Workspace, _, _, cx| {
                        ws.call(Call::SectionDelete { id: id2.clone() }, cx)
                    })))
                })
                .into_any_element();
        }
        d.into_any_element()
    }

    fn pill(&self, id: &str, child: impl IntoElement) -> Stateful<Div> {
        let t = &self.settings.theme;
        h_flex()
            .id(SharedString::from(id.to_string()))
            .h(px(26.))
            .px(px(10.))
            .gap(px(6.))
            .rounded(px(13.))
            .border_1()
            .border_color(parse_color(&t.line))
            .bg(hsla(0., 0., 0.15, 1.))
            .hover(|s| s.bg(hsla(0., 0., 0.18, 1.)))
            .text_size(px(12.5))
            .text_color(parse_color(&t.text))
            .child(child)
    }

    fn toolbar(&mut self, _: &mut Window, cx: &mut Context<Self>) -> Div {
        let t = self.settings.theme.clone();
        let w = self.current_worktree().cloned();
        let chat = self.current_chat().cloned();
        let ssh_host = self.model.ssh_host.clone();
        let mut bar = h_flex()
            .gap(px(8.))
            .pl(px(if self.sidebar_visible { 23. } else { 104. }))
            .child(div().size(px(6.)).rounded_full().bg(if chat.as_ref().map(|c| c.alive).unwrap_or(false) { parse_color(&t.success) } else { parse_color(&t.text_faint) }));
        if let Some(w) = &w {
            let letter = w.project.chars().next().unwrap_or('R').to_ascii_uppercase().to_string();
            bar = bar
                .child(self.badge(&letter, w.hue, 16.))
                .child(div().text_size(px(13.)).text_color(parse_color(&t.text)).child(w.title.clone()));
            if let Some(c) = &chat {
                bar = bar
                    .child(Icon::new(IconName::ChevronRight).xsmall().text_color(parse_color(&t.text_faint)))
                    .child(div().text_size(px(12.)).text_color(parse_color(&t.age)).child(c.name.clone()));
            }
            if w.uncommitted > 0 {
                bar = bar.child(div().size(px(6.)).rounded_full().bg(parse_color(&t.warning)));
            }
        }
        bar = bar.child(div().flex_1().h_full().id("toolbar-drag").on_mouse_down(MouseButton::Left, |e, window, _| {
            if e.click_count == 2 { window.zoom_window() } else { window.start_window_move() }
        }));
        let status = self.status.clone();
        bar = bar.child(self.pill("status", div().font_family(t.mono_font_family.clone()).child(if self.connected { "agent box".into() } else { status })));
        if let Some(w) = &w {
            let url = format!("cursor://vscode-remote/ssh-remote+{}{}", ssh_host, w.path);
            bar = bar.child(self.pill("ide", Icon::new(IconName::SquareTerminal).small()).on_click(move |_, _, cx| cx.open_url(&url)));
            if let Some(mr) = &w.mr {
                let url = mr.url.clone();
                bar = bar.child(self.pill("mr", format!("!{}", mr.number)).on_click(move |_, _, cx| cx.open_url(&url)));
            }
        }
        let _ = cx;
        bar
    }

    fn chat_tabs(&mut self, _: &mut Window, cx: &mut Context<Self>) -> Div {
        let t = self.settings.theme.clone();
        let mut tabs = h_flex().items_stretch().pl(px(8.));
        let Some(w) = self.current_worktree().cloned() else { return tabs };
        let current = self.current_chat().map(|c| c.id.clone());
        for c in self.chats_of(&w).into_iter().cloned().collect::<Vec<_>>() {
            let on = current.as_deref() == Some(&c.id);
            let (path, id) = (w.path.clone(), c.id.clone());
            tabs = tabs.child(
                h_flex()
                    .id(SharedString::from(format!("tab:{}", c.id)))
                    .relative()
                    .gap(px(8.))
                    .px(px(14.))
                    .text_size(px(13.))
                    .text_color(if on { parse_color(&t.text) } else { parse_color(&t.age) })
                    .hover(|s| s.text_color(parse_color(&t.text)))
                    .child(Icon::new(IconName::Asterisk).xsmall().text_color(rgb(0xd97757)))
                    .child(div().max_w(px(240.)).truncate().child(c.name.clone()))
                    .when(c.state == "working", |d| d.child(Spinner::new().xsmall()))
                    .when(on, |d| d.child(div().absolute().left_0().right_0().bottom_0().h(px(2.)).bg(parse_color(&t.accent))))
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        ws.active_chat.insert(path.clone(), id.clone());
                        ws.sync_selection(window, cx);
                        cx.notify();
                    })),
            );
        }
        let dir = w.path.clone();
        tabs.child(div().w(px(1.)).my(px(10.)).mx(px(4.)).bg(parse_color(&t.line))).child(
            h_flex()
                .id("new-tab")
                .px(px(10.))
                .gap(px(4.))
                .text_color(parse_color(&t.text_muted))
                .hover(|s| s.text_color(parse_color(&t.text)))
                .child(Icon::new(IconName::Plus).xsmall())
                .on_click(cx.listener(move |ws, _, window, cx| ws.new_chat_dialog(Some(dir.clone()), window, cx))),
        )
    }

    fn chat_view(&mut self, _: &mut Window, cx: &mut Context<Self>) -> Div {
        let t = self.settings.theme.clone();
        let cs = self.settings.chat.clone();
        let Some(chat) = self.current_chat().cloned() else {
            let msg = if self.current_worktree().is_some() { "No chat in this worktree yet. Press + above to start one." } else { "Select a worktree" };
            return div().size_full().flex().items_center().justify_center().text_color(parse_color(&t.text_muted)).child(msg);
        };
        let items = self.transcripts.get(&chat.id).map(|t| t.items.clone()).unwrap_or_default();
        let mut list = v_flex().gap(px(14.)).px(px(28.)).py(px(18.)).when_some(cs.max_width, |d, w| d.max_w(px(w)).mx_auto().w_full());
        for (i, item) in items.iter().enumerate() {
            let key = format!("{}:{i}", chat.id);
            let el: Option<AnyElement> = match item {
                Item::User { text } => Some(
                    div()
                        .p(px(10.))
                        .rounded(px(8.))
                        .bg(hsla(0., 0., 0.16, 1.))
                        .border_1()
                        .border_color(parse_color(&t.line))
                        .text_size(px(cs.text_size))
                        .text_color(parse_color(&t.text))
                        .child(text.clone())
                        .into_any_element(),
                ),
                Item::Assistant { text } => Some(
                    div().text_size(px(cs.text_size)).text_color(parse_color(&t.text)).child(TextView::markdown(SharedString::from(key.clone()), text.clone())).into_any_element(),
                ),
                Item::Thinking { text } if cs.show_thinking => Some(
                    div().text_size(px(cs.text_size - 1.)).italic().text_color(parse_color(&t.text_muted)).child(text.clone()).into_any_element(),
                ),
                Item::Thinking { .. } => None,
                Item::Tool { id, name, summary, input, result, is_error } => {
                    let open = self.expanded_tools.contains(id);
                    let tid = id.clone();
                    let dot = if result.is_none() { parse_color(&t.warning) } else if *is_error { parse_color(&t.danger) } else { parse_color(&t.success) };
                    let mut col = v_flex().gap(px(6.)).child(
                        h_flex()
                            .id(SharedString::from(format!("tool:{id}")))
                            .gap(px(8.))
                            .cursor_pointer()
                            .child(div().size(px(7.)).rounded_full().bg(dot))
                            .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(parse_color(&t.text)).child(name.clone()))
                            .child(div().flex_1().truncate().text_size(px(13.)).text_color(parse_color(&t.text_muted)).child(summary.clone()))
                            .on_click(cx.listener(move |ws, _, _, cx| {
                                if !ws.expanded_tools.remove(&tid) {
                                    ws.expanded_tools.insert(tid.clone());
                                }
                                cx.notify();
                            })),
                    );
                    if open || (!cs.collapse_tool_calls) {
                        let detail = format!(
                            "{}{}",
                            serde_json::to_string_pretty(input).unwrap_or_default(),
                            result.as_ref().map(|r| format!("\n\n{}", r.chars().take(4000).collect::<String>())).unwrap_or_default()
                        );
                        col = col.child(
                            div()
                                .ml(px(15.))
                                .p(px(10.))
                                .rounded(px(6.))
                                .bg(hsla(0., 0., 0.09, 1.))
                                .font_family(t.mono_font_family.clone())
                                .text_size(px(12.))
                                .text_color(parse_color(&t.age))
                                .child(detail),
                        );
                    }
                    Some(col.into_any_element())
                }
                Item::Notice { text } => Some(div().text_size(px(12.5)).text_color(parse_color(&t.danger)).child(text.clone()).into_any_element()),
            };
            if let Some(el) = el {
                list = list.child(el);
            }
        }
        if chat.state == "waiting" {
            list = list.child(self.waiting_card(&chat, cx));
        }
        let composer = self.composers.get(&chat.id).cloned();
        v_flex()
            .size_full()
            .min_h_0()
            .child(div().id("chat-scroll").flex_1().min_h_0().overflow_y_scroll().track_scroll(&self.chat_scroll).child(list))
            .when_some(composer, |d, c| {
                d.child(
                    div()
                        .flex_shrink_0()
                        .px(px(20.))
                        .pb(px(14.))
                        .pt(px(4.))
                        .when_some(cs.max_width, |d, w| d.max_w(px(w + 16.)).mx_auto().w_full())
                        .child(
                            div()
                                .rounded(px(10.))
                                .border_1()
                                .border_color(parse_color(&t.line))
                                .bg(hsla(0., 0., 0.13, 1.))
                                .p(px(6.))
                                .child(Textarea::new(&c).appearance(false)),
                        ),
                )
            })
    }

    fn waiting_card(&self, chat: &Chat, cx: &mut Context<Self>) -> AnyElement {
        let t = &self.settings.theme;
        let w = chat.waiting_on.clone().unwrap_or_default();
        let message = w["message"].as_str().or(w["tool_name"].as_str()).unwrap_or("The agent is waiting for you").to_string();
        let options: Vec<String> = w["tool_input"]["questions"][0]["options"]
            .as_array()
            .map(|o| o.iter().filter_map(|x| x["label"].as_str().map(str::to_string)).collect())
            .unwrap_or_else(|| vec!["Yes".into(), "Yes, don't ask again".into(), "No".into()]);
        let mut row = h_flex().gap(px(6.)).flex_wrap();
        for (i, label) in options.iter().enumerate() {
            let id = chat.id.clone();
            let key = (i + 1).to_string();
            row = row.child(
                self.pill(&format!("opt:{}:{i}", chat.id), label.clone())
                    .on_click(cx.listener(move |ws, _, _, cx| ws.call(Call::ChatKeys { chat_id: id.clone(), keys: vec![key.clone()] }, cx))),
            );
        }
        let id = chat.id.clone();
        row = row.child(
            self.pill(&format!("esc:{}", chat.id), "Cancel")
                .on_click(cx.listener(move |ws, _, _, cx| ws.call(Call::ChatKeys { chat_id: id.clone(), keys: vec!["Escape".into()] }, cx))),
        );
        v_flex()
            .gap(px(10.))
            .p(px(12.))
            .rounded(px(10.))
            .border_1()
            .border_color(parse_color(&t.warning))
            .bg(hsla(0.1, 0.4, 0.12, 1.))
            .child(div().text_size(px(13.)).text_color(parse_color(&t.text)).child(message))
            .child(row)
            .into_any_element()
    }

    fn branch_bar(&mut self, _: &mut Window, cx: &mut Context<Self>) -> Div {
        let t = self.settings.theme.clone();
        let mut bar = h_flex().gap(px(10.)).px(px(16.)).cursor_row_resize();
        let Some(w) = self.current_worktree().cloned() else { return bar };
        let chat = self.current_chat().cloned();
        bar = bar
            .child(Icon::new(IconName::FolderOpen).xsmall().text_color(parse_color(&t.age)))
            .child(div().text_size(px(13.)).text_color(parse_color(&t.text)).child(w.branch.clone().unwrap_or("detached".into())))
            .child(div().flex_1());
        if w.uncommitted > 0 {
            bar = bar.child(div().text_size(px(12.5)).text_color(parse_color(&t.warning)).child(format!(
                "{} uncommitted change{}",
                w.uncommitted,
                if w.uncommitted == 1 { "" } else { "s" }
            )));
            if let Some(c) = &chat {
                let id = c.id.clone();
                bar = bar.child(self.pill("commit", "Commit and push").on_click(cx.listener(move |ws, _, _, cx| {
                    ws.call(Call::ChatSend { chat_id: id.clone(), text: "Commit and push your changes.".into() }, cx)
                })));
            }
        }
        if !w.is_primary {
            if let Some(b) = w.branch.clone() {
                bar = bar.child(self.pill("test-local", "Test on Mac").on_click(cx.listener(move |ws, _, window, cx| {
                    ws.run_local(vec!["chat".into(), "test-local".into(), "--branch".into(), b.clone()], window, cx)
                })));
            }
        }
        if let Some(c) = &chat {
            let (id, to) = (c.id.clone(), if c.host == "box" { "mac" } else { "box" });
            let label = if to == "mac" { "Move chat to Mac" } else { "Move chat to box" };
            bar = bar.child(self.pill("move", label).on_click(cx.listener(move |ws, _, window, cx| {
                ws.run_local(vec!["chat".into(), "move".into(), id.clone(), "--to".into(), to.into()], window, cx)
            })));
        }
        let start = self.sizes.get("chat-card").copied();
        bar.on_mouse_down(MouseButton::Left, cx.listener(move |ws, e: &MouseDownEvent, _, cx| {
            ws.resizing = Some(Resize { id: "chat-card".into(), vertical: true, start: f32::from(e.position.y), start_size: start.unwrap_or(-1.) });
            cx.notify();
        }))
    }

    fn terminal_tabs(&mut self, _: &mut Context<Self>) -> Div {
        let t = &self.settings.theme;
        h_flex().px(px(18.)).gap(px(4.)).child(
            div()
                .h(px(26.))
                .px(px(11.))
                .flex()
                .items_center()
                .rounded(px(13.))
                .border_1()
                .border_color(rgb(0x3b4350))
                .bg(rgb(0x22262e))
                .text_size(px(13.))
                .text_color(parse_color(&t.text))
                .child("Terminal"),
        )
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.register_terminals(cx);
        let layout = self.settings.layout.clone();
        let body = self.node(&layout, window, cx).unwrap_or_else(|| div().into_any_element());
        div()
            .id("workspace")
            .track_focus(&self.focus)
            .key_context("Workspace")
            .size_full()
            .bg(parse_color(&self.settings.theme.window))
            .text_color(parse_color(&self.settings.theme.text))
            .font_family(self.settings.theme.font_family.clone())
            .on_action(cx.listener(Self::toggle_sidebar))
            .on_action(cx.listener(Self::toggle_terminal))
            .on_action(cx.listener(Self::open_settings))
            .on_action(cx.listener(Self::reload))
            .on_action(cx.listener(Self::palette))
            .on_action(cx.listener(|ws, _: &NewChat, w, cx| {
                let dir = ws.selected.clone();
                ws.new_chat_dialog(dir, w, cx)
            }))
            .on_action(cx.listener(|ws, _: &NextWorktree, w, cx| ws.step_worktree(1, w, cx)))
            .on_action(cx.listener(|ws, _: &PreviousWorktree, w, cx| ws.step_worktree(-1, w, cx)))
            .on_mouse_move(cx.listener(|ws, e: &MouseMoveEvent, _, cx| {
                if let Some(r) = &ws.resizing {
                    let pos = if r.vertical { f32::from(e.position.y) } else { f32::from(e.position.x) };
                    let base = if r.start_size < 0. { 400. } else { r.start_size };
                    let v = (base + pos - r.start).clamp(if r.vertical { 120. } else { 200. }, 1600.);
                    ws.sizes.insert(r.id.clone(), v);
                    cx.notify();
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|ws, _, _, _| {
                if ws.resizing.take().is_some() {
                    save_sizes(&ws.sizes);
                }
            }))
            .child(body)
    }
}

fn split_emoji(title: &str) -> (Option<String>, String) {
    let mut chars = title.chars();
    if let Some(first) = chars.next() {
        if !first.is_ascii() && !first.is_alphanumeric() {
            let rest: String = chars.collect();
            let rest = rest.trim_start_matches('\u{fe0f}').trim_start().to_string();
            return (Some(first.to_string()), rest);
        }
    }
    (None, title.to_string())
}

fn age(ts: Option<f64>) -> String {
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

fn sizes_path() -> std::path::PathBuf {
    settings::path().with_file_name("ui-sizes.json")
}

fn load_sizes() -> HashMap<String, f32> {
    std::fs::read_to_string(sizes_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn save_sizes(sizes: &HashMap<String, f32>) {
    let _ = std::fs::write(sizes_path(), serde_json::to_string_pretty(sizes).unwrap_or_default());
}
