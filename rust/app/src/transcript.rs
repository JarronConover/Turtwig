/*! Claude Code session files, turned into the items a chat view draws. One file line can hold several
content blocks; tool results are attached to the call they answer. */

use serde_json::Value;
use std::collections::HashMap;

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
    pub items: Vec<Item>,
    tool_index: HashMap<String, usize>,
    /** Bumped on every change so views can tell new content from a re-render. */
    pub revision: u64,
}

impl Transcript {
    /** A short line on what the agent is doing now, from the newest item: "Reading settings.jsonc". */
    pub fn activity(&self) -> String {
        match self.items.last() {
            Some(Item::Tool { name, summary, result: None, .. }) => format!("{} {summary}", verb(name)),
            Some(Item::Tool { .. }) | Some(Item::User { .. }) | Some(Item::Image { .. }) => "Thinking".into(),
            Some(Item::Thinking { .. }) => "Thinking".into(),
            Some(Item::Assistant { .. }) => "Writing".into(),
            _ => String::new(),
        }
    }

    pub fn append(&mut self, from: usize, entries: &[Value]) {
        if from != self.lines {
            if from < self.lines {
                return;
            }
            self.lines = from;
        }
        for e in entries {
            self.lines += 1;
            self.push_entry(e);
        }
        self.revision += 1;
    }

    fn push_entry(&mut self, e: &Value) {
        if e.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            return;
        }
        let kind = e.get("type").and_then(Value::as_str).unwrap_or("");
        // Text Claude Code adds for the model (a slash command's expanded skill, caveats): never typed.
        if kind == "user" && e.get("isMeta").and_then(Value::as_bool) == Some(true) {
            return;
        }
        let content = &e["message"]["content"];
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
                    if e.get("level").and_then(Value::as_str) == Some("error") {
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
            self.items.push(Item::User { text: format!("{name} {args}").trim().to_string() });
            return;
        }
        if t.starts_with("<task-notification>") {
            let summary = between(t, "<summary>", "</summary>").unwrap_or("A background task finished");
            self.items.push(Item::Notice { text: summary.to_string() });
            return;
        }
        // Harness wrappers (command output, reminders) aren't things the user typed.
        if t.is_empty() || t.starts_with("<command-") || t.starts_with("<local-command") || t.starts_with("<system-reminder") {
            return;
        }
        self.items.push(Item::User { text: t.to_string() });
    }

    fn tool_result(&mut self, b: &Value) {
        let id = b["tool_use_id"].as_str().unwrap_or("");
        let text = match &b["content"] {
            Value::String(s) => s.clone(),
            Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n"),
            _ => String::new(),
        };
        if let Some(&i) = self.tool_index.get(id) {
            if let Some(Item::Tool { result, is_error, .. }) = self.items.get_mut(i) {
                *result = Some(text);
                *is_error = b["is_error"].as_bool().unwrap_or(false);
            }
        }
    }
}

fn verb(tool: &str) -> &str {
    match tool {
        "Read" => "Reading",
        "Write" | "Edit" | "NotebookEdit" => "Editing",
        "Bash" => "Running",
        "Grep" | "Glob" => "Searching for",
        "WebFetch" => "Fetching",
        "WebSearch" => "Searching the web for",
        "Agent" | "Task" => "Delegating:",
        _ => tool,
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
    text.lines().next().unwrap_or("").chars().take(140).collect()
}

fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = text.find(open)? + open.len();
    let end = text[start..].find(close)? + start;
    Some(text[start..end].trim())
}
