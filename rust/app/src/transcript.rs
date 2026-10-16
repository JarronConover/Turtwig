/*! Claude Code session files, turned into the items a chat view draws. One file line can hold several
content blocks; tool results are attached to the call they answer. */

use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/** How session lines turn into items, from settings `chat.show`, `chat.labels`, `chat.tools`,
`chat.tool_names`, `chat.pending`, and `chat.summary_chars`; `set_options` applies them (to lines read
after, not ones already shown). */
#[derive(Clone, Debug)]
pub struct Options {
    /** Lines Claude Code adds for the model (isMeta: expanded skills, caveats), as notices. */
    pub meta: bool,
    /** Subagent lines inside the parent's transcript. */
    pub sidechain: bool,
    /** Harness wrappers (command output, system reminders), as notices. */
    pub wrappers: bool,
    /** The compaction summary: "step" (a collapsed line), "message" (a user bubble), or "hidden". */
    pub compaction: String,
    /** Messages from other Claude sessions: "step", "message", or "hidden". */
    pub agent_messages: String,
    /** Background-task notifications: "notice", "message", or "hidden". */
    pub task_notifications: String,
    /** Claude Code's error lines as notices. */
    pub errors: bool,
    /** The "Thinking"/"Working" row while a chat works. */
    pub pending: bool,
    pub summary_chars: usize,
    pub labels: Value,
    pub tools: Value,
    pub tool_names: Value,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            meta: false,
            sidechain: false,
            wrappers: false,
            compaction: "step".into(),
            agent_messages: "step".into(),
            task_notifications: "notice".into(),
            errors: true,
            pending: true,
            summary_chars: 140,
            labels: Value::Null,
            tools: Value::Null,
            tool_names: Value::Null,
        }
    }
}

thread_local! {
    static OPTIONS: std::cell::RefCell<Rc<Options>> = std::cell::RefCell::new(Rc::new(Options::default()));
}

/** Reads `chat.show`, `chat.labels`, `chat.tools`, `chat.tool_names`, `chat.pending`, `chat.summary_chars`. */
pub fn set_options(chat: &Value) {
    let d = Options::default();
    let show = &chat["show"];
    let b = |k: &str, d: bool| show.get(k).and_then(Value::as_bool).unwrap_or(d);
    let t = |k: &str, d: &str| show.get(k).and_then(Value::as_str).unwrap_or(d).to_string();
    let o = Options {
        meta: b("meta", d.meta),
        sidechain: b("sidechain", d.sidechain),
        wrappers: b("wrappers", d.wrappers),
        compaction: t("compaction", &d.compaction),
        agent_messages: t("agent_messages", &d.agent_messages),
        task_notifications: t("task_notifications", &d.task_notifications),
        errors: b("errors", d.errors),
        pending: chat["pending"].get("show").and_then(Value::as_bool).unwrap_or(d.pending),
        summary_chars: chat.get("summary_chars").and_then(Value::as_u64).map(|n| n as usize).unwrap_or(d.summary_chars),
        labels: chat.get("labels").cloned().unwrap_or(Value::Null),
        tools: chat.get("tools").cloned().unwrap_or(Value::Null),
        tool_names: chat.get("tool_names").cloned().unwrap_or(Value::Null),
    };
    OPTIONS.with(|c| *c.borrow_mut() = Rc::new(o));
}

pub fn options() -> Rc<Options> {
    OPTIONS.with(|c| c.borrow().clone())
}

/** A transcript label from `chat.labels` (else `default`), with `{name}` placeholders filled. */
pub fn label(key: &str, default: &str, vars: &[(&str, &str)]) -> String {
    let o = options();
    let mut s = o.labels.get(key).and_then(Value::as_str).unwrap_or(default).to_string();
    for (k, v) in vars {
        s = s.replace(&format!("{{{k}}}"), v);
    }
    s
}

/** How a tool's step reads: prefixes while running, done, and failed (the subject follows them), its icon,
and the verb in a working chat's one-line activity. `chat.tools.NAME` (or `chat.tools["*"]` for tools
with no entry) overrides any of these. */
#[derive(Clone, Debug, PartialEq)]
pub struct ToolLook {
    pub doing: String,
    pub done: String,
    pub failed: String,
    pub icon: String,
    pub activity: String,
}

pub fn tool_look(name: &str) -> ToolLook {
    let (doing, done, failed, icon, activity) = match name {
        "Read" => ("Reading", "Read", "Couldn't read", "file-text", "Reading"),
        "Edit" | "MultiEdit" => ("Editing", "Edited", "Couldn't edit", "pencil", "Editing"),
        "Write" => ("Writing", "Wrote", "Couldn't write", "file-plus", "Editing"),
        "NotebookEdit" => ("Editing", "Edited", "Couldn't edit", "notebook-pen", "Editing"),
        "Bash" => ("", "", "Failed:", "square-terminal", "Running"),
        "Grep" => ("Searching for", "Searched for", "Couldn't search for", "search", "Searching for"),
        "Glob" => ("Finding", "Found", "Couldn't find", "folder-search", "Searching for"),
        "WebFetch" => ("Fetching", "Fetched", "Couldn't fetch", "globe", "Fetching"),
        "WebSearch" => ("Searching the web for", "Searched the web for", "Couldn't search the web for", "search", "Searching the web for"),
        "Agent" | "Task" => ("Delegating:", "Delegated:", "Delegation failed:", "bot", "Delegating:"),
        "TodoWrite" => ("Updating the plan", "Updated the plan", "Couldn't update the plan", "list-todo", "TodoWrite"),
        "Skill" => ("Loading", "Loaded", "Couldn't load", "sparkles", "Skill"),
        "Compaction" => ("", "", "", "fold-vertical", "Compaction"),
        "AgentMessage" => ("", "", "", "message-square", "AgentMessage"),
        _ => ("", "", "Failed:", "wrench", name),
    };
    let o = options();
    let over = o.tools.get(name).or_else(|| o.tools.get("*")).cloned().unwrap_or(Value::Null);
    let pick = |k: &str, d: &str| over.get(k).and_then(Value::as_str).unwrap_or(d).to_string();
    ToolLook { doing: pick("doing", doing), done: pick("done", done), failed: pick("failed", failed), icon: pick("icon", icon), activity: pick("activity", activity) }
}

/** A tool's name as shown when it has no entry: `mcp__server__tool` as "server · tool"
(`chat.tool_names`: `strip` prefix, `separator`). */
pub fn tool_display_name(name: &str) -> String {
    let o = options();
    let strip = o.tool_names.get("strip").and_then(Value::as_str).unwrap_or("mcp__");
    let sep = o.tool_names.get("separator").and_then(Value::as_str).unwrap_or(" · ");
    name.trim_start_matches(strip).replace("__", sep)
}

#[derive(Debug, Clone)]
pub enum Item {
    User { text: String },
    /** An image the user attached (base64, as Claude Code stores it). */
    Image { media_type: String, data: String },
    Assistant { text: String },
    Thinking { text: String },
    Tool { id: String, name: String, summary: String, input: Value, result: Option<String>, is_error: bool },
    Notice { text: String },
}

#[derive(Default)]
pub struct Transcript {
    pub lines: usize,
    /** The first file line loaded (long chats load only their newest lines at first). */
    pub start: usize,
    pub items: Vec<Item>,
    tool_index: HashMap<String, usize>,
    /** How the newest compaction started ("auto" or "manual"), from its boundary line. */
    compact_trigger: Option<String>,
    /** Bumped on every change so views can tell new content from a re-render. */
    pub revision: u64,
    /** Tool results whose call is in lines not loaded yet (an older page attaches them). */
    orphans: HashMap<String, (String, bool)>,
    /** Messages just sent, shown right away until the file has them: (text, item index). */
    echoes: Vec<(String, usize)>,
    /** Background shells and agents that have finished: their tool_use ids and task ids. */
    finished: HashSet<String>,
    /** A subagent's own transcript: every line is a sidechain, so they're kept. */
    keep_sidechain: bool,
}

/** Something a chat started that's still running: a background shell or a subagent. */
#[derive(Debug, Clone, PartialEq)]
pub struct Background {
    /** "shell" or "agent". */
    pub kind: &'static str,
    pub label: String,
    /** The tool_use id that started it. */
    pub tool_id: String,
    /** The shell's task id, or the agent's id once known. */
    pub task_id: Option<String>,
    /** Where a background shell writes its output. */
    pub output: Option<String>,
}

impl Transcript {
    /** A short line on what the agent is doing now, from the newest item: "Reading settings.jsonc"
    (verbs: `chat.tools.NAME.activity`; `chat.labels.activity_thinking` / `activity_writing`). */
    pub fn activity(&self) -> String {
        match self.items.last() {
            Some(Item::Tool { name, summary, result: None, .. }) => format!("{} {summary}", tool_look(name).activity),
            Some(Item::Tool { .. }) | Some(Item::User { .. }) | Some(Item::Image { .. }) | Some(Item::Thinking { .. }) => label("activity_thinking", "Thinking", &[]),
            Some(Item::Assistant { .. }) => label("activity_writing", "Writing", &[]),
            _ => String::new(),
        }
    }

    pub fn append(&mut self, from: usize, entries: &[Value]) {
        if from != self.lines {
            if from < self.lines {
                return;
            }
            if self.lines == 0 {
                self.start = from;
            }
            self.lines = from;
        }
        for e in entries {
            self.lines += 1;
            self.push_entry(e);
        }
        self.revision += 1;
    }

    /** An empty transcript for a chat id (`sub:...` ids are subagents). */
    pub fn for_chat(id: &str) -> Self {
        Transcript { keep_sidechain: id.starts_with("sub:"), ..Default::default() }
    }

    /** Tool results still waiting for their call (in lines not loaded yet). */
    pub fn orphan_count(&self) -> usize {
        self.orphans.len()
    }

    /** How many finished task and tool ids it has seen (for inspect.json). */
    pub fn finished_count(&self) -> usize {
        self.finished.len()
    }

    /** Background shells and subagents started in the loaded lines that haven't finished. */
    pub fn background(&self) -> Vec<Background> {
        let mut out = vec![];
        for it in &self.items {
            let Item::Tool { id, name, summary, input, result, .. } = it else { continue };
            let bg = input.get("run_in_background").and_then(Value::as_bool) == Some(true);
            let done = |task: &Option<String>| self.finished.contains(id) || task.as_ref().is_some_and(|t| self.finished.contains(t));
            match name.as_str() {
                "Bash" if bg => {
                    let Some(r) = result else { continue };
                    let task = after(r, "running in background with ID: ").map(|s| word(s).trim_end_matches('.').to_string());
                    let output = after(r, "Output is being written to: ").map(|s| word(s).trim_end_matches('.').to_string());
                    if task.is_some() && !done(&task) {
                        out.push(Background { kind: "shell", label: summary.clone(), tool_id: id.clone(), task_id: task, output });
                    }
                }
                "Agent" | "Task" => {
                    let task = result.as_deref().and_then(|r| after(r, "agentId: ")).map(|s| word(s).to_string());
                    let launched = result.as_deref().is_some_and(|r| r.contains("agent launched") || r.contains("working in the background"));
                    let running = result.is_none() || ((bg || launched) && task.is_some());
                    if running && !done(&task) {
                        let kind = input.get("subagent_type").and_then(Value::as_str).filter(|t| *t != "general-purpose");
                        let label = match kind {
                            Some(k) => format!("{k}: {summary}"),
                            None => summary.clone(),
                        };
                        out.push(Background { kind: "agent", label, tool_id: id.clone(), task_id: task, output: None });
                    }
                }
                _ => {}
            }
        }
        out
    }

    /** Notes background work that finished, from a <task-notification> (in a queued or a delivered message). */
    fn note_finished(&mut self, text: &str) {
        let mut rest = text;
        while let Some(i) = rest.find("<task-notification>") {
            let block = &rest[i..];
            let end = block.find("</task-notification>").unwrap_or(block.len());
            let block = &block[..end];
            for tag in ["task-id", "tool-use-id"] {
                if let Some(v) = between(block, &format!("<{tag}>"), &format!("</{tag}>")) {
                    self.finished.insert(v.to_string());
                }
            }
            rest = &rest[i + end.max(1)..];
        }
    }

    /** Shows a message you just sent before Claude Code writes it to the file (which takes a moment). */
    pub fn echo(&mut self, text: &str) {
        self.echoes.push((text.trim().to_string(), self.items.len()));
        self.items.push(Item::User { text: text.trim().to_string() });
        self.revision += 1;
    }

    /** Swaps the echo of `text` (or else the oldest echo) out for the real message about to be added. */
    fn settle_echo(&mut self, text: &str) {
        if self.echoes.is_empty() {
            return;
        }
        let k = self.echoes.iter().position(|(t, _)| t == text).unwrap_or(0);
        let (_, at) = self.echoes.remove(k);
        if at < self.items.len() {
            self.items.remove(at);
            for i in self.tool_index.values_mut().chain(self.echoes.iter_mut().map(|e| &mut e.1)) {
                if *i > at {
                    *i -= 1;
                }
            }
        }
    }

    /** Adds the older lines `from..self.start` in front. */
    pub fn prepend(&mut self, from: usize, entries: &[Value]) {
        let mut older = Transcript { lines: from, start: from, keep_sidechain: self.keep_sidechain, ..Default::default() };
        for e in entries {
            older.lines += 1;
            older.push_entry(e);
        }
        for (id, (text, err)) in std::mem::take(&mut self.orphans) {
            match older.tool_index.get(&id).and_then(|&i| older.items.get_mut(i)) {
                Some(Item::Tool { result, is_error, .. }) => {
                    *result = Some(text);
                    *is_error = err;
                }
                _ => {
                    older.orphans.insert(id, (text, err));
                }
            }
        }
        let shift = older.items.len();
        for i in self.tool_index.values_mut().chain(self.echoes.iter_mut().map(|e| &mut e.1)) {
            *i += shift;
        }
        self.tool_index.extend(older.tool_index);
        older.items.append(&mut self.items);
        self.items = older.items;
        self.orphans = older.orphans;
        // Background work that finished within the older lines (its notice can sit right after the call).
        self.finished.extend(older.finished);
        self.start = from;
        self.revision += 1;
    }

    fn push_entry(&mut self, e: &Value) {
        let opts = options();
        if !self.keep_sidechain && !opts.sidechain && e.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            return;
        }
        let kind = e.get("type").and_then(Value::as_str).unwrap_or("");
        if kind == "queue-operation" {
            if let Some(c) = e.get("content").and_then(Value::as_str) {
                self.note_finished(c);
            }
            return;
        }
        let content = &e["message"]["content"];
        // Text Claude Code adds for the model (a slash command's expanded skill, caveats): never typed.
        if kind == "user" && e.get("isMeta").and_then(Value::as_bool) == Some(true) {
            if opts.meta {
                let text = content.as_str().map(str::to_string).unwrap_or_else(|| {
                    content.as_array().into_iter().flatten().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n")
                });
                if !text.trim().is_empty() {
                    self.items.push(Item::Notice { text });
                }
            }
            return;
        }
        if kind == "system" && e.get("subtype").and_then(Value::as_str) == Some("compact_boundary") {
            self.compact_trigger = e["compactMetadata"]["trigger"].as_str().map(str::to_string);
            return;
        }
        // The summary Claude Code writes when it compacts: a collapsed step, not something you typed.
        if kind == "user" && e.get("isCompactSummary").and_then(Value::as_bool) == Some(true) {
            let text = content.as_str().map(str::to_string).unwrap_or_else(|| {
                content.as_array().into_iter().flatten().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n")
            });
            match opts.compaction.as_str() {
                "hidden" => return,
                "message" => {
                    self.items.push(Item::User { text });
                    return;
                }
                _ => {}
            }
            self.items.push(Item::Tool {
                id: format!("compact:{}", self.lines),
                name: "Compaction".into(),
                summary: String::new(),
                input: serde_json::json!({ "trigger": self.compact_trigger.take() }),
                result: Some(text),
                is_error: false,
            });
            return;
        }
        match kind {
            "user" => {
                if let Some(text) = content.as_str() {
                    self.user_text(text);
                } else if let Some(blocks) = content.as_array() {
                    for b in blocks {
                        match b.get("type").and_then(Value::as_str) {
                            Some("text") => self.user_text(b["text"].as_str().unwrap_or("")),
                            Some("image") => {
                                if let Some(data) = b["source"]["data"].as_str() {
                                    let media_type = b["source"]["media_type"].as_str().unwrap_or("image/png").to_string();
                                    self.items.push(Item::Image { media_type, data: data.to_string() });
                                }
                            }
                            Some("tool_result") => self.tool_result(b),
                            _ => {}
                        }
                    }
                }
            }
            "assistant" => {
                for b in content.as_array().into_iter().flatten() {
                    match b.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            let text = b["text"].as_str().unwrap_or("").to_string();
                            if !text.trim().is_empty() {
                                self.items.push(Item::Assistant { text });
                            }
                        }
                        Some("thinking") => {
                            let text = b["thinking"].as_str().unwrap_or("").to_string();
                            if !text.trim().is_empty() {
                                self.items.push(Item::Thinking { text });
                            }
                        }
                        Some("tool_use") => {
                            let id = b["id"].as_str().unwrap_or("").to_string();
                            let name = b["name"].as_str().unwrap_or("tool").to_string();
                            let input = b["input"].clone();
                            if matches!(name.as_str(), "TaskStop" | "KillShell" | "KillBash") {
                                for k in ["task_id", "shell_id", "bash_id", "id"] {
                                    if let Some(v) = input.get(k).and_then(Value::as_str) {
                                        self.finished.insert(v.to_string());
                                    }
                                }
                            }
                            self.tool_index.insert(id.clone(), self.items.len());
                            self.items.push(Item::Tool {
                                summary: summarize(&name, &input),
                                id,
                                name,
                                input,
                                result: None,
                                is_error: false,
                            });
                        }
                        _ => {}
                    }
                }
            }
            "system" => {
                if let Some(t) = e.get("content").and_then(Value::as_str) {
                    if opts.errors && e.get("level").and_then(Value::as_str) == Some("error") {
                        self.items.push(Item::Notice { text: t.to_string() });
                    }
                }
            }
            _ => {}
        }
    }

    fn user_text(&mut self, text: &str) {
        let t = text.trim();
        // A slash command shows as what was typed ("/loop /ship-feature X"), not its markup.
        if let Some(name) = between(t, "<command-name>", "</command-name>") {
            let args = between(t, "<command-args>", "</command-args>").unwrap_or("");
            let text = format!("{name} {args}").trim().to_string();
            self.settle_echo(&text);
            self.items.push(Item::User { text });
            return;
        }
        let opts = options();
        // Another Claude session talking to this one (SendMessage, teammates): a collapsed step, not you.
        if t.contains("<teammate-message") || t.contains("<cross-session-message") || t.starts_with("Another Claude session sent a message") {
            match opts.agent_messages.as_str() {
                "hidden" => return,
                "message" => {
                    self.items.push(Item::User { text: t.to_string() });
                    return;
                }
                _ => {}
            }
            for (from, summary, body) in agent_messages(t) {
                let id = format!("agent-msg:{}", self.lines);
                self.items.push(Item::Tool {
                    summary: summary.clone(),
                    id,
                    name: "AgentMessage".into(),
                    input: serde_json::json!({ "from": from }),
                    result: Some(body),
                    is_error: false,
                });
            }
            return;
        }
        if t.contains("<task-notification>") {
            self.note_finished(t);
        }
        if t.starts_with("<task-notification>") {
            let fallback = label("background_finished", "A background task finished", &[]);
            let summary = between(t, "<summary>", "</summary>").map(str::to_string).unwrap_or(fallback);
            match opts.task_notifications.as_str() {
                "hidden" => {}
                "message" => self.items.push(Item::User { text: t.to_string() }),
                _ => self.items.push(Item::Notice { text: summary }),
            }
            return;
        }
        // Harness wrappers (command output, reminders) aren't things the user typed.
        if t.is_empty() {
            return;
        }
        if t.starts_with("<command-") || t.starts_with("<local-command") || t.starts_with("<system-reminder") {
            if opts.wrappers {
                self.items.push(Item::Notice { text: t.to_string() });
            }
            return;
        }
        self.settle_echo(t);
        self.items.push(Item::User { text: t.to_string() });
    }

    fn tool_result(&mut self, b: &Value) {
        let id = b["tool_use_id"].as_str().unwrap_or("");
        let text = match &b["content"] {
            Value::String(s) => s.clone(),
            Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n"),
            _ => String::new(),
        };
        let err = b["is_error"].as_bool().unwrap_or(false);
        match self.tool_index.get(id).and_then(|&i| self.items.get_mut(i)) {
            Some(Item::Tool { result, is_error, .. }) => {
                *result = Some(text);
                *is_error = err;
            }
            _ => {
                self.orphans.insert(id.to_string(), (text, err));
            }
        }
    }
}

fn summarize(name: &str, input: &Value) -> String {
    let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let short = |p: String| p.rsplit('/').next().unwrap_or(&p).to_string();
    let text = match name {
        "Bash" => s("description").is_empty().then(|| s("command")).unwrap_or_else(|| s("description")),
        "Read" | "Write" | "Edit" | "NotebookEdit" => short(s("file_path")),
        "Grep" | "Glob" => s("pattern"),
        "WebFetch" => s("url"),
        "WebSearch" => s("query"),
        "Agent" | "Task" => s("description"),
        "Skill" => s("skill"),
        "TodoWrite" => "update the todo list".to_string(),
        _ => input
            .as_object()
            .and_then(|o| o.values().find_map(|v| v.as_str().map(str::to_string)))
            .unwrap_or_default(),
    };
    text.lines().next().unwrap_or("").chars().take(options().summary_chars).collect()
}

fn after<'a>(text: &'a str, marker: &str) -> Option<&'a str> {
    text.find(marker).map(|i| &text[i + marker.len()..])
}

fn word(s: &str) -> &str {
    s.split(|c: char| c.is_whitespace() || c == ')' || c == ',').next().unwrap_or("")
}

fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = text.find(open)? + open.len();
    let end = text[start..].find(close)? + start;
    Some(text[start..end].trim())
}

/** (from, summary, body) for each `<teammate-message teammate_id=.. summary=..>body</teammate-message>`
(or cross-session message) in a user turn; idle notices with a JSON body use its `result`. */
fn agent_messages(text: &str) -> Vec<(String, String, String)> {
    let mut out = vec![];
    for tag in ["teammate-message", "cross-session-message"] {
        let open = format!("<{tag}");
        let close = format!("</{tag}>");
        let mut rest = text;
        while let Some(start) = rest.find(&open) {
            let after = &rest[start..];
            let Some(head_end) = after.find('>') else { break };
            let head = &after[..head_end];
            let attr = |k: &str| {
                let key = format!("{k}=\"");
                head.find(&key).and_then(|i| head[i + key.len()..].split('"').next()).unwrap_or("").to_string()
            };
            let body_start = head_end + 1;
            let body_end = after.find(&close).unwrap_or(after.len());
            let mut body = after[body_start..body_end.max(body_start)].trim().to_string();
            let mut summary = attr("summary");
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                if let Some(r) = v.get("result").and_then(|x| x.as_str()) {
                    if summary.is_empty() {
                        summary = v.get("type").and_then(|x| x.as_str()).unwrap_or("update").replace('_', " ");
                    }
                    body = r.to_string();
                }
            }
            let from = [attr("teammate_id"), attr("from"), attr("session")].into_iter().find(|x| !x.is_empty()).unwrap_or_else(|| label("another_session", "another session", &[]));
            out.push((from, summary, body));
            rest = &after[(body_end + close.len()).min(after.len())..];
        }
    }
    if out.is_empty() {
        out.push((label("another_session", "another session", &[]), String::new(), text.to_string()));
    }
    out
}

#[cfg(test)]
mod agent_message_tests {
    #[test]
    fn reads_teammate_messages() {
        let t = "Another Claude session sent a message:\n<teammate-message teammate_id=\"box-migrate\" color=\"blue\" summary=\"Done\">\nAll good.\n</teammate-message>\n\n<teammate-message teammate_id=\"box-migrate\">\n{\"type\":\"idle_notification\",\"result\":\"Idle now\"}\n</teammate-message>\nThis came from another Claude session";
        let m = super::agent_messages(t);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0], ("box-migrate".to_string(), "Done".to_string(), "All good.".to_string()));
        assert_eq!(m[1].1, "idle notification");
        assert_eq!(m[1].2, "Idle now");
    }
}

#[cfg(test)]
mod compaction_tests {
    use super::{Item, Transcript};
    use serde_json::json;

    #[test]
    fn a_compaction_summary_is_a_step_not_a_user_message() {
        let mut t = Transcript::default();
        t.append(0, &[
            json!({"type": "system", "subtype": "compact_boundary", "content": "Conversation compacted", "compactMetadata": {"trigger": "auto"}}),
            json!({"type": "user", "isCompactSummary": true, "message": {"role": "user", "content": "This session is being continued…\n\nSummary: x"}}),
            json!({"type": "user", "message": {"role": "user", "content": "next question"}}),
        ]);
        assert_eq!(t.items.len(), 2);
        match &t.items[0] {
            Item::Tool { name, input, result, .. } => {
                assert_eq!(name, "Compaction");
                assert_eq!(input["trigger"], "auto");
                assert!(result.as_deref().unwrap_or("").contains("Summary: x"));
            }
            other => panic!("expected a step, got {other:?}"),
        }
        assert!(matches!(&t.items[1], Item::User { text } if text == "next question"));
    }
}

#[cfg(test)]
mod paging_tests {
    use super::Transcript;
    use serde_json::json;

    #[test]
    fn remembers_where_a_tail_load_started() {
        let line = json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "hi"}]}});
        let mut t = Transcript::default();
        t.append(1200, &[line.clone(), line.clone()]);
        assert_eq!((t.start, t.lines), (1200, 1202));
        t.append(1202, &[line]);
        assert_eq!((t.start, t.lines), (1200, 1203));
    }

    #[test]
    fn a_sent_message_shows_at_once_and_is_replaced_by_the_real_one() {
        let mut t = Transcript::default();
        t.append(0, &[json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "hi"}]}})]);
        t.echo("fix it");
        assert_eq!(t.items.len(), 2);
        t.append(1, &[json!({"type": "user", "message": {"content": "fix it"}})]);
        assert_eq!(t.items.len(), 2);
        assert!(t.echoes.is_empty());
    }

    #[test]
    fn work_that_finished_in_an_older_page_stays_finished() {
        let launch = json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": "t7", "name": "Agent", "input": {"description": "Look"}}]}});
        let launched = json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "t7", "content": "Async agent launched successfully.\nagentId: a1 (internal)"}]}});
        let done = json!({"type": "queue-operation", "operation": "enqueue", "content": "<task-notification>\n<task-id>a1</task-id>\n<tool-use-id>t7</tool-use-id>\n</task-notification>"});
        let later = json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "ok"}]}});
        let mut t = Transcript::default();
        t.append(3, &[later]);
        t.prepend(0, &[launch, launched, done]);
        assert!(t.background().is_empty());
    }

    #[test]
    fn an_older_page_goes_in_front_and_claims_its_results() {
        let call = json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "ls"}}]}});
        let result = json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "t1", "content": "a.txt"}]}});
        let said = json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "done"}]}});
        let mut t = Transcript::default();
        t.append(10, &[result, said]);
        t.prepend(9, &[call]);
        assert_eq!((t.start, t.lines), (9, 12));
        assert!(matches!(&t.items[0], super::Item::Tool { result: Some(r), .. } if r == "a.txt"));
        assert!(matches!(&t.items[1], super::Item::Assistant { text } if text == "done"));
    }
}

#[cfg(test)]
mod background_tests {
    use super::Transcript;
    use serde_json::json;

    #[test]
    fn running_shells_and_agents_until_they_finish() {
        let mut t = Transcript::default();
        t.append(0, &[
            json!({"type": "assistant", "message": {"content": [
                {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "make", "description": "Build", "run_in_background": true}},
                {"type": "tool_use", "id": "t2", "name": "Agent", "input": {"description": "Look around", "prompt": "x"}}
            ]}}),
            json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "t1", "content": "Command running in background with ID: b9. Output is being written to: /tmp/x/b9.output. You will be notified."}]}}),
        ]);
        let bg = t.background();
        assert_eq!(bg.len(), 2);
        assert_eq!((bg[0].kind, bg[0].task_id.as_deref(), bg[0].output.as_deref()), ("shell", Some("b9"), Some("/tmp/x/b9.output")));
        assert_eq!(bg[1].kind, "agent");
        t.append(2, &[json!({"type": "queue-operation", "operation": "enqueue", "content": "<task-notification>\n<task-id>b9</task-id>\n<status>completed</status>\n</task-notification>"})]);
        assert_eq!(t.background().len(), 1);
    }
}

#[cfg(test)]
mod options_tests {
    use super::{set_options, Item, Transcript};
    use serde_json::json;

    #[test]
    fn show_options_change_what_lines_become() {
        let lines = [
            json!({"type": "user", "isMeta": true, "message": {"content": "skill text"}}),
            json!({"type": "user", "message": {"content": "<system-reminder>x</system-reminder>"}}),
            json!({"type": "user", "message": {"content": "<task-notification><summary>done</summary></task-notification>"}}),
        ];
        set_options(&json!({}));
        let mut t = Transcript::default();
        t.append(0, &lines);
        assert!(matches!(t.items.as_slice(), [Item::Notice { text }] if text == "done"));
        set_options(&json!({"show": {"meta": true, "wrappers": true, "task_notifications": "hidden"}, "labels": {"activity_thinking": "Pondering"}}));
        let mut t = Transcript::default();
        t.append(0, &lines);
        assert_eq!(t.items.len(), 2);
        assert_eq!(t.activity(), "");
        t.append(3, &[json!({"type": "user", "message": {"content": "hi"}})]);
        assert_eq!(t.activity(), "Pondering");
        set_options(&json!({}));
    }
}

