/*! The agent CLIs a chat can run (option `agents`, {name: definition}; bin/chat launches from the same map).
A registry chat names its agent in `agent` (none: claude); claude falls back to the top-level options. */

use serde_json::{json, Value};
use std::path::PathBuf;

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
            "prompt_char": "›",
            "waiting_pattern": CODEX_WAITING_PATTERN,
            "turn_events": { "task_started": "working", "task_complete": "idle", "turn_aborted": "idle" },
        }),
        _ => json!({ "transcripts": "claude" }),
    }
}

pub const CODEX_WAITING_PATTERN: &str =
    r"(?i)Would you like to|Trust this folder|Hooks need review|enter to (confirm|continue|select)|esc to (cancel|skip|quit)|Allow .* to run";

/** `agents.AGENT.KEY`, else (claude only) the top-level option, else the built-in default. */
pub fn get(agent: &str, key: &str) -> Option<Value> {
    let set = config::get("agents").and_then(|a| a.get(agent)?.get(key).cloned()).filter(|v| !v.is_null());
    set.or_else(|| if agent == "claude" { config::get(key) } else { None })
        .or_else(|| builtin(agent).get(key).cloned())
}

pub fn text(agent: &str, key: &str, default: &str) -> String {
    get(agent, key).and_then(|v| v.as_str().map(str::to_string)).unwrap_or_else(|| default.to_string())
}

/** How the agent's transcripts read (`transcripts`): "claude" or "codex". */
pub fn format(agent: &str) -> String {
    text(agent, "transcripts", "claude")
}

fn expand(path: &str) -> PathBuf {
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
        _ => Some(util::transcript_path(s("dir")?, s("id")?)),
    }
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
}
