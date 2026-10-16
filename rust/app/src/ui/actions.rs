/*! Actions: what clicks, keys, menus, and palette entries do. An action is a name (`"palette.open"`),
an object `{"do": name, ...args}`, or a list of actions run in order. */

use super::expr::{to_val, with};
use crate::workspace::Workspace;
use agent_box_protocol::Call;
use gpui_kit::*;
use serde_json::{json, Value};

pub const ACTIONS: &[(&str, &str)] = &[
    ("state.set", "{key, value}: set a state value (value may be any JSON or a template)"),
    ("state.toggle", "{key, default?}: flip a boolean state value (default: its value when unset)"),
    ("state.cycle", "{key, values: [..]}: move a state value to the next item in values"),
    ("select", "{worktree?: path, chat?: id, offset?: n}: select a worktree and/or one of its chats; offset moves n worktrees down (negative: up)"),
    ("chat.new", "{dir?, repo?, name?, kind?, branch?, base?, prompt?, agent?}: start a chat at once; no dir = a fresh worktree in repo; agent: a key of `daemon.agents` (default `daemon.default_agent`)"),
    ("chat.send", "{chat, text}: send a message"),
    ("chat.keys", "{chat, keys: [..]}: press keys in the chat's session (Enter, Escape, Up, 1, ...)"),
    ("chat.archive / chat.stop / chat.start", "{chat}"),
    ("chat.close", "{chat}: close its tab and stop its Claude process (wherever it runs)"),
    ("chat.take_over", "{chat}: ask, then close a chat in the program running it (super.engineering) and continue it here"),
    ("chat.open", "{chat, host?, force?}: open a chat as a tab, resuming its process if it isn't running (host: where a chat from `.history` lives)"),
    ("tab.move", "{chat?, target: chat id, zone: before|after}: reorder a worktree's tabs (chat defaults to `dragged`)"),
    ("chat.interrupt", "{chat}: stop what the agent is doing (Esc), keeping the chat open"),
    ("queue.remove / queue.edit / queue.send_now", "{chat, index}: drop a queued message, put it back in the composer to change, or send it now"),
    ("clipboard.copy", "{text}: put text on the clipboard"),
    ("chat.fork", "{chat}: a new tab in the same worktree that starts from a copy of this chat's conversation"),
    ("completion.accept", "{index?}: take an entry from the composer's completion list (`.app.completion`)"),
    ("history.load", "fetch past chats from every host into `.history` (`.app.history_loading` while it runs)"),
    ("if", "{cond, then, else?}: run `then` when cond is truthy (true, a non-empty string other than \"false\"), else `else`"),
    ("confirm", "{title, text?, confirm?, cancel?, then}: ask first; `then` runs if they confirm. Shown by the Confirm component (`.app.confirm`)"),
    ("confirm.accept / confirm.cancel", "answer the open confirm dialog"),
    ("chat.attach", "{chat, paths: [..]}: attach files to a chat (preview chips above its composer; uploaded to the box for box chats; sent with the next message). An `on.drop` action gets the dropped files as `paths`."),
    ("composer.send", "{chat}: send what's in a chat's composer"),
    ("chat.move", "{chat, to: mac|box, scope?: section|worktree|chat}: move a chat between machines, with what daemon.move_together (or scope) takes along: `.selected.move` says what that is"),
    ("section.new", "{assign?: path}: ask for a name, create the section (and move that worktree into it)"),
    ("section.rename", "{id, name?}: rename (asks when name is missing)"),
    ("section.delete", "{id}"),
    ("section.move", "{id?, index? | target: section id with zone}: reorder a section (id defaults to `dragged`)"),
    ("worktree.move", "{path?, section?: id|null, before?: path | target: path with zone: before|after}: move a worktree into a section (null: under its project), next to another or last (path defaults to `dragged`; drops supply `zone`)"),
    ("project.move", "{repo?, index? | target: repo with zone}: reorder a project (repo defaults to `dragged`)"),
    ("section.assign", "{path, section}: move a worktree into a section (null = out of it)"),
    ("worktree.delete", "{path}"),
    ("terminal.new", "{path?, id?, label?, command?}: a terminal tab under the worktree (a fresh shell, or `command`)"),
    ("terminal.select / terminal.close", "{path?, id}: show or close a terminal tab (the last one can't be closed); `.selected.terminals` lists them"),
    ("shell.open", "{task, output, label?, path?}: a terminal tab following a background shell's output file"),
    ("subagent.open", "{chat, agent, label?}: open a subagent's conversation (read-only) as a tab next to its chat; `.selected.background` lists running ones"),
    ("usage.refresh", "fetch token usage from every machine, the plan's limits from Claude Code, and models.dev rates now (`.usage` holds them; they also refresh every usage.refresh_secs)"),
    ("nav.back / nav.forward", "go back or forward through the worktrees and chats you've looked at, like a browser (`.app.nav.back` / `.forward` say whether you can)"),
    ("transcript.load_earlier", "{chat?}: load `chat.transcript_page` more of a long chat's older messages (scrolling to the top does this too)"),
    ("worktree.rename_branch", "{path, branch?}: rename the worktree's git branch (asks when branch is missing)"),
    ("worktree.new", "{repo, host: mac|box, branch?}: a worktree on that branch, checked out if it exists, else new from origin/main (asks when branch is missing)"),
    ("project.settings", "{repo}: edit the project's name, badge letter and color (saved in state as project_name:REPO, project_letter:REPO, project_hue:REPO)"),
    ("project.open_remote", "{path}: open the repo's origin (GitLab, GitHub) in the browser"),
    ("project.unhide", "{repo}: show the project's hidden worktrees again (hidden ones have state hidden:PATH)"),
    ("project.restore", "{repo?}: bring back a removed project (state removed_project:REPO), or all of them"),
    ("worktree.label", "{path, label?}: rename a worktree in the sidebar; a leading emoji becomes its icon (asks when label is missing)"),
    ("worktree.test_local", "{branch}: check the branch out on the Mac with the box's latest work"),
    ("prompt", "{title, placeholder?, value?, then}: ask for text; `then` runs with it as `.input`"),
    ("palette.open", "the command palette"),
    ("steps.expand_all / steps.collapse_all", "{chat?}: open or close every step line in a chat's transcript"),
    ("ui.click", "{id?, label?, double?, press?}: click a clickable node as if by hand: exact node id, else the first whose id or tooltip contains `label` (inspect.json lists them); `double`/`press` use its double-click or mouse-down action"),
    ("ui.menu", "{id? | node?, item}: run an item of a node's right-click menu (node by exact id, else id or tooltip containing `node`; item by label, exact or containing)"),
    ("dialog.submit", "{values?: [..]}: fill the open dialog's fields in order (`.app.dialog`, values in inspect.json) and save it"),
    ("dialog.cancel", "close the open dialog without saving"),
    ("palette.run", "{label | index}: run a command palette entry (label exact, else the first containing it) without opening the palette"),
    ("chat.answer", "{chat?, option: index | label | \"cancel\"}: answer a waiting chat's question card (its options are in `.chats[].waiting_on`)"),
    ("chat.jump_to_latest", "{chat?}: scroll to the newest message and keep following"),
    ("completion.move / completion.close", "{by?}: move the composer's completion highlight, or close the list"),
    ("composer.set / composer.type", "{chat?, text}: replace or add to the text in a chat's message box (the designer's is chat `designer`)"),
    ("js", "{code, ...args}: run JavaScript with `args`, `ctx` (the context), and `ab` (see Scripts)"),
    ("<a script action>", "any name a script registers with ab.action(name, fn) works as an action everywhere"),
    ("app.snapshot", "capture the window to snapshot.png in the settings folder (for agents to look at)"),
    ("drop.preview", "{key, zone: before|inside|after, kind?, value?}: show a drop target's indicator without dragging (for styling), as if `value` were being dragged; cleared by the next drag"),
    ("diff.refresh", "reload the selected worktree's diff now"),
    ("diff.collapse_all / diff.expand_all", "collapse or expand every file in the diff"),
    ("designer.send", "{text}: ask the designer (a local agent that edits these settings live) for a change"),
    ("designer.reset", "end the designer conversation and start fresh"),
    ("designer.history", "reread the designer's past conversations into `.designer.sessions` (newest first: id, title (the latest message), first, messages, age_text, current)"),
    ("designer.open", "{id}: show and continue an earlier designer conversation (one from `.designer.sessions`)"),
    ("settings.undo", "put the settings file back as it was before the last designer request (repeatable)"),
    ("settings.open / settings.reload / settings.docs", "edit, reload, or read the settings reference"),
    ("notify", "{text}: a notification inside the window"),
    ("notify.system", "{title, text, chat?, sound?: bool}: a macOS notification banner; clicking it brings the window up on `chat`"),
    ("dock.bounce", "bounce the Dock icon once"),
    ("sound", "{name, volume?}: play a macOS sound by name (Glass, Ping, Funk, Hero, Submarine...) or an audio file path"),
    ("open", "{url}: open a URL or app link"),
    ("shell", "{run, env?: {NAME: value}, notify?: bool}: run a shell command on the Mac (pass user text through env, not into run); notify shows its last output line"),
    ("settings.agent", "{request}: a local Claude Code agent reads SETTINGS.md and edits your settings to do what request says; the app reloads when it saves"),
    ("window.move / window.zoom / app.quit", ""),
];

/** A one-shot local Claude Code run in the settings folder; it may only read and edit files there. */
const SETTINGS_AGENT: &str = r#"cd "$HOME/.agent-box/state/settings" && claude -p "You are customizing the a.box app for its user. Read SETTINGS.md in this folder first: it documents every setting and holds the full defaults. Then edit settings.jsonc (only that file) to do what the user asks, keeping their existing overrides. The app reloads on save; problems show up in its settings panel, so keep the file valid JSON with comments. When done, reply with one short sentence saying what you changed. The user's request: $REQUEST" --permission-mode acceptEdits --allowedTools Read Edit Write Glob Grep --settings '{"disableAllHooks": true}' 2>&1 | tail -n 1"#;

impl Workspace {
    /** The worktree listed after `target` in its list, skipping `moving`; None when `target` is last. */
    fn worktree_after(&self, target: &str, moving: &str) -> Option<String> {
        let m = &self.model;
        let lists = m.projects.iter().map(|p| &p.items).chain(m.sections.iter().map(|s| &s.items));
        for list in lists {
            if let Some(i) = list.iter().position(|w| w.path == target) {
                return list[i + 1..].iter().find(|w| w.path != moving).map(|w| w.path.clone());
            }
        }
        None
    }

    /** Applies a worktree move to the local model at once, so the sidebar settles before the box confirms. */
    fn move_worktree_now(&mut self, path: &str, section: Option<&str>, before: Option<&str>) {
        let m = &mut self.model;
        let mut taken = None;
        for list in m.projects.iter_mut().map(|p| &mut p.items).chain(m.sections.iter_mut().map(|s| &mut s.items)) {
            if let Some(i) = list.iter().position(|w| w.path == path) {
                taken = Some(list.remove(i));
                break;
            }
        }
        let Some(w) = taken else { return };
        let list = match section {
            Some(id) => m.sections.iter_mut().find(|s| s.id == id).map(|s| &mut s.items),
            None => m.projects.iter_mut().find(|p| p.repo == w.repo).map(|p| &mut p.items),
        };
        if let Some(list) = list {
            let at = before.and_then(|b| list.iter().position(|x| x.path == b)).unwrap_or(list.len());
            list.insert(at, w);
        }
    }

    fn rename_branch(&mut self, path: String, branch: String, cx: &mut Context<Self>) {
        let branch = branch.trim().to_string();
        if branch.is_empty() || self.worktree(&path).and_then(|w| w.branch.as_deref()) == Some(branch.as_str()) {
            return;
        }
        let call = Call::WorktreeRenameBranch { path: path.clone(), branch: branch.clone() };
        let remote = self.route(&call);
        let m = &mut self.model;
        for w in m.projects.iter_mut().flat_map(|p| p.items.iter_mut()).chain(m.sections.iter_mut().flat_map(|s| s.items.iter_mut())) {
            if w.path == path {
                w.branch = Some(branch.clone());
            }
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            if let Err(e) = remote.call(call).await {
                this.update(cx, |ws, cx| {
                    ws.pending_actions.push(json!({"do": "notify", "text": ws.tr("rename_branch_failed", "Couldn't rename the branch: {error}", &[("error", &e.to_string())])}));
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn new_worktree(&mut self, repo: String, host: String, branch: String, cx: &mut Context<Self>) {
        let remote = self.remote_for_host(&host);
        self.status = self.tr("making_worktree", "Making a worktree for {branch}…", &[("branch", &branch)]);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = remote.call(Call::WorktreeCreate { repo, branch: branch.trim().to_string() }).await;
            this.update(cx, |ws, cx| {
                match res {
                    Ok(v) => {
                        if let Some(p) = v["path"].as_str().or_else(|| v["response"]["path"].as_str()) {
                            ws.selected = Some(p.to_string());
                            ws.set_state("selected_worktree".into(), Value::String(p.to_string()));
                        }
                        ws.status = ws.tr("connected", "Connected", &[]);
                    }
                    Err(e) => {
                        ws.status = ws.tr("connected", "Connected", &[]);
                        ws.pending_actions.push(json!({"do": "notify", "text": ws.tr("new_worktree_failed", "Couldn't make the worktree: {error}", &[("error", &e.to_string())])}));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn project_settings(&mut self, repo: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = self.model.projects.iter().find(|p| p.repo == repo) else { return };
        let (name, letter, hue) = self.project_look(&repo, &p.name, p.hue);
        let fields = vec![("Name".to_string(), name), ("Badge letter".to_string(), letter), ("Badge color (hue, 0–360)".to_string(), format!("{hue:.0}"))];
        self.fields_dialog(&format!("{} settings", p.name), fields, window, cx, move |ws, vals, cx| {
            for (key, v) in ["project_name", "project_letter", "project_hue"].iter().zip(vals) {
                ws.set_state(format!("{key}:{repo}"), if v.trim().is_empty() { Value::Null } else { Value::String(v.trim().to_string()) });
            }
            cx.notify();
        });
    }

    fn label_worktree(&mut self, path: String, label: String, cx: &mut Context<Self>) {
        let m = &mut self.model;
        for w in m.projects.iter_mut().flat_map(|p| p.items.iter_mut()).chain(m.sections.iter_mut().flat_map(|s| s.items.iter_mut())) {
            if w.path == path {
                w.title = label.clone();
            }
        }
        self.call(Call::WorktreeLabel { path, label }, cx);
        cx.notify();
    }

    pub(crate) fn run_action(&mut self, action: &Value, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            Value::Null => {}
            Value::Array(list) => {
                for a in list {
                    self.run_action(a, window, cx);
                }
            }
            Value::String(name) => {
                self.log_action(action);
                self.dispatch(name, &serde_json::Map::new(), window, cx)
            }
            Value::Object(o) => {
                self.log_action(action);
                let name = o.get("do").and_then(Value::as_str).unwrap_or("").to_string();
                self.dispatch(&name, o, window, cx);
            }
            _ => {}
        }
    }

    fn dispatch(&mut self, name: &str, a: &serde_json::Map<String, Value>, window: &mut Window, cx: &mut Context<Self>) {
        let s = |k: &str| a.get(k).and_then(Value::as_str).map(str::to_string).filter(|x| !x.is_empty());
        let chat = || s("chat").or_else(|| self.current_chat().map(|c| c.id.clone()));
        match name {
            "state.set" => {
                if let Some(k) = s("key") {
                    self.set_state(k, a.get("value").cloned().unwrap_or(Value::Null));
                }
            }
            "state.toggle" => {
                if let Some(k) = s("key") {
                    let v = self.state.get(&k).and_then(Value::as_bool).or_else(|| a.get("default").and_then(Value::as_bool)).unwrap_or(false);
                    self.set_state(k, Value::Bool(!v));
                }
            }
            "state.cycle" => {
                if let (Some(k), Some(Value::Array(vals))) = (s("key"), a.get("values")) {
                    let cur = self.state.get(&k).cloned().unwrap_or(Value::Null);
                    let i = vals.iter().position(|v| *v == cur).map(|i| (i + 1) % vals.len()).unwrap_or(0);
                    if let Some(v) = vals.get(i) {
                        self.set_state(k, v.clone());
                    }
                }
            }
            "select" => {
                if let Some(n) = a.get("offset").and_then(Value::as_i64) {
                    let paths: Vec<String> = self.all_worktrees().iter().map(|w| w.path.clone()).collect();
                    if !paths.is_empty() {
                        let cur = self.selected.as_ref().and_then(|p| paths.iter().position(|x| x == p)).unwrap_or(0) as i64;
                        let next = (cur + n).rem_euclid(paths.len() as i64) as usize;
                        self.select(paths[next].clone(), window, cx);
                    }
                } else if let Some(p) = s("worktree") {
                    if let Some(c) = s("chat") {
                        self.active_chat.insert(p.clone(), c);
                    }
                    self.select(p, window, cx);
                } else if let Some(c) = s("chat") {
                    if let Some(p) = self.selected.clone() {
                        self.active_chat.insert(p, c);
                        self.sync_selection(window, cx);
                    }
                }
            }
            "chat.new" => {
                self.starting = s("dir").filter(|_| s("branch").is_none());
                let call = Call::ChatNew {
                    name: s("name").unwrap_or_else(|| self.tr("new_chat_name", "New chat", &[])),
                    kind: s("kind").unwrap_or_else(|| {
                        let k = if s("dir").is_some() && s("branch").is_none() { "chat.new_kind_in_folder" } else { "chat.new_kind" };
                        self.settings.get(k).and_then(Value::as_str).unwrap_or(if k == "chat.new_kind" { "orchestrator" } else { "primary" }).to_string()
                    }),
                    branch: s("branch"),
                    base: s("base"),
                    dir: s("dir"),
                    repo: s("repo"),
                    prompt: s("prompt"),
                    model: s("model"),
                    agent: s("agent"),
                };
                self.start_chat(call, cx);
            }
            "chat.send" => {
                if let (Some(id), Some(text)) = (chat(), s("text")) {
                    self.call(Call::ChatSend { chat_id: id, text }, cx);
                }
            }
            "chat.keys" => {
                let keys = a.get("keys").and_then(Value::as_array).map(|k| k.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
                if let Some(id) = chat() {
                    self.call(Call::ChatKeys { chat_id: id, keys }, cx);
                }
            }
            "chat.close" if chat().is_some_and(|id| id.starts_with("sub:")) => {
                if let Some(id) = chat() {
                    self.sub_chats.retain(|(_, c)| c.id != id);
                    self.opened.remove(&id);
                    self.active_chat.retain(|_, c| c != &id);
                    self.sync_selection(window, cx);
                }
            }
            "chat.close" => {
                if let Some(id) = chat() {
                    self.closed.insert(id.clone());
                    self.opened.remove(&id);
                    self.active_chat.retain(|_, c| c != &id);
                    let host = self.host_of_chat(&id);
                    if self.chat(&id).is_some_and(|c| c.host == "mac") && !self.remotes.contains_key("mac") {
                        self.run_local(vec!["chat".into(), "stop".into(), id], window, cx);
                    } else if host == "mac" && self.settings.get("chat.close_on_mac").and_then(Value::as_str) != Some("stop") {
                        // On the Mac, stopping only ends the turn; archiving closes super.engineering's tab
                        // (and its process). The chat stays in History.
                        self.call(Call::ChatArchive { chat_id: id }, cx);
                    } else {
                        self.call(Call::ChatStop { chat_id: id }, cx);
                    }
                    self.sync_selection(window, cx);
                }
            }
            "chat.open" => {
                if let Some(id) = chat() {
                    // One super.engineering still runs opens to watch; resuming it (force) takes it over.
                    let elsewhere = self.chat(&id).is_some_and(|c| c.status == "running_elsewhere")
                        && a.get("force").and_then(Value::as_bool) != Some(true);
                    self.closed.remove(&id);
                    self.opened.insert(id.clone());
                    let alive = self.chat(&id).map(|c| (c.alive, c.host.clone()));
                    if let Some(p) = self.all_worktrees().iter().find(|w| w.chat_ids.contains(&id)).map(|w| w.path.clone()) {
                        self.active_chat.insert(p.clone(), id.clone());
                        self.select(p, window, cx);
                    }
                    match alive {
                        _ if elsewhere => {}
                        Some((false, host)) if host == "mac" && !self.remotes.contains_key("mac") => {
                            self.run_local(vec!["chat".into(), "start".into(), id], window, cx)
                        }
                        Some((false, _)) => self.call(Call::ChatStart { chat_id: id }, cx),
                        // Not in any worktree we know yet (from history): start it on its host and
                        // select it when it shows up.
                        None => {
                            let host = s("host").unwrap_or_else(|| "box".into());
                            self.pending_select = Some(id.clone());
                            let remote = self.remote_for_host(&host);
                            cx.spawn(async move |_, _| {
                                let _ = remote.call(Call::ChatStart { chat_id: id }).await;
                            })
                            .detach();
                        }
                        _ => {}
                    }
                }
            }
            "chat.take_over" => {
                if let Some(id) = chat() {
                    let name = self.chat(&id).map(|c| c.name.clone()).unwrap_or_default();
                    let ask = self.takeover_confirm(&name, serde_json::json!({"do": "chat.open", "chat": id, "force": true}));
                    self.run_action(&ask, window, cx);
                }
            }
            "tab.move" => {
                let dragged = s("chat").or_else(|| a.get("dragged").and_then(Value::as_str).map(str::to_string));
                if let (Some(id), Some(target)) = (dragged, s("target")) {
                    if let Some(w) = self.all_worktrees().into_iter().find(|w| w.chat_ids.contains(&target)).cloned() {
                        let mut ids: Vec<String> = self.chats_of_path(&w.path);
                        ids.retain(|x| x != &id);
                        let at = ids.iter().position(|x| x == &target).map(|i| if s("zone").as_deref() == Some("after") { i + 1 } else { i }).unwrap_or(ids.len());
                        ids.insert(at, id);
                        let mut order = self.state.get("tab_order").and_then(Value::as_object).cloned().unwrap_or_default();
                        order.insert(w.path.clone(), serde_json::json!(ids));
                        self.set_state("tab_order".into(), Value::Object(order));
                    }
                }
            }
            "history.load" => self.load_history(cx),
            "queue.remove" | "queue.edit" | "queue.send_now" => {
                let index = a.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                if let Some(id) = chat() {
                    let text = self.queued.get_mut(&id).filter(|q| index < q.len()).map(|q| q.remove(index));
                    if let Some(text) = text {
                        match name {
                            // Back into the composer (ahead of anything typed there), to change and send again.
                            "queue.edit" => {
                                if let Some(state) = self.composers.get(&id).cloned() {
                                    state.update(cx, |s, cx| {
                                        let rest = s.value().to_string();
                                        let joined = if rest.trim().is_empty() { text.clone() } else { format!("{text}\n{rest}") };
                                        s.set_value(joined, window, cx);
                                        s.focus(window, cx);
                                    });
                                    window.dispatch_action(Box::new(gpui_kit::component::input::MoveToEnd), cx);
                                }
                            }
                            "queue.send_now" => self.send_chat(&id, text),
                            _ => {}
                        }
                    }
                    self.ctx_rev += 1;
                    cx.notify();
                }
            }
            "clipboard.copy" => {
                if let Some(t) = a.get("text").map(super::render::value_text) {
                    cx.write_to_clipboard(ClipboardItem::new_string(t));
                    let t = self.tr("copied", "Copied", &[]);
                    self.toast(t, window, cx);
                }
            }
            "chat.fork" => {
                if let Some(id) = chat() {
                    self.starting = self.all_worktrees().into_iter().find(|w| w.chat_ids.contains(&id)).map(|w| w.path.clone());
                    self.start_chat(Call::ChatFork { chat_id: id }, cx);
                }
            }
            "completion.accept" => {
                let index = a.get("index").and_then(Value::as_u64).map(|i| i as usize);
                self.accept_completion(index, window, cx);
            }
            // Stop ends the turn (Esc in Claude Code), on either machine; the chat keeps running. A chat
            // super.engineering is running can only be stopped there.
            "chat.interrupt" => {
                if let Some(id) = chat() {
                    if self.chat(&id).is_some_and(|c| c.status == "running_elsewhere") {
                        let t = self.tr("stop_elsewhere", "This chat is running in super.engineering: stop it there, or take it over first", &[]);
                        self.toast(t, window, cx);
                    } else {
                        self.call(Call::ChatKeys { chat_id: id, keys: vec!["Escape".into()] }, cx);
                    }
                }
            }
            "if" => {
                let truthy = match a.get("cond") {
                    Some(Value::Bool(b)) => *b,
                    Some(Value::String(t)) => !t.is_empty() && t != "false" && t != "null",
                    Some(Value::Null) | None => false,
                    Some(Value::Number(n)) => n.as_f64() != Some(0.),
                    Some(_) => true,
                };
                let next = if truthy { a.get("then") } else { a.get("else") };
                if let Some(next) = next.cloned() {
                    self.run_action(&next, window, cx);
                }
            }
            "confirm" => {
                self.confirm = Some(Value::Object(a.clone()));
                cx.notify();
            }
            "confirm.accept" => {
                if let Some(next) = self.confirm.take().and_then(|c| c.get("then").cloned()) {
                    self.run_action(&next, window, cx);
                }
                cx.notify();
            }
            "confirm.cancel" => {
                self.confirm = None;
                cx.notify();
            }
            "chat.archive" | "chat.stop" | "chat.start" => {
                if let Some(id) = chat() {
                    self.call(
                        match name {
                            "chat.archive" => Call::ChatArchive { chat_id: id },
                            "chat.stop" => Call::ChatStop { chat_id: id },
                            _ => Call::ChatStart { chat_id: id },
                        },
                        cx,
                    );
                }
            }
            "chat.attach" => {
                let paths: Vec<crate::workspace::Attachment> = a
                    .get("paths")
                    .and_then(Value::as_array)
                    .map(|p| p.iter().filter_map(Value::as_str).map(|x| crate::workspace::Attachment::Path(x.into())).collect())
                    .unwrap_or_default();
                if let Some(id) = chat() {
                    self.attach(id, paths, window, cx);
                }
            }
            "composer.send" => {
                if let Some(id) = chat() {
                    self.send_composer(&id, window, cx);
                }
            }
            "chat.move" => {
                if let Some(id) = chat() {
                    let to = s("to").unwrap_or("mac".into());
                    let mut args = vec!["chat".into(), "move".into(), id, "--to".into(), to];
                    if let Some(scope) = s("scope") {
                        args.extend(["--scope".into(), scope]);
                    }
                    self.run_local(args, window, cx);
                }
            }
            "section.new" => self.new_section(s("assign"), window, cx),
            "section.rename" => {
                if let Some(id) = s("id") {
                    match s("name") {
                        Some(n) => self.call(Call::SectionRename { id, name: n }, cx),
                        None => {
                            let current = self.model.sections.iter().find(|x| x.id == id).map(|x| x.name.clone()).unwrap_or_default();
                            self.name_dialog("Rename section", &current, window, cx, move |ws, n, cx| {
                                ws.call(Call::SectionRename { id: id.clone(), name: n }, cx)
                            });
                        }
                    }
                }
            }
            "section.delete" => {
                if let Some(id) = s("id") {
                    self.call(Call::SectionDelete { id }, cx);
                }
            }
            "section.assign" => {
                if let Some(path) = s("path") {
                    self.call(Call::SectionAssign { path, section: s("section") }, cx);
                }
            }
            "worktree.label" => {
                if let Some(path) = s("path") {
                    match s("label") {
                        Some(l) => self.label_worktree(path, l, cx),
                        None => {
                            let current = self.worktree(&path).map(|w| w.title.clone()).unwrap_or_default();
                            self.name_dialog("Rename worktree", &current, window, cx, move |ws, l, cx| ws.label_worktree(path.clone(), l, cx));
                        }
                    }
                }
            }
            "terminal.new" => {
                if let Some(path) = s("path").or_else(|| self.selected.clone()) {
                    let mut tabs = self.term_tabs(&path);
                    let id = s("id").unwrap_or_else(|| {
                        let n = tabs.iter().filter_map(|t| t["id"].as_str()?.parse::<u32>().ok()).max().unwrap_or(1) + 1;
                        n.to_string()
                    });
                    if !tabs.iter().any(|t| t["id"].as_str() == Some(id.as_str())) {
                        let n = tabs.len() + 1;
                        let mut tab = json!({"id": id, "label": s("label").unwrap_or_else(|| self.tr("terminal_n", "Terminal {n}", &[("n", &n.to_string())]))});
                        if let Some(c) = s("command") {
                            tab["command"] = Value::String(c);
                        }
                        tabs.push(tab);
                        self.set_state(format!("terms:{path}"), Value::Array(tabs));
                    }
                    self.set_state(format!("term_active:{path}"), Value::String(id));
                    self.set_state("bottom_tab".into(), Value::String("terminal".into()));
                    self.set_state("terminal_open".into(), Value::Bool(true));
                }
            }
            "terminal.select" => {
                if let (Some(path), Some(id)) = (s("path").or_else(|| self.selected.clone()), s("id")) {
                    self.set_state(format!("term_active:{path}"), Value::String(id));
                    self.set_state("bottom_tab".into(), Value::String("terminal".into()));
                }
            }
            "terminal.close" => {
                if let (Some(path), Some(id)) = (s("path").or_else(|| self.selected.clone()), s("id")) {
                    let mut tabs = self.term_tabs(&path);
                    if tabs.len() > 1 {
                        let at = tabs.iter().position(|t| t["id"].as_str() == Some(id.as_str()));
                        if let Some(i) = at {
                            tabs.remove(i);
                            if let Some(t) = self.terminals.remove(&Workspace::term_key(&path, &id)) {
                                t.read(cx).kill();
                            }
                            if self.term_active(&path) == id {
                                let next = tabs[i.min(tabs.len() - 1)]["id"].as_str().unwrap_or("1").to_string();
                                self.set_state(format!("term_active:{path}"), Value::String(next));
                            }
                            self.set_state(format!("terms:{path}"), Value::Array(tabs));
                        }
                    }
                }
            }
            "shell.open" => {
                if let (Some(task), Some(output)) = (s("task"), s("output")) {
                    let path = s("path").or_else(|| self.selected.clone());
                    let label = s("label").unwrap_or_else(|| self.tr("background_shell", "Background shell", &[]));
                    let quoted = format!("'{}'", output.replace('\'', "'\\''"));
                    let open = json!({"do": "terminal.new", "path": path, "id": format!("task-{task}"), "label": label,
                                      "command": format!("tail -n +1 -F {quoted}")});
                    self.run_action(&open, window, cx);
                }
            }
            "subagent.open" => {
                if let (Some(parent), Some(agent)) = (s("chat"), s("agent")) {
                    let id = format!("sub:{parent}:{agent}");
                    let path = self.all_worktrees().into_iter().find(|w| w.chat_ids.contains(&parent)).map(|w| w.path.clone());
                    if let (Some(path), Some(p)) = (path, self.chat(&parent).cloned()) {
                        if self.chat(&id).is_none() {
                            let mut c = p.clone();
                            c.id = id.clone();
                            c.name = s("label").unwrap_or_else(|| self.tr("subagent", "Subagent", &[]));
                            c.kind = "subagent".into();
                            c.status = "subagent".into();
                            c.alive = false;
                            c.state = "working".into();
                            c.waiting_on = None;
                            c.suggestion = None;
                            c.tmux = String::new();
                            self.sub_chats.push((path.clone(), c));
                        }
                        self.closed.remove(&id);
                        self.opened.insert(id.clone());
                        self.active_chat.insert(path.clone(), id);
                        self.select(path, window, cx);
                    }
                }
            }
            "usage.refresh" => self.refresh_usage(true, cx),
            "nav.back" => self.navigate(-1, window, cx),
            "nav.forward" => self.navigate(1, window, cx),
            "transcript.load_earlier" => {
                if let Some(id) = chat() {
                    self.load_earlier(&id, cx);
                }
            }
            "worktree.rename_branch" => {
                if let Some(path) = s("path") {
                    match s("branch") {
                        Some(b) => self.rename_branch(path, b, cx),
                        None => {
                            let current = self.worktree(&path).and_then(|w| w.branch.clone()).unwrap_or_default();
                            self.name_dialog("Rename branch", &current, window, cx, move |ws, b, cx| ws.rename_branch(path.clone(), b, cx));
                        }
                    }
                }
            }
            "worktree.new" => {
                let repo = s("repo").unwrap_or_else(|| "redo".into());
                let host = s("host").unwrap_or_else(|| "box".into());
                match s("branch") {
                    Some(b) => self.new_worktree(repo, host, b, cx),
                    None => {
                        let title = format!("New {repo} worktree from branch (new or existing)");
                        self.name_dialog(&title, "", window, cx, move |ws, b, cx| ws.new_worktree(repo.clone(), host.clone(), b, cx));
                    }
                }
            }
            "project.settings" => {
                if let Some(repo) = s("repo") {
                    self.project_settings(repo, window, cx);
                }
            }
            "project.open_remote" => {
                if let Some(path) = s("path") {
                    let remote = self.remote_for_host(&self.host_of_path(&path));
                    let call = Call::Exec { cmd: "git remote get-url origin".into(), cwd: Some(path), timeout_secs: Some(15) };
                    cx.spawn(async move |this, cx| {
                        let res = remote.call(call).await;
                        this.update(cx, |ws, cx| {
                            match res.ok().and_then(|v| v["stdout"].as_str().and_then(|u| web_url(u.trim()))) {
                                Some(url) => cx.open_url(&url),
                                None => ws.pending_actions.push(json!({"do": "notify", "text": ws.tr("no_remote", "This project has no origin remote", &[])})),
                            }
                            cx.notify();
                        })
                        .ok();
                    })
                    .detach();
                }
            }
            "project.unhide" => {
                if let Some(repo) = s("repo") {
                    let keys: Vec<String> = self.all_worktrees().iter().filter(|w| w.repo == repo).map(|w| format!("hidden:{}", w.path)).filter(|k| self.state.contains_key(k)).collect();
                    for k in keys {
                        self.set_state(k, Value::Null);
                    }
                }
            }
            "project.restore" => {
                let repos: Vec<String> = match s("repo") {
                    Some(r) => vec![r],
                    None => self.model.projects.iter().map(|p| p.repo.clone()).collect(),
                };
                for r in repos {
                    if self.state.contains_key(&format!("removed_project:{r}")) {
                        self.set_state(format!("removed_project:{r}"), Value::Null);
                    }
                }
            }
            "worktree.move" => {
                if let Some(path) = s("path").or_else(|| s("dragged")) {
                    let before = match (s("target"), s("zone").as_deref()) {
                        (Some(t), Some("after")) => self.worktree_after(&t, &path),
                        (Some(t), _) => Some(t),
                        (None, _) => s("before"),
                    };
                    if before.as_deref() != Some(path.as_str()) {
                        self.move_worktree_now(&path, s("section").as_deref(), before.as_deref());
                        self.call(Call::WorktreeMove { path, section: s("section"), before }, cx);
                    }
                }
            }
            "section.move" => {
                if let Some(id) = s("id").or_else(|| s("dragged")) {
                    let ids: Vec<String> = self.model.sections.iter().map(|x| x.id.clone()).filter(|x| *x != id).collect();
                    let after = s("zone").as_deref() == Some("after");
                    let target = s("target").or_else(|| s("before"));
                    let index = a.get("index").and_then(Value::as_u64).map(|i| i as usize).or_else(|| target.and_then(|b| ids.iter().position(|x| *x == b)).map(|i| i + after as usize)).unwrap_or(ids.len());
                    if let Some(from) = self.model.sections.iter().position(|x| x.id == id) {
                        let sec = self.model.sections.remove(from);
                        self.model.sections.insert(index.min(self.model.sections.len()), sec);
                    }
                    self.call(Call::SectionMove { id, index }, cx);
                }
            }
            "project.move" => {
                if let Some(repo) = s("repo").or_else(|| s("dragged")) {
                    let repos: Vec<String> = self.model.projects.iter().map(|p| p.repo.clone()).filter(|r| *r != repo).collect();
                    let after = s("zone").as_deref() == Some("after");
                    let target = s("target").or_else(|| s("before"));
                    let index = a.get("index").and_then(Value::as_u64).map(|i| i as usize).or_else(|| target.and_then(|b| repos.iter().position(|x| *x == b)).map(|i| i + after as usize)).unwrap_or(repos.len());
                    if let Some(from) = self.model.projects.iter().position(|p| p.repo == repo) {
                        let p = self.model.projects.remove(from);
                        self.model.projects.insert(index.min(self.model.projects.len()), p);
                    }
                    self.call(Call::ProjectMove { repo, index }, cx);
                }
            }
            "worktree.delete" => {
                if let Some(path) = s("path") {
                    self.call(Call::WorktreeDelete { path }, cx);
                }
            }
            "worktree.test_local" => {
                if let Some(b) = s("branch") {
                    self.run_local(vec!["chat".into(), "test-local".into(), "--branch".into(), b], window, cx);
                }
            }
            "prompt" => {
                let then = a.get("then").cloned().unwrap_or(Value::Null);
                let title = s("title").unwrap_or("Enter a value".into());
                let value = s("value").unwrap_or_default();
                self.name_dialog(&title, &value, window, cx, move |ws, text, cx| {
                    let ctx = with(&ws.context(), vec![("input".into(), to_val(&Value::String(text)))]);
                    let resolved = ws.exprs.resolve_action(&then, &ctx);
                    ws.pending_actions.push(resolved);
                    cx.notify();
                });
            }
            "palette.open" => self.open_palette(window, cx),
            "steps.expand_all" | "steps.collapse_all" => {
                if let Some(id) = chat() {
                    let keys: Vec<String> = self
                        .transcripts
                        .get(&id)
                        .map(|t| {
                            t.items
                                .iter()
                                .enumerate()
                                .flat_map(|(i, it)| match it {
                                    crate::transcript::Item::Tool { id: tid, .. } => vec![tid.clone(), format!("{id}:run:{i}")],
                                    crate::transcript::Item::Thinking { .. } => vec![format!("{id}:think:{i}"), format!("{id}:run:{i}")],
                                    _ => vec![],
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    if name == "steps.expand_all" {
                        self.expanded_tools.extend(keys);
                    } else {
                        for k in keys {
                            self.expanded_tools.remove(&k);
                        }
                    }
                }
            }
            "app.snapshot" => self.snapshot(window, cx),
            "ui.menu" => {
                // Run a right-click menu item without the mouse: the node by `id` (else the first whose id or
                // tooltip contains `node`), its item by `item` (exact, else containing; submenus included).
                let (id, node, item) = (s("id"), s("node").map(|l| l.to_lowercase()), s("item").unwrap_or_default());
                let menus: Vec<&(String, String, std::rc::Rc<Vec<Value>>, jaq_json::Val)> = self.menus.iter().chain(self.menus_last.iter()).collect();
                let menu = menus
                    .iter()
                    .find(|(nid, _, _, _)| id.as_deref() == Some(nid.as_str()))
                    .or_else(|| {
                        let base = id.as_deref()?;
                        menus.iter().find(|(nid, _, _, _)| nid.starts_with(base))
                    })
                    .or_else(|| {
                        let n = node.as_deref()?;
                        menus.iter().find(|(nid, tip, _, _)| nid.to_lowercase().contains(n) || tip.to_lowercase().contains(n))
                    })
                    .map(|(_, _, spec, ctx)| (spec.clone(), ctx.clone()));
                let Some((spec, ctx)) = menu else {
                    self.exprs.note(format!("ui.menu: no menu on {}", id.or(node).unwrap_or_default()));
                    return;
                };
                fn flat(entries: Vec<crate::ui::render::MenuEntry>, out: &mut Vec<(String, Value)>) {
                    for e in entries {
                        match e {
                            crate::ui::render::MenuEntry::Item { label, action, disabled: false, .. } => out.push((label, action)),
                            crate::ui::render::MenuEntry::Submenu { children, .. } => flat(children, out),
                            _ => {}
                        }
                    }
                }
                let mut items = vec![];
                flat(self.menu_entries(&spec, &ctx), &mut items);
                let low = item.to_lowercase();
                let pick = items.iter().find(|(l, _)| *l == item).or_else(|| items.iter().find(|(l, _)| l.to_lowercase().contains(&low))).map(|(_, a)| a.clone());
                match pick {
                    Some(action) => self.run_action(&action, window, cx),
                    None => self.exprs.note(format!("ui.menu: no item {item:?}; items: {}", items.iter().map(|(l, _)| l.as_str()).collect::<Vec<_>>().join(", "))),
                }
            }
            "dialog.submit" => {
                let values = a.get("values").and_then(Value::as_array).map(|v| v.iter().map(super::render::value_text).collect());
                self.submit_dialog(values, window, cx);
            }
            "dialog.cancel" => self.cancel_dialog(window, cx),
            "palette.run" => self.run_palette(s("label"), a.get("index").and_then(Value::as_u64).map(|i| i as usize), window, cx),
            "chat.answer" => {
                // A waiting chat's question card, without clicking: `option` is a 0-based index or a label;
                // "cancel" (or -1) presses Esc.
                if let Some(id) = chat() {
                    let w = self.chat(&id).and_then(|c| c.waiting_on.clone()).unwrap_or_default();
                    let (options, _) = crate::workspace::waiting_options(&w, &self.settings);
                    let pick = match a.get("option") {
                        Some(Value::Number(n)) => n.as_i64(),
                        Some(Value::String(l)) if l.eq_ignore_ascii_case("cancel") => Some(-1),
                        Some(Value::String(l)) => options.iter().position(|o| o.eq_ignore_ascii_case(l)).map(|i| i as i64),
                        _ => None,
                    };
                    match pick {
                        Some(-1) => self.call(Call::ChatKeys { chat_id: id, keys: vec!["Escape".into()] }, cx),
                        Some(i) if i >= 0 && (i as usize) < options.len() => self.call(Call::ChatKeys { chat_id: id, keys: vec![(i + 1).to_string()] }, cx),
                        _ => self.exprs.note(format!("chat.answer: no option {:?}; options: {}", a.get("option"), options.join(", "))),
                    }
                }
            }
            "chat.jump_to_latest" => {
                if let Some(id) = chat() {
                    self.jump_to_latest(&id);
                }
            }
            "completion.move" => {
                let by = a.get("by").and_then(Value::as_i64).unwrap_or(1);
                self.move_completion(by, cx);
            }
            "completion.close" => self.completion = None,
            "ui.click" => {
                let (id, label) = (s("id"), s("label").map(|l| l.to_lowercase()));
                let suffix = if a.get("double").and_then(Value::as_bool) == Some(true) { ":double" } else if a.get("press").and_then(Value::as_bool) == Some(true) { ":press" } else { "" };
                let id = id.map(|i| format!("{i}{suffix}"));
                // This frame's and the last (rows of virtual lists are drawn after the rest of the frame).
                let all: Vec<&(String, String, Value)> = self.clickables.iter().chain(self.clickables_last.iter()).collect();
                let hit = all
                    .iter()
                    .find(|(nid, _, _)| id.as_deref() == Some(nid.as_str()))
                    .or_else(|| {
                        // A repeated node's id gets its item key added: match the start, and the double/press suffix.
                        let base = id.as_deref()?.trim_end_matches(suffix);
                        all.iter().find(|(nid, _, _)| nid.starts_with(base) && nid.ends_with(suffix) && (!suffix.is_empty() || !nid.ends_with(":double") && !nid.ends_with(":press")))
                    })
                    .or_else(|| {
                        let l = label.as_deref()?;
                        all.iter().find(|(nid, tip, _)| (nid.to_lowercase().contains(l) || tip.to_lowercase().contains(l)) && nid.ends_with(suffix))
                    })
                    .map(|(_, _, a)| a.clone());
                match hit {
                    Some(action) => self.run_action(&action, window, cx),
                    None => self.exprs.note(format!("ui.click: nothing clickable matches {}", id.or(label).unwrap_or_default())),
                }
            }
            "composer.set" | "composer.type" => {
                let target = s("chat").or_else(|| self.current_chat().map(|c| c.id.clone()));
                if let Some(id) = target {
                    self.ensure_composer(&id, window, cx);
                    if let Some(c) = self.composers.get(&id).cloned() {
                        let text = s("text").unwrap_or_default();
                        let replace = name == "composer.set";
                        c.update(cx, |st, cx| if replace { st.set_value(text, window, cx) } else { st.insert(text, window, cx) });
                    }
                    self.update_completion(&id, cx);
                }
            }
            "drop.preview" => {
                self.drop_hint = s("key").map(|key| crate::ui::render::DropHint { key, zone: s("zone").unwrap_or("inside".into()), kind: s("kind").unwrap_or_default(), value: a.get("value").cloned().unwrap_or(Value::Null) });
                *self.drag_item.borrow_mut() = self.drop_hint.as_ref().map(|h| (h.kind.clone(), h.value.clone())).filter(|(_, v)| !v.is_null());
                self.drop_preview = true;
            }
            "diff.refresh" => {
                for d in self.diffs.values_mut() {
                    d.stale = true;
                    d.fetched = d.fetched.map(|t| t - std::time::Duration::from_secs(10));
                }
            }
            "diff.collapse_all" => {
                let paths: Vec<String> = self.diffs.values().flat_map(|d| d.files.iter().map(|f| f.path.clone())).collect();
                self.diff_collapsed.extend(paths);
            }
            "diff.expand_all" => self.diff_collapsed.clear(),
            "designer.send" => {
                if let Some(t) = s("text") {
                    self.designer_send(t, cx);
                }
            }
            "designer.reset" => self.designer_reset(cx),
            "designer.history" => {
                self.designer_sessions = crate::designer::sessions();
                self.ctx_rev += 1;
            }
            "designer.open" => {
                if let Some(id) = s("id") {
                    self.designer_open(&id, cx);
                }
            }
            "settings.undo" => {
                if !crate::designer::undo() {
                    let t = self.tr("nothing_to_undo", "Nothing to undo", &[]);
                    self.toast(t, window, cx);
                }
            }
            "settings.open" => self.open_settings(window, cx),
            "settings.reload" => self.reload_settings(cx),
            "settings.docs" => {
                let _ = std::process::Command::new("open").arg(crate::settings::dir().join("SETTINGS.md")).spawn();
            }
            "notify" => {
                self.toast(s("text").unwrap_or_default(), window, cx);
            }
            "notify.system" => {
                self.notify_count += 1;
                let id = format!("{}#{}", s("chat").unwrap_or_default(), self.notify_count);
                #[cfg(target_os = "macos")]
                crate::macos::notify(&id, &s("title").unwrap_or_else(|| self.tr("app_name", "a.box", &[])), &s("text").unwrap_or_default(), a.get("sound").and_then(Value::as_bool) != Some(false));
            }
            "sound" => {
                #[cfg(target_os = "macos")]
                if let Some(n) = s("name") {
                    crate::macos::play_sound(&n, a.get("volume").and_then(Value::as_f64).unwrap_or(1.) as f32);
                }
            }
            "dock.bounce" => {
                #[cfg(target_os = "macos")]
                crate::macos::bounce();
            }
            "open" => {
                if let Some(u) = s("url") {
                    cx.open_url(&u);
                }
            }
            "shell" => {
                if let Some(cmd) = s("run") {
                    let env: Vec<(String, String)> = match a.get("env") {
                        Some(Value::Object(e)) => e.iter().map(|(k, v)| (k.clone(), super::render::value_text(v))).collect(),
                        _ => vec![],
                    };
                    let notify = a.get("notify").and_then(Value::as_bool) == Some(true);
                    self.run_shell(cmd, env, notify, window, cx);
                }
            }
            "settings.agent" => {
                if let Some(req) = s("request") {
                    self.run_shell(SETTINGS_AGENT.into(), vec![("REQUEST".into(), req)], true, window, cx);
                }
            }
            "window.move" => window.start_window_move(),
            "window.zoom" => window.zoom_window(),
            "app.quit" => cx.quit(),
            "js" => {
                let code = s("code").unwrap_or_default();
                let mut args = a.clone();
                args.remove("code");
                args.remove("do");
                self.script_deliver(serde_json::json!({"type": "eval", "code": code, "args": args}), window, cx);
            }
            other if self.scripts.as_ref().is_some_and(|sc| sc.has_action(other)) => {
                let msg = crate::script::action_message(other, a);
                self.script_deliver(msg, window, cx);
            }
            // Actions that live with the views they drive (transcript steps, the diff, terminals).
            other if crate::ui::trace_view::dispatch(self, other, a, window, cx) => {}
            other if crate::ui::diff_view::dispatch(self, other, a, window, cx) => {}
            other if crate::terminal::dispatch(self, other, a, window, cx) => {}
            other => self.exprs.note(format!("unknown action `{other}`")),
        }
        self.pending_motion = true;
        self.ctx_rev += 1;
        cx.notify();
    }
}

/** The browser page for a git remote: `git@host:group/repo.git` or `https://host/group/repo.git`. */
fn web_url(remote: &str) -> Option<String> {
    let r = remote.trim().trim_end_matches(".git");
    if r.is_empty() {
        return None;
    }
    if let Some(rest) = r.strip_prefix("git@") {
        let (host, path) = rest.split_once(':')?;
        return Some(format!("https://{host}/{path}"));
    }
    if let Some(rest) = r.strip_prefix("ssh://") {
        let rest = rest.split_once('@').map(|x| x.1).unwrap_or(rest);
        let (host, path) = rest.split_once('/')?;
        return Some(format!("https://{}/{path}", host.split(':').next().unwrap_or(host)));
    }
    r.starts_with("http").then(|| r.to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn web_urls_from_remotes() {
        assert_eq!(super::web_url("git@gitlab.com:redo/redoapp/product/redo.git\n").as_deref(), Some("https://gitlab.com/redo/redoapp/product/redo"));
        assert_eq!(super::web_url("ssh://git@gitlab.com:22/tj15/agent-box.git").as_deref(), Some("https://gitlab.com/tj15/agent-box"));
        assert_eq!(super::web_url("https://gitlab.com/tj15/agent-box.git").as_deref(), Some("https://gitlab.com/tj15/agent-box"));
        assert_eq!(super::web_url(""), None);
    }
}
