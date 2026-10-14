/*! Actions: what clicks, keys, menus, and palette entries do. An action is a name (`"palette.open"`),
an object `{"do": name, ...args}`, or a list of actions run in order. */

use super::expr::{to_val, with};
use crate::workspace::Workspace;
use agent_box_protocol::Call;
use gpui_kit::*;
use serde_json::Value;

pub const ACTIONS: &[(&str, &str)] = &[
    ("state.set", "{key, value}: set a state value (value may be any JSON or a template)"),
    ("state.toggle", "{key, default?}: flip a boolean state value (default: its value when unset)"),
    ("state.cycle", "{key, values: [..]}: move a state value to the next item in values"),
    ("select", "{worktree?: path, chat?: id, offset?: n}: select a worktree and/or one of its chats; offset moves n worktrees down (negative: up)"),
    ("chat.new", "{dir?, repo?, name?, kind?, branch?, base?, prompt?}: start a chat at once; no dir = a fresh worktree in repo"),
    ("chat.send", "{chat, text}: send a message"),
    ("chat.keys", "{chat, keys: [..]}: press keys in the chat's session (Enter, Escape, Up, 1, ...)"),
    ("chat.archive / chat.stop / chat.start", "{chat}"),
    ("chat.attach", "{chat, paths: [..]}: attach files to a chat (preview chips above its composer; uploaded to the box for box chats; sent with the next message). An `on.drop` action gets the dropped files as `paths`."),
    ("composer.send", "{chat}: send what's in a chat's composer"),
    ("chat.move", "{chat, to: mac|box}: move a chat between machines"),
    ("section.new", "{assign?: path}: ask for a name, create the section (and move that worktree into it)"),
    ("section.rename", "{id, name?}: rename (asks when name is missing)"),
    ("section.delete", "{id}"),
    ("section.move", "{id?, index? | target: section id with zone}: reorder a section (id defaults to `dragged`)"),
    ("worktree.move", "{path?, section?: id|null, before?: path | target: path with zone: before|after}: move a worktree into a section (null: under its project), next to another or last (path defaults to `dragged`; drops supply `zone`)"),
    ("project.move", "{repo?, index? | target: repo with zone}: reorder a project (repo defaults to `dragged`)"),
    ("section.assign", "{path, section}: move a worktree into a section (null = out of it)"),
    ("worktree.delete", "{path}"),
    ("worktree.test_local", "{branch}: check the branch out on the Mac with the box's latest work"),
    ("prompt", "{title, placeholder?, value?, then}: ask for text; `then` runs with it as `.input`"),
    ("palette.open", "the command palette"),
    ("steps.expand_all / steps.collapse_all", "{chat?}: open or close every step line in a chat's transcript"),
    ("ui.click", "{id?, label?}: click a clickable node as if by hand: exact node id, else the first whose id or tooltip contains `label` (inspect.json lists them)"),
    ("composer.set / composer.type", "{chat?, text}: replace or add to the text in a chat's message box (the designer's is chat `designer`)"),
    ("js", "{code, ...args}: run JavaScript with `args`, `ctx` (the context), and `ab` (see Scripts)"),
    ("<a script action>", "any name a script registers with ab.action(name, fn) works as an action everywhere"),
    ("app.snapshot", "capture the window to snapshot.png in the settings folder (for agents to look at)"),
    ("drop.preview", "{key, zone: before|inside|after, kind?}: show a drop target's indicator without dragging (for styling); cleared by the next drag"),
    ("diff.refresh", "reload the selected worktree's diff now"),
    ("diff.collapse_all / diff.expand_all", "collapse or expand every file in the diff"),
    ("designer.send", "{text}: ask the designer (a local agent that edits these settings live) for a change"),
    ("designer.reset", "end the designer conversation and start fresh"),
    ("settings.undo", "put the settings file back as it was before the last designer request (repeatable)"),
    ("settings.open / settings.reload / settings.docs", "edit, reload, or read the settings reference"),
    ("notify", "{text}: a notification inside the window"),
    ("notify.system", "{title, text, chat?, sound?: bool}: a macOS notification banner; clicking it brings the window up on `chat`"),
    ("dock.bounce", "bounce the Dock icon once"),
    ("open", "{url}: open a URL or app link"),
    ("shell", "{run, env?: {NAME: value}, notify?: bool}: run a shell command on the Mac (pass user text through env, not into run); notify shows its last output line"),
    ("settings.agent", "{request}: a local Claude Code agent reads SETTINGS.md and edits your settings to do what request says; the app reloads when it saves"),
    ("window.move / window.zoom / app.quit", ""),
];

/** A one-shot local Claude Code run in the settings folder; it may only read and edit files there. */
const SETTINGS_AGENT: &str = r#"cd "$HOME/.agent-box/state/settings" && claude -p "You are customizing the Agent Box app for its user. Read SETTINGS.md in this folder first: it documents every setting and holds the full defaults. Then edit settings.jsonc (only that file) to do what the user asks, keeping their existing overrides. The app reloads on save; problems show up in its settings panel, so keep the file valid JSON with comments. When done, reply with one short sentence saying what you changed. The user's request: $REQUEST" --permission-mode acceptEdits --allowedTools Read Edit Write Glob Grep 2>&1 | tail -n 1"#;

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
                let call = Call::ChatNew {
                    name: s("name").unwrap_or("New chat".into()),
                    kind: s("kind").unwrap_or(if s("dir").is_some() && s("branch").is_none() { "primary".into() } else { "orchestrator".into() }),
                    branch: s("branch"),
                    base: s("base"),
                    dir: s("dir"),
                    repo: s("repo"),
                    prompt: s("prompt"),
                    model: s("model"),
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
                    self.run_local(vec!["chat".into(), "move".into(), id, "--to".into(), to], window, cx);
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
            "ui.click" => {
                let (id, label) = (s("id"), s("label").map(|l| l.to_lowercase()));
                let hit = self
                    .clickables
                    .iter()
                    .find(|(nid, _, _)| id.as_deref() == Some(nid.as_str()))
                    .or_else(|| {
                        let l = label.as_deref()?;
                        self.clickables.iter().find(|(nid, tip, _)| nid.to_lowercase().contains(l) || tip.to_lowercase().contains(l))
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
                }
            }
            "drop.preview" => {
                self.drop_hint = s("key").map(|key| crate::ui::render::DropHint { key, zone: s("zone").unwrap_or("inside".into()), kind: s("kind").unwrap_or_default() });
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
            "settings.undo" => {
                use gpui_kit::component::WindowExt as _;
                if !crate::designer::undo() {
                    window.push_notification("Nothing to undo", cx);
                }
            }
            "settings.open" => self.open_settings(window, cx),
            "settings.reload" => self.reload_settings(cx),
            "settings.docs" => {
                let _ = std::process::Command::new("open").arg(crate::settings::dir().join("SETTINGS.md")).spawn();
            }
            "notify" => {
                use gpui_kit::component::WindowExt as _;
                window.push_notification(s("text").unwrap_or_default(), cx);
            }
            "notify.system" => {
                self.notify_count += 1;
                let id = format!("{}#{}", s("chat").unwrap_or_default(), self.notify_count);
                #[cfg(target_os = "macos")]
                crate::macos::notify(&id, &s("title").unwrap_or("Agent Box".into()), &s("text").unwrap_or_default(), a.get("sound").and_then(Value::as_bool) != Some(false));
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
            other => self.exprs.note(format!("unknown action `{other}`")),
        }
        self.pending_motion = true;
        cx.notify();
    }
}
