/*! The agent CLIs a chat can run (option `agents`, {name: definition}; bin/chat launches from the same map).
A registry chat names its agent in `agent` (none: claude); claude falls back to the top-level options. */

use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

use crate::{config, util};

/** The chat's agent: its `agent`, else claude. */
pub fn of(chat: &Value) -> String {
    chat.get("agent").and_then(Value::as_str).filter(|a| !a.is_empty()).unwrap_or("claude").to_string()
}

fn builtin(agent: &str) -> Value {
    match agent {
        "codex" => json!({
            "transcripts": "codex",
            "sessions_dir": "~/.codex/sessions",
            "transcripts_root": "~/.codex/sessions",
            "archived_dir": "~/.codex/archived_sessions",
            "provider": "openai",
            "limits_field": "rate_limits",
            "prompt_char": "›",
            "waiting_pattern": CODEX_WAITING_PATTERN,
            "turn_events": { "task_started": "working", "task_complete": "idle", "turn_aborted": "idle" },
            "title": { "index": "~/.codex/session_index.jsonl", "id": "id", "field": "thread_name" },
        }),
        "claude" => json!({
            "transcripts": "claude",
            // How its chats run: "sdk" (stream-json mode, driven by messages) or "tui" (the CLI in tmux).
            "transport": "sdk",
            "transcripts_root": "~/.claude/projects",
            "provider": "anthropic",
            "process_names": ["claude"],
            // Clears a prompt Claude Code put back in its input after an early Esc (see ChatKeys), a line a
            // press. Not C-c: with no turn running, that also stops every background agent.
            "clear_input_keys": ["C-u"],
            // Its input box: the prompt (and the no-break space after it) right under a rule. While neither
            // shows, it's in a menu (/artifacts, /model) and the app shows its live screen (`screen`).
            "input_box": { "rule": "──", "prompt": "❯\u{a0}" },
            "process_session_pattern": r"--(?:resume|session-id)[ =]([0-9a-f]{8}-[0-9a-f-]{27})",
            "title": { "records": [{ "type": "custom-title", "field": "customTitle" }, { "type": "ai-title", "field": "aiTitle" }] },
        }),
        _ => json!({ "transcripts": "claude" }),
    }
}

/** Every agent: the built-in claude and codex, then the other keys of the `agents` option. */
pub fn names() -> Vec<String> {
    let mut out = vec!["claude".to_string(), "codex".to_string()];
    if let Some(m) = config::get("agents").and_then(|a| a.as_object().cloned()) {
        out.extend(m.keys().filter(|k| !out.contains(k)).cloned().collect::<Vec<_>>());
    }
    out
}

/** How a transcript line becomes an entry. */
pub type Parser = Arc<dyn Fn(&[u8]) -> Value + Send + Sync>;

/** The agent's line parser: "claude" (Claude Code's own entries), "codex" (rollouts), or "jsonl": one JSON
object a line, read through `jsonl_fields` ({type, role, text: JSON pointers}, `roles` mapping role names). */
pub fn parser(agent: &str) -> Parser {
    match format(agent).as_str() {
        "codex" => Arc::new(crate::codex::entry),
        "jsonl" => {
            let fields = get(agent, "jsonl_fields").unwrap_or_else(|| json!({}));
            Arc::new(move |line: &[u8]| jsonl_entry(line, &fields))
        }
        _ => Arc::new(crate::server::parse_entry),
    }
}

fn jsonl_entry(line: &[u8], fields: &Value) -> Value {
    let Ok(v) = serde_json::from_slice::<Value>(line) else { return Value::Null };
    let at = |k: &str, default: &str| fields.get(k).and_then(Value::as_str).unwrap_or(default).to_string();
    let role = v.pointer(&at("role", "/role")).and_then(Value::as_str).unwrap_or("");
    let role = fields.pointer(&format!("/roles/{role}")).and_then(Value::as_str).unwrap_or(role).to_string();
    let text = match v.pointer(&at("text", "/text")) {
        Some(Value::String(t)) => t.clone(),
        Some(other) if !other.is_null() => other.to_string(),
        _ => return json!({ "type": v.pointer(&at("type", "/type")).cloned().unwrap_or(Value::Null) }),
    };
    if role != "user" && role != "assistant" {
        return json!({ "type": role });
    }
    json!({ "type": role, "message": { "role": role, "content": [{ "type": "text", "text": text }] },
            "timestamp": v.pointer(&at("timestamp", "/timestamp")).cloned().unwrap_or(Value::Null) })
}

pub const CODEX_WAITING_PATTERN: &str =
    r"(?i)Would you like to|Trust this folder|Hooks need review|enter to (confirm|continue|select)|esc to (cancel|skip|quit)|Allow .* to run";

/** `agents.AGENT.KEY`, else (claude only) the top-level option, else the built-in default. */
pub fn get(agent: &str, key: &str) -> Option<Value> {
    let set = config::get("agents").and_then(|a| a.get(agent)?.get(key).cloned()).filter(|v| !v.is_null());
    set.or_else(|| if agent == "claude" { config::get(key) } else { None })
        .or_else(|| builtin(agent).get(key).cloned())
}

/** The agent's whole definition: built-in keys, then (claude) the top-level ones it reads, then `agents.AGENT`. */
pub fn merged(agent: &str) -> Value {
    let mut out = builtin(agent);
    if agent == "claude" {
        for k in ["prompt_char", "waiting_pattern"] {
            if let Some(v) = config::get(k) {
                out[k] = v;
            }
        }
    }
    if let Some(m) = config::get("agents").and_then(|a| a.get(agent)?.as_object().cloned()) {
        for (k, v) in m.into_iter().filter(|(_, v)| !v.is_null()) {
            out[k] = v;
        }
    }
    out
}

pub fn text(agent: &str, key: &str, default: &str) -> String {
    get(agent, key).and_then(|v| v.as_str().map(str::to_string)).unwrap_or_else(|| default.to_string())
}

/** How the agent's transcripts read (`transcripts`): "claude", "codex" or "jsonl". */
pub fn format(agent: &str) -> String {
    text(agent, "transcripts", "claude")
}

pub fn expand(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => util::home().join(rest),
        None => PathBuf::from(path),
    }
}

/** Where the agent keeps its rollouts (`sessions_dir`). */
pub fn sessions_dir(agent: &str) -> PathBuf {
    expand(&text(agent, "sessions_dir", "~/.codex/sessions"))
}

/** The chat's transcript (maybe not written yet). None: a Codex chat whose session isn't known yet. */
pub fn transcript_path(chat: &Value) -> Option<PathBuf> {
    let s = |k: &str| chat.get(k).and_then(Value::as_str).filter(|v| !v.is_empty());
    let agent = of(chat);
    match format(&agent).as_str() {
        "codex" => {
            if let Some(p) = s("transcript").map(PathBuf::from).filter(|p| p.exists()) {
                return Some(p);
            }
            crate::codex::find_rollout(&sessions_dir(&agent), s("session_id")?)
        }
        "jsonl" => {
            if let Some(p) = s("transcript").map(PathBuf::from).filter(|p| p.exists()) {
                return Some(p);
            }
            // `transcript_path`: {id} {session} {dir} {dir_slug}, ~ for home.
            let template = text(&agent, "transcript_path", "");
            let dir = s("dir").unwrap_or_default();
            let filled = template.replace("{id}", s("id")?).replace("{session}", s("session_id").or(s("id"))?)
                .replace("{dir_slug}", &util::slug(&util::realpath(dir))).replace("{dir}", dir);
            (!template.is_empty()).then(|| expand(&filled))
        }
        _ => Some(util::transcript_path(s("dir")?, s("id")?)),
    }
}

/** How the chat's agent runs: the registry's `transport` (set when it was launched), else the agent's
(`agents.AGENT.transport`), else "tui". */
pub fn transport(chat: &Value) -> String {
    chat.get("transport").and_then(Value::as_str).filter(|t| !t.is_empty()).map(str::to_string)
        .or_else(|| get(&of(chat), "transport").and_then(|v| v.as_str().map(str::to_string)))
        .unwrap_or_else(|| "tui".into())
}

/** How the chat was last launched: the registry's `transport`, else "tui". A chat nobody started in SDK
mode (one another program runs, or an old TUI one) is never taken for an SDK chat on its own. */
pub fn launched_transport(chat: &Value) -> String {
    chat.get("transport").and_then(Value::as_str).filter(|t| !t.is_empty()).unwrap_or("tui").to_string()
}

/** The id the agent's hooks report for the chat: its `session_id` (Codex's own), else the chat id. */
pub fn session_key(chat: &Value) -> Option<String> {
    let s = |k: &str| chat.get(k).and_then(Value::as_str).filter(|v| !v.is_empty()).map(str::to_string);
    s("session_id").or_else(|| s("id"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn a_chat_without_an_agent_is_claude() {
        assert_eq!(super::of(&json!({"id": "x"})), "claude");
        assert_eq!(super::of(&json!({"agent": "codex"})), "codex");
        assert_eq!(super::format("codex"), "codex");
        assert_eq!(super::text("codex", "prompt_char", ""), "›");
    }

    #[test]
    fn a_jsonl_line_becomes_a_text_entry() {
        let fields = json!({"role": "/who", "text": "/body", "roles": {"bot": "assistant"}});
        let e = super::jsonl_entry(br#"{"who": "bot", "body": "hi"}"#, &fields);
        assert_eq!(e["type"], "assistant");
        assert_eq!(e["message"]["content"][0]["text"], "hi");
        assert_eq!(super::jsonl_entry(br#"{"who": "user", "body": "yo"}"#, &fields)["message"]["role"], "user");
        assert_eq!(super::jsonl_entry(br#"{"type": "meta"}"#, &fields)["type"], "meta");
    }
}
