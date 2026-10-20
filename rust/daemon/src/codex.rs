/*! Codex CLI rollouts (`~/.codex/sessions/YYYY/MM/DD/rollout-<time>-<session>.jsonl`) as Claude Code
transcript entries, one per line, so the app draws both the same way. */

use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/** A rollout line as a transcript entry. The thread items Codex shows (`item_completed`) carry the
conversation; everything else becomes `{type}`, which the app skips. */
pub fn entry(line: &[u8]) -> Value {
    let Ok(v) = serde_json::from_slice::<Value>(line) else { return Value::Null };
    let p = &v["payload"];
    let kind = v["type"].as_str().unwrap_or("");
    let s = |k: &str| p[k].as_str().unwrap_or("").to_string();
    let out = match (kind, p["type"].as_str().unwrap_or("")) {
        ("event_msg", "item_completed") => item(&p["item"]),
        // Rollouts from Codex versions before thread items.
        ("event_msg", "user_message") => user(vec![text(&s("message"))]),
        ("event_msg", "agent_message") => assistant(vec![text(&s("message"))]),
        ("event_msg", "agent_reasoning") => assistant(vec![json!({"type": "thinking", "thinking": s("text")})]),
        ("event_msg", "error") => Some(json!({"type": "system", "level": "error", "content": s("message")})),
        // The model each turn runs on (the app shows the newest).
        ("turn_context", _) => Some(json!({"type": "turn_context", "model": s("model")})),
        _ => None,
    };
    out.unwrap_or_else(|| json!({ "type": kind }))
}

fn text(t: &str) -> Value {
    json!({"type": "text", "text": t})
}

fn user(content: Vec<Value>) -> Option<Value> {
    (!content.is_empty()).then(|| json!({"type": "user", "message": {"role": "user", "content": content}}))
}

fn assistant(content: Vec<Value>) -> Option<Value> {
    (!content.is_empty()).then(|| json!({"type": "assistant", "message": {"role": "assistant", "content": content}}))
}

/** A tool call and its result in one entry (a thread item arrives finished). */
fn tool(id: &str, name: &str, input: Value, result: String, is_error: bool) -> Option<Value> {
    let mut result = result;
    crate::server::cut(&mut result);
    assistant(vec![
        json!({"type": "tool_use", "id": id, "name": name, "input": input}),
        json!({"type": "tool_result", "tool_use_id": id, "content": result, "is_error": is_error}),
    ])
}

fn texts(parts: &Value) -> Vec<String> {
    parts.as_array().into_iter().flatten()
        .filter_map(|p| p.as_str().or_else(|| p["text"].as_str()).map(str::to_string))
        .collect()
}

/** `["/bin/zsh", "-lc", "cmd"]` as `cmd`. */
fn command_line(cmd: &Value) -> String {
    let parts: Vec<&str> = cmd.as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    match parts.as_slice() {
        [_, flag, script] if flag.starts_with('-') && flag.ends_with('c') => script.to_string(),
        _ => cmd.as_str().map(str::to_string).unwrap_or_else(|| parts.join(" ")),
    }
}

fn failed(it: &Value) -> bool {
    matches!(it["status"].as_str(), Some("failed" | "declined" | "error"))
}

fn item(it: &Value) -> Option<Value> {
    let s = |k: &str| it[k].as_str().unwrap_or("").to_string();
    let id = s("id");
    match it["type"].as_str()? {
        "UserMessage" => {
            let mut content = vec![];
            for c in it["content"].as_array().into_iter().flatten() {
                match c["type"].as_str() {
                    Some("text") => content.push(text(c["text"].as_str().unwrap_or(""))),
                    Some("image") => {
                        let url = c["url"].as_str().or_else(|| c["image_url"].as_str()).unwrap_or("");
                        if let Some((head, data)) = url.strip_prefix("data:").and_then(|u| u.split_once(";base64,")) {
                            content.push(json!({"type": "image", "source": {"type": "base64", "media_type": head, "data": data}}));
                        }
                    }
                    _ => {}
                }
            }
            user(content)
        }
        "AgentMessage" => assistant(texts(&it["content"]).iter().map(|t| text(t)).collect()),
        "Reasoning" => {
            let mut parts = texts(&it["summary_text"]);
            if parts.is_empty() {
                parts = texts(&it["raw_content"]);
            }
            let thought = parts.join("\n\n");
            (!thought.trim().is_empty()).then(|| assistant(vec![json!({"type": "thinking", "thinking": thought})]))?
        }
        "CommandExecution" => {
            let cwd = s("cwd");
            let input = json!({"command": command_line(&it["command"]), "cwd": cwd.strip_prefix("file://").unwrap_or(&cwd)});
            let code = it["exit_code"].as_i64().unwrap_or(0);
            tool(&id, "Bash", input, s("aggregated_output"), failed(it) || code != 0)
        }
        "FileChange" => {
            let mut content = vec![];
            for (i, (path, change)) in it["changes"].as_object().into_iter().flatten().enumerate() {
                let diff = change["unified_diff"].as_str().unwrap_or("");
                let lines = |mark: char| diff.lines().filter(|l| l.starts_with(mark) && !l.starts_with(&format!("{mark}{mark}{mark}")))
                    .map(|l| &l[1..]).collect::<Vec<_>>().join("\n");
                let (name, input) = match change["type"].as_str() {
                    Some("add") => ("Write", json!({"file_path": path, "content": change["content"].as_str().map(str::to_string).unwrap_or_else(|| lines('+'))})),
                    Some("delete") => ("Delete", json!({"file_path": path})),
                    _ => ("Edit", json!({"file_path": path, "old_string": lines('-'), "new_string": lines('+'), "move_path": change["move_path"]})),
                };
                let call = format!("{id}:{i}");
                content.push(json!({"type": "tool_use", "id": call, "name": name, "input": input}));
                content.push(json!({"type": "tool_result", "tool_use_id": call, "content": s("stdout"), "is_error": failed(it)}));
            }
            assistant(content)
        }
        "McpToolCall" => {
            let name = format!("mcp__{}__{}", s("server"), s("tool"));
            let r = &it["result"];
            let mut out = texts(&r["content"]).join("\n");
            if out.is_empty() {
                out = it["error"]["message"].as_str().or_else(|| it["error"].as_str()).unwrap_or("").to_string();
            }
            tool(&id, &name, it["arguments"].clone(), out, failed(it) || r["isError"].as_bool() == Some(true))
        }
        "WebSearch" => tool(&id, "WebSearch", json!({"query": s("query")}), String::new(), failed(it)),
        "SubAgentActivity" => {
            let path = s("agent_path");
            let input = json!({"description": path.rsplit('/').next().unwrap_or(&path), "agent": s("agent_thread_id")});
            tool(&id, "Agent", input, s("kind"), false)
        }
        other => {
            // Anything newer: a step named for its kind (an Extension's `kind`, e.g. clock.sleep).
            let name = it["kind"].as_str().filter(|_| other == "Extension").map(str::to_string)
                .or_else(|| it["tool"].as_str().map(|t| format!("{other}.{t}"))).unwrap_or_else(|| other.to_string());
            let input: Map<String, Value> = it.as_object()?.iter()
                .filter(|(k, v)| !matches!(k.as_str(), "type" | "id" | "kind" | "status") && !v.is_null())
                .map(|(k, v)| (k.clone(), v.clone())).collect();
            tool(&id, &name, Value::Object(input), s("status"), failed(it))
        }
    }
}

static FOUND: Mutex<Option<HashMap<String, PathBuf>>> = Mutex::new(None);

fn newest_first(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
    v.sort();
    v.reverse();
    v
}

/** The rollout of session `id` under `dir` (by its file name), newest days first. */
pub fn find_rollout(dir: &Path, id: &str) -> Option<PathBuf> {
    if let Some(p) = FOUND.lock().unwrap().as_ref().and_then(|m| m.get(id)).filter(|p| p.exists()) {
        return Some(p.clone());
    }
    let suffix = format!("-{id}.jsonl");
    for year in newest_first(dir) {
        for month in newest_first(&year) {
            for day in newest_first(&month) {
                for f in newest_first(&day) {
                    if f.to_string_lossy().ends_with(&suffix) {
                        FOUND.lock().unwrap().get_or_insert_with(HashMap::new).insert(id.to_string(), f.clone());
                        return Some(f);
                    }
                }
            }
        }
    }
    None
}

static TURNS: Mutex<Option<HashMap<PathBuf, (u64, Option<String>)>>> = Mutex::new(None);

/** The state the rollout's newest turn event puts the chat in (`turn_events`: {event_msg type: state}),
read from its last `codex_state_tail_kb`. None: no turn event there. */
pub fn turn_state(path: &Path, events: &Map<String, Value>) -> Option<String> {
    let len = std::fs::metadata(path).ok()?.len();
    if let Some((at, st)) = TURNS.lock().unwrap().as_ref().and_then(|m| m.get(path).cloned()) {
        if at == len {
            return st;
        }
    }
    let tail = (crate::config::count("codex_state_tail_kb", 512) as u64) * 1024;
    let mut f = std::fs::File::open(path).ok()?;
    f.seek(SeekFrom::Start(len.saturating_sub(tail))).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let st = last_turn_event(&buf, events);
    TURNS.lock().unwrap().get_or_insert_with(HashMap::new).insert(path.to_path_buf(), (len, st.clone()));
    st
}

fn last_turn_event(buf: &[u8], events: &Map<String, Value>) -> Option<String> {
    for line in buf.split(|b| *b == b'\n').rev() {
        if !line.windows(9).any(|w| w == b"event_msg") {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<Value>(line) else { continue };
        if let Some(st) = v["payload"]["type"].as_str().and_then(|t| events.get(t)).and_then(Value::as_str) {
            return Some(st.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::entry;
    use serde_json::{json, Value};

    fn line(v: Value) -> Vec<u8> {
        serde_json::to_vec(&v).unwrap()
    }

    fn done(item: Value) -> Vec<u8> {
        line(json!({"timestamp": "2026-10-16T23:00:58Z", "type": "event_msg", "payload": {"type": "item_completed", "item": item}}))
    }

    #[test]
    fn messages_become_user_and_assistant_text() {
        let u = entry(&done(json!({"type": "UserMessage", "id": "u1", "content": [{"type": "text", "text": "fix it", "text_elements": []}]})));
        assert_eq!(u["type"], "user");
        assert_eq!(u["message"]["content"][0]["text"], "fix it");
        let a = entry(&done(json!({"type": "AgentMessage", "id": "m1", "content": [{"type": "Text", "text": "done"}], "phase": "final_answer"})));
        assert_eq!(a["type"], "assistant");
        assert_eq!(a["message"]["content"][0], json!({"type": "text", "text": "done"}));
    }

    #[test]
    fn a_command_is_a_bash_step_with_its_output() {
        let e = entry(&done(json!({"type": "CommandExecution", "id": "exec-1", "command": ["/bin/zsh", "-lc", "echo hi"],
            "cwd": "file:///tmp/w", "status": "completed", "aggregated_output": "hi\n", "exit_code": 0})));
        let c = &e["message"]["content"];
        assert_eq!(c[0], json!({"type": "tool_use", "id": "exec-1", "name": "Bash", "input": {"command": "echo hi", "cwd": "/tmp/w"}}));
        assert_eq!(c[1]["tool_use_id"], "exec-1");
        assert_eq!(c[1]["content"], "hi\n");
        assert_eq!(c[1]["is_error"], false);
        let failed = entry(&done(json!({"type": "CommandExecution", "id": "exec-2", "command": "false", "exit_code": 1})));
        assert_eq!(failed["message"]["content"][1]["is_error"], true);
    }

    #[test]
    fn file_changes_and_tool_calls() {
        let e = entry(&done(json!({"type": "FileChange", "id": "p1", "status": "completed", "stdout": "Success.", "changes": {
            "/w/a.rs": {"type": "update", "unified_diff": "@@ -1 +1 @@\n-old\n+new\n", "move_path": null}}})));
        let c = &e["message"]["content"];
        assert_eq!(c[0]["name"], "Edit");
        assert_eq!(c[0]["input"]["old_string"], "old");
        assert_eq!(c[0]["input"]["new_string"], "new");
        assert_eq!(c[1]["tool_use_id"], c[0]["id"]);
        let m = entry(&done(json!({"type": "McpToolCall", "id": "x1", "server": "linear", "tool": "get_issue", "arguments": {"id": "A-1"},
            "status": "failed", "result": {"content": [{"type": "text", "text": "reauthenticate"}], "isError": true}})));
        let c = &m["message"]["content"];
        assert_eq!(c[0]["name"], "mcp__linear__get_issue");
        assert_eq!(c[1]["content"], "reauthenticate");
        assert_eq!(c[1]["is_error"], true);
        let x = entry(&done(json!({"type": "Extension", "kind": "clock.sleep", "id": "c1", "durationMs": 45000})));
        assert_eq!(x["message"]["content"][0]["name"], "clock.sleep");
        assert_eq!(x["message"]["content"][0]["input"]["durationMs"], 45000);
    }

    #[test]
    fn reasoning_with_a_summary_is_thinking_and_the_rest_is_skipped() {
        let r = entry(&done(json!({"type": "Reasoning", "id": "r1", "summary_text": ["Looking at the tests"], "raw_content": []})));
        assert_eq!(r["message"]["content"][0], json!({"type": "thinking", "thinking": "Looking at the tests"}));
        let hidden = entry(&done(json!({"type": "Reasoning", "id": "r2", "summary_text": [], "raw_content": []})));
        assert_eq!(hidden, json!({"type": "event_msg"}));
        for skipped in [
            json!({"type": "session_meta", "payload": {"id": "s", "cwd": "/w"}}),
            json!({"type": "response_item", "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "# AGENTS.md"}]}}),
            json!({"type": "event_msg", "payload": {"type": "token_count"}}),
        ] {
            let kind = skipped["type"].clone();
            assert_eq!(entry(&line(skipped)), json!({"type": kind}));
        }
        assert_eq!(entry(b"not json"), Value::Null);
    }

    #[test]
    fn the_newest_turn_event_sets_the_state() {
        let events = json!({"task_started": "working", "task_complete": "idle", "turn_aborted": "idle"});
        let events = events.as_object().unwrap();
        let ev = |t: &str| String::from_utf8(line(json!({"type": "event_msg", "payload": {"type": t}}))).unwrap();
        let started = format!("{}\n{}\n", ev("task_started"), ev("token_count"));
        assert_eq!(super::last_turn_event(started.as_bytes(), events).as_deref(), Some("working"));
        let aborted = format!("{started}{}\n{{\"partial", ev("turn_aborted"));
        assert_eq!(super::last_turn_event(aborted.as_bytes(), events).as_deref(), Some("idle"));
        assert_eq!(super::last_turn_event(ev("token_count").as_bytes(), events), None);
    }

    #[test]
    fn finds_a_rollout_by_its_session_id() {
        let dir = std::env::temp_dir().join(format!("abx-codex-{}", std::process::id()));
        let day = dir.join("2026").join("10").join("16");
        std::fs::create_dir_all(&day).unwrap();
        let f = day.join("rollout-2026-10-16T17-00-58-01a146f2-7877.jsonl");
        std::fs::write(&f, "{}\n").unwrap();
        assert_eq!(super::find_rollout(&dir, "01a146f2-7877"), Some(f));
        assert_eq!(super::find_rollout(&dir, "nope"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
