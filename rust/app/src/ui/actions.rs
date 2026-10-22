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
    ("select", "{worktree?: path, chat?: id, offset?: n}: select a worktree and/or one of its chats; offset moves n worktrees down the sidebar (negative: up), in `sidebar.select_order`"),
    ("chat.new", "{dir?, repo?, name?, kind?, branch?, base?, prompt?, agent?, model?, select?}: start a chat at once; no dir = a fresh worktree in repo; agent: a key of `daemon.agents` (default `daemon.default_agent`); model: the agent's model; select (default `chat.select_new`): show it once it starts"),
    ("chat.show", "{chat}: select the worktree holding a chat with it as the active tab (the designer: open its panel); the default `notification.click`"),
    ("chat.send", "{chat, text}: send a message"),
    ("chat.keys", "{chat, keys: [..]}: press keys in the chat's session (Enter, Escape, Up, 1, ...)"),
    ("chat.archive / chat.stop / chat.start", "{chat}"),
    ("chat.close", "{chat}: close its tab and stop its Claude process (wherever it runs)"),
    ("chat.rename", "{chat, name?}: rename a chat and its tab; its agent's own titles no longer replace the name (asks when name is missing)"),
    ("chat.take_over", "{chat}: ask, then close a chat in the program running it (super.engineering) and continue it here"),
    ("chat.open", "{chat, host?, force?}: open a chat as a tab, resuming its process if it isn't running (host: where a chat from `.history` lives)"),
    ("tab.move", "{chat?, target: chat id, zone: before|after}: reorder a worktree's tabs (chat defaults to `dragged`)"),
    ("chat.interrupt", "{chat}: stop what the agent is doing (its `daemon.agents.<agent>.interrupt_keys`, default Escape), keeping the chat open"),
    ("chat.background", "{chat}: move the shell command the agent is waiting on to the background (`daemon.agents.<agent>.background_keys`; `.selected.chat.foreground_shell` says one is running), so the chat can go on"),
    ("queue.remove / queue.edit / queue.send_now", "{chat, index}: drop a queued message, put it back in the composer to change, or send it now"),
    ("clipboard.copy", "{text, quiet?}: put text on the clipboard (quiet: no \"Copied\" notice)"),
    ("clipboard.read", "{then}: `then` runs with the clipboard's text as `.input`"),
    ("chat.fork", "{chat}: a new tab in the same worktree that starts from a copy of this chat's conversation"),
    ("completion.accept", "{index?}: take an entry from the composer's completion list (`.app.completion`)"),
    ("history.load", "{hosts?: [..], limit?}: fetch past chats from every connected host (or those) into `.history` (`.app.history_loading` while it runs)"),
    ("if", "{cond, then, else?}: run `then` when cond is truthy (true, a non-empty string other than \"false\"), else `else`"),
    ("confirm", "{title, text?, confirm?, cancel?, then}: ask first; `then` runs if they confirm. Shown by the Confirm component (`.app.confirm`)"),
    ("confirm.accept / confirm.cancel", "answer the open confirm dialog"),
    ("chat.attach", "{chat, paths: [..]}: attach files to a chat (preview chips above its composer; uploaded to the box for box chats; sent with the next message). An `on.drop` action gets the dropped files as `paths`."),
    ("composer.send", "{chat}: send what's in a chat's composer"),
    ("chat.move", "{chat, to?: host (default `app.local_host`), scope?: section|worktree|chat, follow?}: move a chat between machines, with what daemon.move_together (or scope) takes along: `.selected.move` says what that is; follow (default `chat.move_follow`) selects it where it lands"),
    ("section.new", "{name?, assign?: path, host?}: create a section (asks for a name when it's missing) on host (default the worktree's, else `app.default_host`), and move that worktree into it"),
    ("section.rename", "{id, name?}: rename (asks when name is missing)"),
    ("section.delete", "{id}"),
    ("section.move", "{id?, index? | target: section id with zone}: reorder a section (id defaults to `dragged`); with several machines' sections the order is kept in `state.section_order`"),
    ("worktree.move", "{path?, section?: id|null, before?: path | target: path with zone: before|after}: move a worktree into a section (null: under its project), next to another or last (path defaults to `dragged`; drops supply `zone`); a project listing several machines' worktrees keeps its order in `state.worktree_order:REPO`"),
    ("project.move", "{repo?, index? | target: repo with zone}: reorder a project (repo defaults to `dragged`)"),
    ("section.assign", "{path, section}: move a worktree into a section (null = out of it)"),
    ("worktree.delete", "{path}"),
    ("terminal.new", "{path?, id?, label?, command?, cwd?, host?}: a terminal tab under the worktree (a fresh shell, or `command`), in the worktree's folder on its machine unless cwd / host say otherwise"),
    ("terminal.select / terminal.close", "{path?, id}: show or close a terminal tab (the last one can't be closed); `.selected.terminals` lists them"),
    ("shell.open", "{task, output, label?, path?}: a terminal tab following a background shell's output file"),
    ("subagent.open", "{chat, agent, label?}: open a subagent's conversation (read-only) as a tab next to its chat; `.selected.background` lists running ones"),
    ("usage.refresh", "{host?}: fetch token usage from every machine (or one), the plans' limits, and models.dev rates now (`.usage` holds them; they also refresh every usage.refresh_secs)"),
    ("nav.back / nav.forward", "{steps?}: go back or forward through the worktrees and chats you've looked at, like a browser (`.app.nav.back` / `.forward` say whether you can; `.app.nav.visits` lists them)"),
    ("transcript.load_earlier", "{chat?}: load `chat.transcript_page` more of a long chat's older messages (scrolling to the top does this too)"),
    ("worktree.rename_branch", "{path, branch?}: rename the worktree's git branch (asks when branch is missing)"),
    ("worktree.new", "{repo?, host?, branch?, base?, name?, select?, then?}: a worktree on that branch, checked out if it exists, else new from origin/<base> (default the repo's default branch) in a folder named name (asks when branch is missing); repo / host default to `worktree.default_repo` / `worktree.default_host` (else `app.default_host`); select (default `worktree.select`) shows it; `then` runs with its `.path`"),
    ("project.settings", "{repo}: edit the project's name, badge letter and color (saved in state as project_name:REPO, project_letter:REPO, project_hue:REPO)"),
    ("project.open_remote", "{path}: open the repo's origin (GitLab, GitHub) in the browser, at `project.remote_url_template` ({host}, {path})"),
    ("project.unhide", "{repo}: show the project's hidden worktrees again (hidden ones have state hidden:PATH)"),
    ("project.restore", "{repo?}: bring back a removed project (state removed_project:REPO), or all of them"),
    ("worktree.label", "{path, label?}: rename a worktree in the sidebar; a leading emoji becomes its icon (asks when label is missing)"),
    ("worktree.test_local", "{branch}: check the branch out on the Mac with the box's latest work"),
    ("prompt", "{title, placeholder?, value?, then}: ask for text; `then` runs with it as `.input`"),
    ("form", "{title, fields: [{label, value?} | label], then}: ask for several values at once; `then` runs with them as `.inputs` (in order) and the first as `.input`"),
    ("palette.open", "the command palette (drawn by `views.palette` when settings define one: `.app.palette`)"),
    ("palette.move / palette.confirm / palette.close", "{by?} / {index?}: for a palette drawn by `views.palette`: move the highlight, run an entry (default the highlighted one), close it. What's typed is state `palette_query`"),
    ("steps.expand_all / steps.collapse_all", "{chat?}: open or close every step line in a chat's transcript"),
    ("ui.click", "{id?, label?, double?, press?}: click a clickable node as if by hand: exact node id, else the first whose id or tooltip contains `label` (inspect.json lists them); `double`/`press` use its double-click or mouse-down action"),
    ("ui.menu", "{id? | node?, item}: run an item of a node's right-click menu (node by exact id, else id or tooltip containing `node`; item by label, exact or containing)"),
    ("dialog.submit", "{values?: [..]}: fill the open dialog's fields in order (`.app.dialog`, values in inspect.json) and save it. With `views.dialog` in settings, text dialogs are drawn by it and each field is state `dialog_field:N`"),
    ("dialog.cancel", "close the open dialog without saving"),
    ("palette.run", "{label | index}: run a command palette entry (label exact, else the first containing it) without opening the palette"),
    ("chat.answer", "{chat?, option: index | label | \"cancel\"}: answer a waiting chat's question card (its options are in `.chats[].waiting_on`), with its agent's `answer_key` / `cancel_keys`"),
    ("chat.jump_to_latest", "{chat?}: scroll to the newest message and keep following"),
    ("transcript.scroll", "{chat?, row, offset?}: scroll a chat's transcript so row (see inspect.json `transcripts`) is at the top, offset px into it; stops following new messages"),
    ("completion.move / completion.close", "{by?}: move the composer's completion highlight, or close the list"),
    ("composer.set / composer.type", "{chat?, text}: replace or add to the text in a chat's message box (the designer's is chat `designer`)"),
    ("composer.accept_suggestion", "{chat?}: put the agent's suggested next prompt in the message box"),
    ("composer.recall", "{chat?, by?: -1 | 1}: put an earlier message you sent back in the box (-1 older, 1 newer; past the newest empties it)"),
    ("focus", "{area: composer | terminal | none, chat?}: put typing in a chat's message box or the selected worktree's terminal"),
    ("chat.attach_clipboard", "{chat?}: attach the files or image on the clipboard to a chat"),
    ("<a script action>", "any name a script registers with ab.action(name, fn) works as an action everywhere"),
    ("inspect.refresh", "write inspect.json now (`abx state` asks first; otherwise it's written every `inspect.interval_ms`)"),
    ("app.snapshot", "{path?, then?}: capture the window to path (default snapshot.png in the settings folder, for agents to look at); then `then` runs with `.path`, and the `app.snapshot_done` event fires"),
    ("drop.preview", "{key, zone: before|inside|after, kind?, value?}: show a drop target's indicator without dragging (for styling), as if `value` were being dragged; cleared by the next drag"),
    ("diff.refresh", "reload the selected worktree's diff now"),
    ("designer.send", "{text}: ask the designer (a local agent that edits these settings live) for a change"),
    ("designer.reset", "end the designer conversation and start fresh"),
    ("designer.history", "reread the designer's past conversations into `.designer.sessions` (newest first: id, title (the latest message), first, messages, age_text, current)"),
    ("designer.open", "{id}: show and continue an earlier designer conversation (one from `.designer.sessions`)"),
    ("settings.undo", "put the settings file back as it was before the last designer request (repeatable)"),
    ("settings.open / settings.reload / settings.docs", "edit, reload, or read the settings reference (`views.settings` in settings draws the panel instead: `.app.settings_sheet`)"),
    ("settings.set", "{key, value} or {values: {key: value, ..}}: write values into your settings file and reload; a key is a dotted path (`daemon.state_repo_url`) or a list of keys, and the rest of the file (comments included) is left as it is"),
    ("settings.save / settings.close", "{text} / -: for a settings panel drawn by `views.settings`: write the settings file (and reload), close it"),
    ("notify", "{text}: a notification inside the window"),
    ("notify.system", "{title, text, subtitle?, group?, chat?, sound?: bool | name, on_click?}: a macOS notification banner (sound: false for none, or a sound's name); clicking it runs `on_click`, else the `notification.click` event (`.chat`, `.id`)"),
    ("dock.bounce", "{critical?}: bounce the Dock icon once (critical: until the app is in front)"),
    ("sound", "{name, volume?}: play a macOS sound by name (Glass, Ping, Funk, Hero, Submarine...) or an audio file path"),
    ("open", "{url}: open a URL or app link"),
    ("shell", "{run, env?: {NAME: value}, cwd?, host?, timeout? (secs), notify?: bool, then?}: run a shell command on the Mac, or on host's daemon (pass user text through env, not into run); notify shows its last output line; `then` runs with `.output` {code, stdout, stderr}"),
    ("box", "{method, params?, host?, then?}: any daemon call (e.g. `exec` {cmd, cwd?, timeout_secs?}) on host (default: the machine the call belongs to); `then` runs with `.output` (its reply) and `.error`"),
    ("http", "{url, method?, headers?, body?, timeout_secs?, then?}: an HTTP request (no shell; a JSON body is sent as JSON); `then` runs with `.output` {status, ok, headers, body}"),
    ("after", "{ms, then}: run `then` after ms milliseconds (resolved then, against the context at that time)"),
    ("emit", "{event, payload?}: raise an event: settings `events.<event>` handlers run with `.payload`, scripts' `ab.on(event)` get the payload"),
    ("settings.agent", "{request}: a local Claude Code agent reads SETTINGS.md and edits your settings to do what request says; the app reloads when it saves (`settings_agent.command`)"),
    ("window.move / window.zoom / window.minimize / window.fullscreen / window.close", "drag, zoom, minimize, toggle full screen, or close the window"),
    ("window.resize", "{width, height} (or {w, h}): resize the window, in points"),
    ("app.hide / app.activate / app.quit", "hide the app, bring it to the front, or quit"),
    ("image.view", "{path} or {chat, index} (a transcript image): show the image full size over the window; a click or Escape closes it"),
    ("image.close", "close the full-size image"),
];

/** A one-shot local Claude Code run in the settings folder; it may only read and edit files there
(`settings_agent.command`: `{dir}` is the settings folder, `{claude}` designer.claude, $REQUEST the request). */
const SETTINGS_AGENT: &str = r#"cd {dir} && {claude} -p "You are customizing the a.box app for its user. Read SETTINGS.md in this folder first: it documents every setting and holds the full defaults. Then edit settings.jsonc (only that file) to do what the user asks, keeping their existing overrides. The app reloads on save; problems show up in its settings panel, so keep the file valid JSON with comments. When done, reply with one short sentence saying what you changed. The user's request: $REQUEST" --permission-mode acceptEdits --allowedTools Read Edit Write Glob Grep --settings '{"disableAllHooks": true}' 2>&1 | tail -n 1"#;

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

    /** After a move within a project that lists several machines' worktrees: saves the order the sidebar
    shows now (`state.worktree_order:REPO`) and returns where the worktree's own machine puts it (before the
    next of its own worktrees in that order; none: last). */
    fn keep_worktree_order(&mut self, path: &str) -> Option<String> {
        let (i, p) = self.model.projects.iter().enumerate().find(|(_, p)| p.items.iter().any(|w| w.path == path))?;
        let at = p.items.iter().position(|w| w.path == path)?;
        let host = p.items[at].host.clone();
        let next = p.items[at + 1..].iter().find(|w| w.host == host).map(|w| w.path.clone());
        if self.project_hosts.get(i).is_some_and(|h| h.len() > 1) {
            let order = p.items.iter().map(|w| Value::String(w.path.clone())).collect();
            let key = format!("worktree_order:{}", p.repo);
            self.set_state(key, Value::Array(order));
        }
        next
    }

    /** Applies a worktree move to the local model at once, so the sidebar settles before the box confirms. */
    fn move_worktree_now(&mut self, path: &str, section: Option<&str>, before: Option<&str>) {
        move_worktree_in(&mut self.model, path, section, before);
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

    /** A worktree on `branch` in `repo` on `host` (new ones from `base`, in a folder named `name`); selected
    when `select`, then `then` runs with `.path`. */
    #[allow(clippy::too_many_arguments)]
    fn new_worktree(&mut self, repo: String, host: String, branch: String, base: Option<String>, name: Option<String>, select: bool, then: Option<Value>, cx: &mut Context<Self>) {
        let remote = self.remote_for_host(&host);
        self.status = self.tr("making_worktree", "Making a worktree for {branch}…", &[("branch", &branch)]);
        cx.notify();
        let branch = branch.trim().to_string();
        // The plain call when nothing else is asked, so daemons from before WorktreeCreateWith still work.
        let call = if base.is_none() && name.is_none() { Call::WorktreeCreate { repo, branch } } else { Call::WorktreeCreateWith { repo, branch, base, name } };
        cx.spawn(async move |this, cx| {
            let res = remote.call(call).await;
            this.update(cx, |ws, cx| {
                match res {
                    Ok(v) => {
                        if let Some(p) = v["path"].as_str().or_else(|| v["response"]["path"].as_str()) {
                            if select {
                                ws.selected = Some(p.to_string());
                                ws.set_state("selected_worktree".into(), Value::String(p.to_string()));
                            }
                            if let Some(then) = &then {
                                let c = with(&ws.context(), vec![("path".into(), to_val(&Value::String(p.to_string())))]);
                                let a = ws.exprs.resolve_action(then, &c);
                                ws.pending_actions.push(a);
                            }
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
        let fields = vec![
            (self.tr("project_field_name", "Name", &[]), name),
            (self.tr("project_field_letter", "Badge letter", &[]), letter),
            (self.tr("project_field_hue", "Badge color (hue, 0–360)", &[]), format!("{hue:.0}")),
        ];
        let title = self.tr("project_settings_title", "{name} settings", &[("name", &p.name)]);
        self.fields_dialog(&title, fields, window, cx, move |ws, vals, cx| {
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
                    let mut paths = self.sidebar_order();
                    if paths.is_empty() {
                        paths = self.all_worktrees().iter().map(|w| w.path.clone()).collect();
                    }
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
                // `select` (default `chat.select_new`): show the new chat once it starts.
                let select = a.get("select").and_then(Value::as_bool).or_else(|| self.settings.get("chat.select_new").and_then(Value::as_bool)).unwrap_or(true);
                self.starting = s("dir").filter(|_| s("branch").is_none() && select);
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
                    auto_name: s("name").is_none(),
                };
                self.start_chat(call, select, cx);
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
                    let local = self.local_host();
                    if self.chat(&id).is_some_and(|c| self.is_local_host(&c.host)) && !self.remotes.contains_key(&local) {
                        self.run_local(vec!["chat".into(), "stop".into(), id], window, cx);
                    } else if self.is_local_host(&host) && self.settings.get("chat.close_on_mac").and_then(Value::as_str) != Some("stop") {
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
                    let reopened = !self.chat(&id).is_some_and(|c| self.chat_open(c));
                    self.closed.remove(&id);
                    self.opened.insert(id.clone());
                    if reopened {
                        self.tab_to_end(&id);
                    }
                    let alive = self.chat(&id).map(|c| (c.alive, c.host.clone()));
                    if let Some(p) = self.all_worktrees().iter().find(|w| w.chat_ids.contains(&id)).map(|w| w.path.clone()) {
                        self.active_chat.insert(p.clone(), id.clone());
                        self.select(p, window, cx);
                    }
                    let no_local_daemon = !self.remotes.contains_key(&self.local_host());
                    match alive {
                        _ if elsewhere => {}
                        Some((false, host)) if self.is_local_host(&host) && no_local_daemon => {
                            self.run_local(vec!["chat".into(), "start".into(), id], window, cx)
                        }
                        Some((false, _)) => self.call(Call::ChatStart { chat_id: id }, cx),
                        // Not in any worktree we know yet (from history): start it on its host and
                        // select it when it shows up.
                        None => {
                            let host = s("host").unwrap_or_else(|| self.default_host());
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
            "history.load" => {
                let hosts = a.get("hosts").and_then(Value::as_array).map(|h| h.iter().filter_map(Value::as_str).map(str::to_string).collect());
                self.load_history(hosts, a.get("limit").and_then(Value::as_u64).map(|n| n as usize), cx)
            }
            "queue.remove" | "queue.edit" | "queue.send_now" => {
                let index = a.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                if let Some(id) = chat() {
                    let text = self.queued.get_mut(&id).filter(|q| index < q.len()).map(|q| q.remove(index));
                    if let Some(text) = text {
                        match name {
                            // Back into the composer (ahead of anything typed there), to change and send again.
                            "queue.edit" => self.refill_composer(&id, &text, window, cx),
                            "queue.send_now" => self.send_chat(&id, text, window, cx),
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
                    if a.get("quiet").and_then(Value::as_bool) != Some(true) {
                        let t = self.tr("copied", "Copied", &[]);
                        self.toast(t, window, cx);
                    }
                }
            }
            "clipboard.read" => {
                let text = cx.read_from_clipboard().and_then(|c| c.text()).unwrap_or_default();
                if let Some(then) = a.get("then") {
                    let c = with(&self.context(), vec![("input".into(), to_val(&Value::String(text)))]);
                    let next = self.exprs.resolve_action(then, &c);
                    self.run_action(&next, window, cx);
                }
            }
            "chat.fork" => {
                if let Some(id) = chat() {
                    self.starting = self.all_worktrees().into_iter().find(|w| w.chat_ids.contains(&id)).map(|w| w.path.clone());
                    self.start_chat(Call::ChatFork { chat_id: id }, true, cx);
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
                        let agent = self.chat(&id).map(|c| c.agent.clone()).unwrap_or_default();
                        let keys = self.agent_keys(&agent, "interrupt_keys");
                        let remote = self.route(&Call::ChatKeys { chat_id: id.clone(), keys: keys.clone() });
                        // A message stopped before the agent took it up comes back (`restored`: its first line):
                        // back into the composer to change, and out of the conversation.
                        cx.spawn_in(window, async move |this, cx| {
                            let Ok(v) = remote.call(Call::ChatKeys { chat_id: id.clone(), keys }).await else { return };
                            let Some(first) = v["restored"].as_str().map(str::to_string) else { return };
                            let _ = this.update_in(cx, |ws, window, cx| {
                                let text = ws.transcripts.get_mut(&id).and_then(|t| t.withdraw(&first)).unwrap_or(first);
                                ws.refill_composer(&id, &text, window, cx);
                                let note = ws.tr("message_restored", "Stopped before it started: your message is back in the box to edit", &[]);
                                ws.toast(note, window, cx);
                                ws.ctx_rev += 1;
                                cx.notify();
                            });
                        })
                        .detach();
                    }
                }
            }
            "chat.background" => {
                if let Some(id) = chat() {
                    let agent = self.chat(&id).map(|c| c.agent.clone()).unwrap_or_default();
                    let agents = self.settings.get("daemon.agents").cloned().unwrap_or(Value::Null);
                    let keys = crate::behavior::background_keys(&agents, &agent);
                    if keys.is_empty() {
                        let name = agents[&agent]["name"].as_str().unwrap_or(&agent).to_string();
                        let t = self.tr("background_unsupported", "{agent} can't move a running command to the background", &[("agent", &name)]);
                        self.toast(t, window, cx);
                    } else {
                        self.call(Call::ChatKeys { chat_id: id, keys }, cx);
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
                    let to = s("to").unwrap_or_else(|| self.local_host());
                    // `follow` (default `chat.move_follow`): select the chat where it lands.
                    let follow = a.get("follow").and_then(Value::as_bool).or_else(|| self.settings.get("chat.move_follow").and_then(Value::as_bool)).unwrap_or(true);
                    if follow {
                        self.pending_move = Some((id.clone(), to.clone()));
                    }
                    let mut args = vec!["chat".into(), "move".into(), id, "--to".into(), to];
                    if let Some(scope) = s("scope") {
                        args.extend(["--scope".into(), scope]);
                    }
                    self.run_local(args, window, cx);
                }
            }
            "section.new" => self.new_section(s("name"), s("assign"), s("host"), window, cx),
            "section.rename" => {
                if let Some(id) = s("id") {
                    match s("name") {
                        Some(n) => self.call(Call::SectionRename { id, name: n }, cx),
                        None => {
                            let current = self.model.sections.iter().find(|x| x.id == id).map(|x| x.name.clone()).unwrap_or_default();
                            let title = self.tr("rename_section_title", "Rename section", &[]);
                            self.name_dialog(&title, &current, window, cx, move |ws, n, cx| {
                                ws.call(Call::SectionRename { id: id.clone(), name: n }, cx)
                            });
                        }
                    }
                }
            }
            "chat.rename" => {
                if let Some(id) = chat() {
                    match s("name") {
                        Some(n) => self.call(Call::ChatRename { chat_id: id, name: n }, cx),
                        None => {
                            let current = self.chat(&id).map(|c| c.name.clone()).unwrap_or_default();
                            let title = self.tr("rename_chat_title", "Rename chat", &[]);
                            self.name_dialog(&title, &current, window, cx, move |ws, n, cx| {
                                ws.call(Call::ChatRename { chat_id: id.clone(), name: n }, cx)
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
                            let title = self.tr("rename_worktree_title", "Rename worktree", &[]);
                            self.name_dialog(&title, &current, window, cx, move |ws, l, cx| ws.label_worktree(path.clone(), l, cx));
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
                        // Its own folder and machine instead of the worktree's.
                        for k in ["command", "cwd", "host"] {
                            if let Some(v) = s(k) {
                                tab[k] = Value::String(v);
                            }
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
                        self.tab_to_end(&id);
                        self.active_chat.insert(path.clone(), id);
                        self.select(path, window, cx);
                    }
                }
            }
            "usage.refresh" => self.refresh_usage(true, s("host"), cx),
            "nav.back" | "nav.forward" => {
                let steps = a.get("steps").and_then(Value::as_i64).unwrap_or(1).max(1);
                let dir = if name == "nav.back" { -1 } else { 1 };
                for _ in 0..steps {
                    self.navigate(dir, window, cx);
                }
            }
            "transcript.load_earlier" => {
                if let Some(id) = chat() {
                    self.load_earlier(&id, None, cx);
                }
            }
            "worktree.rename_branch" => {
                if let Some(path) = s("path") {
                    match s("branch") {
                        Some(b) => self.rename_branch(path, b, cx),
                        None => {
                            let current = self.worktree(&path).and_then(|w| w.branch.clone()).unwrap_or_default();
                            let title = self.tr("rename_branch_title", "Rename branch", &[]);
                            self.name_dialog(&title, &current, window, cx, move |ws, b, cx| ws.rename_branch(path.clone(), b, cx));
                        }
                    }
                }
            }
            "worktree.new" => {
                let repo = s("repo").unwrap_or_else(|| self.settings.text_at("worktree.default_repo", "redo"));
                let host = s("host").or_else(|| self.settings.get("worktree.default_host").and_then(Value::as_str).map(str::to_string)).unwrap_or_else(|| self.default_host());
                let select = a.get("select").and_then(Value::as_bool).or_else(|| self.settings.get("worktree.select").and_then(Value::as_bool)).unwrap_or(true);
                let then = a.get("then").cloned();
                let (base, folder) = (s("base"), s("name"));
                match s("branch") {
                    Some(b) => self.new_worktree(repo, host, b, base, folder, select, then, cx),
                    None => {
                        let title = self.tr("new_worktree_title", "New {repo} worktree from branch (new or existing)", &[("repo", &repo)]);
                        self.name_dialog(&title, "", window, cx, move |ws, b, cx| ws.new_worktree(repo.clone(), host.clone(), b, base.clone(), folder.clone(), select, then.clone(), cx));
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
                    // `project.remote_url_template`: the page for a remote's host and repo path.
                    let template = self.settings.text_at("project.remote_url_template", "https://{host}/{path}");
                    cx.spawn(async move |this, cx| {
                        let res = remote.call(call).await;
                        this.update(cx, |ws, cx| {
                            match res.ok().and_then(|v| v["stdout"].as_str().and_then(|u| web_url(u.trim(), &template))) {
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
                        let host_before = if s("section").is_none() { self.keep_worktree_order(&path) } else { before.clone() };
                        let host = self.host_of_path(&path);
                        self.pending_moves.retain(|m| m.path != path);
                        self.pending_moves.push(crate::workspace::PendingMove { path: path.clone(), host, section: s("section"), before, host_before: host_before.clone(), at: std::time::Instant::now() });
                        self.call(Call::WorktreeMove { path, section: s("section"), before: host_before }, cx);
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
                    // Its machine only knows its own sections: the index among those, and the mixed order in state.
                    let host = self.section_hosts.get(&id).cloned();
                    let mine: Vec<&String> = self.model.sections.iter().map(|x| &x.id).filter(|x| self.section_hosts.get(*x) == host.as_ref()).collect();
                    let index = mine.iter().position(|x| **x == id).unwrap_or(index);
                    if self.section_hosts.values().collect::<std::collections::HashSet<_>>().len() > 1 {
                        let order = self.model.sections.iter().map(|x| Value::String(x.id.clone())).collect();
                        self.set_state("section_order".into(), Value::Array(order));
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
                let title = s("title").unwrap_or_else(|| self.tr("prompt_title", "Enter a value", &[]));
                let value = s("value").unwrap_or_default();
                self.name_dialog(&title, &value, window, cx, move |ws, text, cx| {
                    let ctx = with(&ws.context(), vec![("input".into(), to_val(&Value::String(text)))]);
                    let resolved = ws.exprs.resolve_action(&then, &ctx);
                    ws.pending_actions.push(resolved);
                    cx.notify();
                });
            }
            "form" => {
                // Several fields at once: `then` runs with `.inputs` (in order) and `.input` (the first).
                let then = a.get("then").cloned().unwrap_or(Value::Null);
                let title = s("title").unwrap_or_else(|| self.tr("prompt_title", "Enter a value", &[]));
                let fields: Vec<(String, String)> = a
                    .get("fields")
                    .and_then(Value::as_array)
                    .map(|f| f.iter().map(|x| match x {
                        Value::String(l) => (l.clone(), String::new()),
                        o => (o.get("label").map(super::render::value_text).unwrap_or_default(), o.get("value").map(super::render::value_text).unwrap_or_default()),
                    }).collect())
                    .unwrap_or_default();
                self.fields_dialog(&title, fields, window, cx, move |ws, vals, cx| {
                    let first = vals.first().cloned().unwrap_or_default();
                    let ctx = with(&ws.context(), vec![("inputs".into(), to_val(&json!(vals))), ("input".into(), to_val(&Value::String(first)))]);
                    let resolved = ws.exprs.resolve_action(&then, &ctx);
                    ws.pending_actions.push(resolved);
                    cx.notify();
                });
            }
            "palette.open" => self.open_palette(window, cx),
            "palette.move" | "palette.confirm" | "palette.close" => self.palette_action(name, a, window, cx),
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
            "app.snapshot" => self.snapshot(s("path"), a.get("then").cloned(), window, cx),
            // Nothing to do: a request always ends with inspect.json written afresh.
            "inspect.refresh" => {}
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
                    let agent = self.chat(&id).map(|c| c.agent.clone()).unwrap_or_default();
                    match pick {
                        Some(-1) => {
                            let keys = self.agent_keys(&agent, "cancel_keys");
                            self.call(Call::ChatKeys { chat_id: id, keys }, cx)
                        }
                        Some(i) if i >= 0 && (i as usize) < options.len() => {
                            let key = self.answer_key(&agent, i as usize + 1);
                            self.call(Call::ChatKeys { chat_id: id, keys: vec![key] }, cx)
                        }
                        _ => self.exprs.note(format!("chat.answer: no option {:?}; options: {}", a.get("option"), options.join(", "))),
                    }
                }
            }
            "transcript.scroll" => {
                if let Some(id) = chat() {
                    let row = a.get("row").and_then(Value::as_u64).unwrap_or(0) as usize;
                    let offset = a.get("offset").and_then(Value::as_f64).unwrap_or(0.) as f32;
                    if let Some(l) = self.lists.get(&id) {
                        l.state.scroll_to(gpui::ListOffset { item_ix: row, offset_in_item: gpui::px(offset) });
                    }
                }
            }
            "image.view" => {
                let path = s("path");
                let index = a.get("index").and_then(Value::as_u64);
                let shown = match (&path, chat(), index) {
                    (Some(p), _, _) => Some((p.clone(), ImageSource::from(std::path::PathBuf::from(p)))),
                    (None, Some(c), Some(i)) => {
                        let key = format!("{c}:{i}");
                        self.images.get(&key).cloned().flatten().map(|img| (key, ImageSource::from(img)))
                    }
                    _ => None,
                };
                if shown.is_some() {
                    self.image_view = shown;
                    self.ctx_rev += 1;
                }
            }
            "image.close" => {
                if self.image_view.take().is_some() {
                    self.ctx_rev += 1;
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
                    None if crate::ui::drive::reveal_and_click(self, id.as_deref(), a) => {}
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
            "settings.close" => {
                self.settings_sheet = false;
            }
            "settings.save" => {
                if let Some(text) = a.get("text").and_then(Value::as_str) {
                    let _ = std::fs::write(crate::settings::path(), text);
                    self.reload_settings(cx);
                }
            }
            "settings.set" => {
                // {key, value} or {values: {key: value, ...}}; keys are dotted paths or lists of keys.
                let keys = |k: &Value| -> Vec<String> {
                    match k {
                        Value::Array(a) => a.iter().map(super::render::value_text).collect(),
                        k => super::render::value_text(k).split('.').map(str::to_string).collect(),
                    }
                };
                let mut writes: Vec<(Vec<String>, Value)> = a.get("key").map(|k| (keys(k), a.get("value").cloned().unwrap_or(Value::Null))).into_iter().collect();
                if let Some(Value::Object(m)) = a.get("values") {
                    writes.extend(m.iter().map(|(k, v)| (keys(&Value::String(k.clone())), v.clone())));
                }
                let failed: Vec<String> = writes.iter().filter_map(|(k, v)| crate::settings::set_value(k, v).err().map(|e| format!("settings.set {}: {e:#}", k.join(".")))).collect();
                for e in &failed {
                    self.exprs.note(e.clone());
                }
                self.reload_settings(cx);
            }
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
                let title = s("title").unwrap_or_else(|| self.tr("app_name", "a.box", &[]));
                let text = s("text").unwrap_or_default();
                // sound: true/absent (the default sound), false (none), or a sound's name.
                let sound = match a.get("sound") {
                    Some(Value::Bool(false)) => None,
                    Some(Value::String(n)) if !n.is_empty() => Some(n.clone()),
                    _ => Some(String::new()),
                };
                if let Some(action) = a.get("on_click").filter(|d| !d.is_null()) {
                    self.notify_actions.insert(id.clone(), action.clone());
                }
                self.record_notice("system", &text, Some(&title));
                #[cfg(target_os = "macos")]
                crate::macos::notify(&id, &title, &text, &crate::macos::NotifyOptions { sound, subtitle: s("subtitle"), group: s("group") });
                #[cfg(not(target_os = "macos"))]
                let _ = sound;
            }
            "sound" => {
                #[cfg(target_os = "macos")]
                if let Some(n) = s("name") {
                    crate::macos::play_sound(&n, a.get("volume").and_then(Value::as_f64).unwrap_or(1.) as f32);
                }
            }
            "dock.bounce" => {
                #[cfg(target_os = "macos")]
                crate::macos::bounce(a.get("critical").and_then(Value::as_bool) == Some(true));
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
                    let then = a.get("then").cloned();
                    if then.is_none() && s("host").is_none() && s("cwd").is_none() && a.get("timeout").is_none() {
                        self.run_shell(cmd, env, notify, window, cx);
                    } else {
                        if notify {
                            let t = self.tr("working", "Working…", &[]);
                            self.toast(t, window, cx);
                        }
                        let job = crate::workspace::ShellJob { run: cmd, cwd: s("cwd"), host: s("host"), timeout: a.get("timeout").and_then(Value::as_f64), env };
                        self.start_shell(job, window, cx, move |ws, output, window, cx| {
                            if notify {
                                let text = output[if output["code"] == 0 { "stdout" } else { "stderr" }].as_str().unwrap_or("").trim().lines().last().map(str::to_string);
                                let fallback = if output["code"] == 0 { ws.tr("done", "Done", &[]) } else { ws.tr("failed", "Failed", &[]) };
                                ws.toast(text.unwrap_or(fallback), window, cx);
                            }
                            if let Some(then) = &then {
                                let c = with(&ws.context(), vec![("output".into(), to_val(&output))]);
                                let next = ws.exprs.resolve_action(then, &c);
                                ws.run_action(&next, window, cx);
                            }
                        });
                    }
                }
            }
            "box" => {
                // Any daemon call (`{method, params}`, e.g. exec) on `host` (default: where the call belongs).
                let call = serde_json::from_value::<Call>(json!({"method": a.get("method"), "params": a.get("params").cloned().unwrap_or(json!({}))}));
                match call {
                    Ok(call) => {
                        let remote = match s("host") {
                            Some(h) => self.remote_for_host(&h),
                            None => self.route(&call),
                        };
                        let then = a.get("then").cloned();
                        cx.spawn_in(window, async move |this, cx| {
                            let res = remote.call(call).await;
                            this.update_in(cx, |ws, window, cx| {
                                let (output, error) = match res {
                                    Ok(v) => (v, Value::Null),
                                    Err(e) => {
                                        ws.exprs.note(format!("box: {e}"));
                                        (Value::Null, Value::String(e.to_string()))
                                    }
                                };
                                if let Some(then) = &then {
                                    let c = with(&ws.context(), vec![("output".into(), to_val(&output)), ("error".into(), to_val(&error))]);
                                    let next = ws.exprs.resolve_action(then, &c);
                                    ws.run_action(&next, window, cx);
                                }
                            })
                            .ok();
                        })
                        .detach();
                    }
                    Err(e) => self.exprs.note(format!("box: unknown call: {e}")),
                }
            }
            "http" => {
                let req = Value::Object(a.clone());
                let then = a.get("then").cloned();
                let path = self.path_env();
                cx.spawn_in(window, async move |this, cx| {
                    let output = cx.background_spawn(async move { crate::script::http(&req, &path) }).await;
                    this.update_in(cx, |ws, window, cx| {
                        if let Some(e) = output["error"].as_str() {
                            ws.exprs.note(format!("http: {e}"));
                        }
                        if let Some(then) = &then {
                            let c = with(&ws.context(), vec![("output".into(), to_val(&output))]);
                            let next = ws.exprs.resolve_action(then, &c);
                            ws.run_action(&next, window, cx);
                        }
                    })
                    .ok();
                })
                .detach();
            }
            "after" => {
                let ms = a.get("ms").and_then(Value::as_f64).unwrap_or(0.).max(0.);
                let then = a.get("then").cloned().unwrap_or(Value::Null);
                cx.spawn_in(window, async move |this, cx| {
                    cx.background_executor().timer(std::time::Duration::from_secs_f64(ms / 1000.)).await;
                    this.update_in(cx, |ws, window, cx| {
                        let c = ws.context();
                        let next = ws.exprs.resolve_action(&then, &c);
                        ws.run_action(&next, window, cx);
                    })
                    .ok();
                })
                .detach();
            }
            "emit" => {
                if let Some(event) = s("event") {
                    let payload = a.get("payload").cloned().unwrap_or(Value::Null);
                    self.fire_event_as(&event, vec![("payload".into(), to_val(&payload))], Some(payload), window, cx);
                }
            }
            "settings.agent" => {
                if let Some(req) = s("request") {
                    let claude = crate::script::expand(&self.settings.text_at("designer.claude", "claude"));
                    let template = self.settings.get("settings_agent.command").and_then(Value::as_str).unwrap_or(SETTINGS_AGENT).to_string();
                    let script = template.replace("{dir}", &crate::behavior::sh_quote(&crate::settings::dir().to_string_lossy())).replace("{claude}", &crate::behavior::sh_quote(&claude));
                    self.run_shell(script, vec![("REQUEST".into(), req)], true, window, cx);
                }
            }
            "window.move" => window.start_window_move(),
            "window.zoom" => window.zoom_window(),
            "window.minimize" => window.minimize_window(),
            "window.fullscreen" => window.toggle_fullscreen(),
            "window.resize" => {
                let n = |k: &str, d: f32| a.get(k).and_then(Value::as_f64).map(|v| v as f32).unwrap_or(d);
                let size = window.viewport_size();
                window.resize(gpui::size(gpui::px(n("width", n("w", f32::from(size.width)))), gpui::px(n("height", n("h", f32::from(size.height))))));
            }
            "window.close" => window.remove_window(),
            "app.hide" => cx.hide(),
            "app.activate" => {
                cx.activate(true);
                window.activate_window();
            }
            "app.quit" => cx.quit(),
            "chat.show" => {
                if let Some(id) = chat() {
                    self.show_chat(&id, window, cx);
                }
            }
            "focus" => self.focus_area_action(s("area").as_deref().unwrap_or("composer"), chat(), window, cx),
            "composer.accept_suggestion" => {
                if let Some(id) = chat() {
                    self.ensure_composer(&id, window, cx);
                    self.on_tab_now(&id, window, cx);
                }
            }
            "chat.attach_clipboard" => {
                if let (Some(id), Some(item)) = (chat(), cx.read_from_clipboard()) {
                    let files = crate::workspace::attachments_from_clipboard(&item);
                    if !files.is_empty() {
                        self.attach(id, files, window, cx);
                    }
                }
            }
            "composer.recall" => {
                if let Some(id) = chat() {
                    self.recall_sent(&id, a.get("by").and_then(Value::as_i64).unwrap_or(-1), window, cx);
                }
            }
            other if self.scripts.as_ref().is_some_and(|sc| sc.has_action(other)) => {
                let msg = crate::script::action_message(other, a);
                self.script_deliver(msg, window, cx);
            }
            // Actions that live with the views they drive (transcript steps, the diff, terminals).
            other if crate::ui::trace_view::dispatch(self, other, a, window, cx) => {}
            other if crate::ui::diff_view::dispatch(self, other, a, window, cx) => {}
            other if crate::terminal::dispatch(self, other, a, window, cx) => {}
            other if crate::ui::drive::dispatch(self, other, a, window, cx) => {}
            other => self.exprs.note(format!("unknown action `{other}`")),
        }
        self.pending_motion = true;
        self.ctx_rev += 1;
        cx.notify();
    }
}

/** A git remote's host and repo path: `git@host:group/repo.git`, `ssh://git@host:22/group/repo.git`, or
`https://host/group/repo.git`. */
fn remote_parts(remote: &str) -> Option<(String, String)> {
    let r = remote.trim().trim_end_matches(".git");
    if r.is_empty() {
        return None;
    }
    if let Some(rest) = r.strip_prefix("git@") {
        let (host, path) = rest.split_once(':')?;
        return Some((host.to_string(), path.to_string()));
    }
    if let Some(rest) = r.strip_prefix("ssh://") {
        let rest = rest.split_once('@').map(|x| x.1).unwrap_or(rest);
        let (host, path) = rest.split_once('/')?;
        return Some((host.split(':').next().unwrap_or(host).to_string(), path.to_string()));
    }
    let rest = r.strip_prefix("https://").or_else(|| r.strip_prefix("http://"))?;
    let rest = rest.split_once('@').map(|x| x.1).unwrap_or(rest);
    let (host, path) = rest.split_once('/')?;
    Some((host.to_string(), path.to_string()))
}

/** The browser page for a git remote, by `template` ({host}, {path}). */
fn web_url(remote: &str, template: &str) -> Option<String> {
    let (host, path) = remote_parts(remote)?;
    Some(template.replace("{host}", &host).replace("{path}", &path))
}

/** Moves a worktree into a section (None: its project) before another, in a model. */
pub(crate) fn move_worktree_in(m: &mut agent_box_protocol::Model, path: &str, section: Option<&str>, before: Option<&str>) {
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

#[cfg(test)]
mod tests {
    #[test]
    fn web_urls_from_remotes() {
        let t = "https://{host}/{path}";
        assert_eq!(super::web_url("git@gitlab.com:redo/redoapp/product/redo.git\n", t).as_deref(), Some("https://gitlab.com/redo/redoapp/product/redo"));
        assert_eq!(super::web_url("ssh://git@gitlab.com:22/tj15/a.box.git", t).as_deref(), Some("https://gitlab.com/tj15/a.box"));
        assert_eq!(super::web_url("https://gitlab.com/tj15/a.box.git", t).as_deref(), Some("https://gitlab.com/tj15/a.box"));
        assert_eq!(super::web_url("git@git.corp:team/app.git", "https://code.corp/{path}/-/tree/main").as_deref(), Some("https://code.corp/team/app/-/tree/main"));
        assert_eq!(super::web_url("", t), None);
    }
}
