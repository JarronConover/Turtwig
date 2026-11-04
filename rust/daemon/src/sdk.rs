/*! SDK chats: one long-lived `claude` per open chat in stream-json mode (as VS Code runs it), driven by
messages and control requests on its stdio instead of keystrokes in tmux; its state follows the stream. */

use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::config;

/** What the daemon's model shows for an SDK chat. */
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    pub alive: bool,
    /** idle | working | waiting | stopped */
    pub state: String,
    pub waiting_on: Option<Value>,
    pub model: Option<String>,
    pub mode: Option<String>,
    pub effort: Option<String>,
    pub session_id: Option<String>,
}

/** What a chat's process reports, for the server to fan out. */
pub enum Notice {
    /** The chat's state, model or mode changed: rebuild the model. */
    Changed,
    /** The reply being written (null: the turn ended). */
    Live(String, Option<Value>),
}

struct Inner {
    alive: bool,
    state: String,
    /** The open `can_use_tool` request, as the app shows it (`waiting_on`). */
    waiting: Option<Value>,
    model: Option<String>,
    mode: Option<String>,
    effort: Option<String>,
    session_id: Option<String>,
    /** The assistant message being streamed (a transcript-shaped entry) and its partial tool inputs. */
    live: Option<Value>,
    partial_json: HashMap<usize, String>,
    last_live_sent: Option<Instant>,
}

pub struct SdkChat {
    pub id: String,
    stdin: Mutex<Option<ChildStdin>>,
    child: Mutex<Option<Child>>,
    inner: Mutex<Inner>,
    /** Our control requests waiting for the process's response, by request id. */
    pending: Mutex<HashMap<String, Sender<Value>>>,
    notices: Sender<Notice>,
    seq: Mutex<u64>,
}

pub struct Sdk {
    chats: Mutex<HashMap<String, Arc<SdkChat>>>,
    notices: Sender<Notice>,
}

impl Sdk {
    pub fn new() -> (Arc<Sdk>, Receiver<Notice>) {
        let (tx, rx) = channel();
        (Arc::new(Sdk { chats: Mutex::new(HashMap::new()), notices: tx }), rx)
    }

    pub fn get(&self, chat_id: &str) -> Option<Arc<SdkChat>> {
        let chats = self.chats.lock().unwrap();
        chats.get(chat_id).cloned().or_else(|| chats.iter().find(|(k, _)| k.starts_with(chat_id)).map(|(_, c)| c.clone()))
    }

    /** The chat's process, running. */
    pub fn running(&self, chat_id: &str) -> Option<Arc<SdkChat>> {
        self.get(chat_id).filter(|c| c.alive())
    }

    pub fn snapshots(&self) -> HashMap<String, Snapshot> {
        self.chats.lock().unwrap().iter().map(|(id, c)| (id.clone(), c.snapshot())).collect()
    }

    /** Starts the chat's process: `argv` (bin/chat's `_sdk_launch` builds it) in `cwd`, with `envs`. A
    process already running for the chat is kept. */
    pub fn start(&self, chat_id: &str, argv: &[String], cwd: Option<&str>, envs: &[(String, String)], model: Option<String>,
                 mode: Option<String>, effort: Option<String>) -> Result<Arc<SdkChat>, String> {
        if let Some(c) = self.running(chat_id) {
            return Ok(c);
        }
        let (program, args) = argv.split_first().ok_or("empty command")?;
        let mut cmd = Command::new(program);
        cmd.args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        for (k, v) in envs {
            cmd.env(k, v);
        }
        if let Some(d) = cwd {
            cmd.current_dir(d);
        }
        let mut child = cmd.spawn().map_err(|e| format!("{program}: {e}"))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let stderr = child.stderr.take();
        let chat = Arc::new(SdkChat {
            id: chat_id.to_string(),
            stdin: Mutex::new(stdin),
            child: Mutex::new(Some(child)),
            inner: Mutex::new(Inner {
                alive: true,
                state: "idle".into(),
                waiting: None,
                model,
                mode,
                effort,
                session_id: None,
                live: None,
                partial_json: HashMap::new(),
                last_live_sent: None,
            }),
            pending: Mutex::new(HashMap::new()),
            notices: self.notices.clone(),
            seq: Mutex::new(0),
        });
        self.chats.lock().unwrap().insert(chat_id.to_string(), chat.clone());
        let reader = chat.clone();
        std::thread::Builder::new().name(format!("sdk-{}", &chat_id[..chat_id.len().min(8)])).spawn(move || reader.read_loop(stdout)).map_err(|e| e.to_string())?;
        if let Some(err) = stderr {
            let id = chat_id.to_string();
            std::thread::spawn(move || {
                for line in BufReader::new(err).lines().map_while(Result::ok) {
                    if !line.trim().is_empty() {
                        eprintln!("sdk {}: {line}", &id[..id.len().min(8)]);
                    }
                }
            });
        }
        // The CLI answers `initialize` with its commands; the answer isn't needed, only that it's up.
        let _ = chat.control(json!({ "subtype": "initialize", "hooks": {} }), config::secs("sdk_init_timeout_secs", 30));
        chat.changed();
        Ok(chat)
    }

    /** Ends the chat's process (stdin closed, then killed if it lingers) and forgets it. */
    pub fn stop(&self, chat_id: &str) {
        let Some(c) = self.get(chat_id) else { return };
        c.stop();
        self.chats.lock().unwrap().remove(&c.id);
    }
}

impl SdkChat {
    pub fn alive(&self) -> bool {
        self.inner.lock().unwrap().alive
    }

    pub fn snapshot(&self) -> Snapshot {
        let i = self.inner.lock().unwrap();
        Snapshot {
            alive: i.alive,
            state: if i.alive { i.state.clone() } else { "stopped".into() },
            waiting_on: if i.alive { i.waiting.clone() } else { None },
            model: i.model.clone(),
            mode: i.mode.clone(),
            effort: i.effort.clone(),
            session_id: i.session_id.clone(),
        }
    }

    fn changed(&self) {
        let _ = self.notices.send(Notice::Changed);
    }

    fn write(&self, v: &Value) -> Result<(), String> {
        let mut guard = self.stdin.lock().unwrap();
        let w = guard.as_mut().ok_or("the chat's process has ended")?;
        let mut s = serde_json::to_string(v).map_err(|e| e.to_string())?;
        s.push('\n');
        w.write_all(s.as_bytes()).and_then(|_| w.flush()).map_err(|e| format!("couldn't write to the chat: {e}"))
    }

    /** A user message: text, with attachments (`{type: "image", source: …}` blocks) after it. */
    pub fn send(&self, text: &str, blocks: Vec<Value>) -> Result<(), String> {
        let mut content = vec![];
        if !text.is_empty() {
            content.push(json!({ "type": "text", "text": text }));
        }
        content.extend(blocks);
        if content.is_empty() {
            return Err("nothing to send".into());
        }
        self.write(&json!({ "type": "user", "message": { "role": "user", "content": content }, "parent_tool_use_id": null, "session_id": null }))?;
        {
            let mut i = self.inner.lock().unwrap();
            i.state = "working".into();
            i.waiting = None;
        }
        self.changed();
        Ok(())
    }

    /** A control request to the process, answered within `timeout` (its `response` object). */
    pub fn control(&self, request: Value, timeout: Duration) -> Result<Value, String> {
        let id = {
            let mut n = self.seq.lock().unwrap();
            *n += 1;
            format!("abx-{}-{}", std::process::id(), *n)
        };
        let (tx, rx) = channel();
        self.pending.lock().unwrap().insert(id.clone(), tx);
        self.write(&json!({ "type": "control_request", "request_id": id, "request": request })).inspect_err(|_| {
            self.pending.lock().unwrap().remove(&id);
        })?;
        match rx.recv_timeout(timeout) {
            Ok(v) if v.get("subtype").and_then(Value::as_str) == Some("error") => {
                Err(v.get("error").and_then(Value::as_str).unwrap_or("the request failed").to_string())
            }
            Ok(v) => Ok(v.get("response").cloned().unwrap_or(Value::Null)),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err("the chat didn't answer in time".into())
            }
        }
    }

    /** Stops the running turn. */
    pub fn interrupt(&self) -> Result<Value, String> {
        self.control(json!({ "subtype": "interrupt" }), config::secs("sdk_control_timeout_secs", 20))
    }

    /** `set_model`, `set_permission_mode`, `apply_flag_settings`, …: the response, with the chat's record
    of its model, mode and effort kept up. */
    pub fn request(&self, request: Value) -> Result<Value, String> {
        let r = self.control(request.clone(), config::secs("sdk_control_timeout_secs", 20))?;
        let mut i = self.inner.lock().unwrap();
        match request.get("subtype").and_then(Value::as_str) {
            Some("set_model") => i.model = r.get("model").or(request.get("model")).and_then(Value::as_str).map(str::to_string),
            Some("set_permission_mode") => i.mode = r.get("mode").or(request.get("mode")).and_then(Value::as_str).map(str::to_string),
            Some("apply_flag_settings") => {
                if let Some(e) = request.pointer("/settings/effortLevel").and_then(Value::as_str) {
                    i.effort = Some(e.to_string());
                }
            }
            _ => {}
        }
        drop(i);
        self.changed();
        Ok(r)
    }

    /** Answers the open `can_use_tool` request (`request_id` None: whichever is open). */
    pub fn answer(&self, request_id: Option<&str>, response: Value) -> Result<(), String> {
        let id = {
            let i = self.inner.lock().unwrap();
            let open = i.waiting.as_ref().and_then(|w| w.get("request_id")).and_then(Value::as_str).map(str::to_string);
            match (request_id, open) {
                (Some(r), Some(o)) if r != o => return Err("that question was already answered".into()),
                (Some(r), _) => r.to_string(),
                (None, Some(o)) => o,
                (None, None) => return Err("the chat isn't waiting on anything".into()),
            }
        };
        self.write(&json!({ "type": "control_response", "response": { "subtype": "success", "request_id": id, "response": response } }))?;
        {
            let mut i = self.inner.lock().unwrap();
            i.waiting = None;
            i.state = "working".into();
            // Allowing with a mode switch ("Allow all edits this session") changes the chat's mode.
            if let Some(mode) = response.get("updatedPermissions").and_then(Value::as_array).and_then(|a| {
                a.iter().find(|p| p.get("type").and_then(Value::as_str) == Some("setMode")).and_then(|p| p.get("mode")).and_then(Value::as_str)
            }) {
                i.mode = Some(mode.to_string());
            }
        }
        self.changed();
        Ok(())
    }

    pub fn stop(&self) {
        *self.stdin.lock().unwrap() = None;
        let child = self.child.lock().unwrap().take();
        if let Some(mut child) = child {
            let until = Instant::now() + config::ms("sdk_stop_wait_ms", 2000);
            while Instant::now() < until {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        self.ended();
    }

    fn ended(&self) {
        let was = {
            let mut i = self.inner.lock().unwrap();
            let was = i.alive;
            i.alive = false;
            i.waiting = None;
            i.live = None;
            was
        };
        self.pending.lock().unwrap().clear();
        if was {
            let _ = self.notices.send(Notice::Live(self.id.clone(), None));
            self.changed();
        }
    }

    fn read_loop(self: Arc<Self>, out: std::process::ChildStdout) {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            self.on_message(&v);
        }
        let child = self.child.lock().unwrap().take();
        if let Some(mut child) = child {
            let _ = child.wait();
        }
        self.ended();
    }

    fn on_message(&self, v: &Value) {
        let kind = v.get("type").and_then(Value::as_str).unwrap_or("");
        // Subagents' messages carry their parent's tool_use id: not the chat's own reply.
        let own = v.get("parent_tool_use_id").is_none_or(Value::is_null);
        match kind {
            "system" if v.get("subtype").and_then(Value::as_str) == Some("init") => {
                let mut i = self.inner.lock().unwrap();
                i.session_id = v.get("session_id").and_then(Value::as_str).map(str::to_string);
                if let Some(m) = v.get("model").and_then(Value::as_str) {
                    i.model = Some(m.to_string());
                }
                if let Some(m) = v.get("permissionMode").and_then(Value::as_str) {
                    i.mode = Some(m.to_string());
                }
                drop(i);
                self.changed();
            }
            "control_request" => self.on_control_request(v),
            "control_response" => {
                let r = v.get("response").cloned().unwrap_or(Value::Null);
                if let Some(id) = r.get("request_id").and_then(Value::as_str) {
                    if let Some(tx) = self.pending.lock().unwrap().remove(id) {
                        let _ = tx.send(r);
                    }
                }
            }
            "control_cancel_request" => {
                // The CLI withdrew its question (the turn was interrupted): nothing to answer any more.
                let id = v.get("request_id").and_then(Value::as_str);
                let mut i = self.inner.lock().unwrap();
                if i.waiting.as_ref().and_then(|w| w.get("request_id")).and_then(Value::as_str) == id || id.is_none() {
                    i.waiting = None;
                    i.state = "working".into();
                    drop(i);
                    self.changed();
                }
            }
            "stream_event" if own => self.on_stream_event(v.get("event").unwrap_or(&Value::Null), v),
            "assistant" if own => {
                let mut i = self.inner.lock().unwrap();
                let mut m = v.clone();
                m["live"] = json!(true);
                if let Some(model) = v.pointer("/message/model").and_then(Value::as_str) {
                    i.model = Some(model.to_string());
                }
                i.live = Some(m.clone());
                i.partial_json.clear();
                i.last_live_sent = Some(Instant::now());
                drop(i);
                let _ = self.notices.send(Notice::Live(self.id.clone(), Some(m)));
            }
            "result" => {
                let mut i = self.inner.lock().unwrap();
                i.state = "idle".into();
                i.waiting = None;
                i.live = None;
                i.partial_json.clear();
                drop(i);
                let _ = self.notices.send(Notice::Live(self.id.clone(), None));
                self.changed();
            }
            _ => {}
        }
    }

    /** A permission or question (`can_use_tool`) waits for the app; any other request (hook callbacks, SDK
    MCP servers, dialogs) is refused at once rather than left hanging. */
    fn on_control_request(&self, v: &Value) {
        let id = v.get("request_id").and_then(Value::as_str).unwrap_or("").to_string();
        let r = v.get("request").cloned().unwrap_or(Value::Null);
        if r.get("subtype").and_then(Value::as_str) == Some("can_use_tool") {
            let tool = r.get("tool_name").and_then(Value::as_str).unwrap_or("a tool").to_string();
            let display = r.get("display_name").and_then(Value::as_str).unwrap_or(&tool).to_string();
            let input = r.get("input").cloned().unwrap_or(json!({}));
            let question = input.pointer("/questions/0/question").and_then(Value::as_str).map(str::to_string);
            let message = question.unwrap_or_else(|| match r.get("description").and_then(Value::as_str) {
                Some(d) if !d.is_empty() => format!("{display}: {d}"),
                _ => format!("Allow {display}?"),
            });
            let mut w = Map::new();
            w.insert("request_id".into(), json!(id));
            w.insert("tool_name".into(), json!(tool));
            w.insert("display_name".into(), json!(display));
            w.insert("tool_input".into(), input);
            w.insert("tool_use_id".into(), r.get("tool_use_id").cloned().unwrap_or(Value::Null));
            w.insert("suggestions".into(), r.get("permission_suggestions").cloned().unwrap_or(json!([])));
            w.insert("message".into(), json!(message));
            w.insert("sdk".into(), json!(true));
            if let Some(d) = r.get("decision_reason") {
                w.insert("decision_reason".into(), d.clone());
            }
            if let Some(b) = r.get("blocked_path") {
                w.insert("blocked_path".into(), b.clone());
            }
            let mut i = self.inner.lock().unwrap();
            i.waiting = Some(Value::Object(w));
            i.state = "waiting".into();
            drop(i);
            self.changed();
        } else {
            let sub = r.get("subtype").and_then(Value::as_str).unwrap_or("?");
            let _ = self.write(&json!({ "type": "control_response", "response": { "subtype": "error", "request_id": id, "error": format!("{sub} isn't handled by Turtwig") } }));
        }
    }

    /** Builds the reply being written from the API's streaming events, and sends it on (at most every
    `sdk_live_ms`, and at each block's end). */
    fn on_stream_event(&self, e: &Value, outer: &Value) {
        let kind = e.get("type").and_then(Value::as_str).unwrap_or("");
        let mut i = self.inner.lock().unwrap();
        let mut flush = false;
        match kind {
            "message_start" => {
                let m = e.get("message").cloned().unwrap_or(json!({}));
                let mut msg = json!({ "role": "assistant", "content": [] });
                for k in ["id", "model", "type"] {
                    if let Some(x) = m.get(k) {
                        msg[k] = x.clone();
                    }
                }
                if let Some(model) = m.get("model").and_then(Value::as_str) {
                    i.model = Some(model.to_string());
                }
                i.live = Some(json!({ "type": "assistant", "message": msg, "live": true, "uuid": outer.get("uuid").cloned().unwrap_or(Value::Null),
                                      "session_id": outer.get("session_id").cloned().unwrap_or(Value::Null) }));
                i.partial_json.clear();
                i.state = "working".into();
            }
            "content_block_start" => {
                let idx = e.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let mut block = e.get("content_block").cloned().unwrap_or(json!({}));
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => block["text"] = json!(block.get("text").and_then(Value::as_str).unwrap_or("")),
                    Some("thinking") => block["thinking"] = json!(block.get("thinking").and_then(Value::as_str).unwrap_or("")),
                    Some("tool_use") => {
                        i.partial_json.insert(idx, String::new());
                    }
                    _ => {}
                }
                if let Some(content) = i.live.as_mut().and_then(|l| l.pointer_mut("/message/content")).and_then(Value::as_array_mut) {
                    while content.len() <= idx {
                        content.push(json!({ "type": "text", "text": "" }));
                    }
                    content[idx] = block;
                }
            }
            "content_block_delta" => {
                let idx = e.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let d = e.get("delta").cloned().unwrap_or(json!({}));
                let mut partial = None;
                if let Some(j) = d.get("partial_json").and_then(Value::as_str) {
                    let buf = i.partial_json.entry(idx).or_default();
                    buf.push_str(j);
                    partial = serde_json::from_str::<Value>(buf).ok();
                }
                if let Some(block) = i.live.as_mut().and_then(|l| l.pointer_mut(&format!("/message/content/{idx}"))) {
                    match d.get("type").and_then(Value::as_str) {
                        Some("text_delta") => append_text(block, "text", d.get("text").and_then(Value::as_str).unwrap_or("")),
                        Some("thinking_delta") => append_text(block, "thinking", d.get("thinking").and_then(Value::as_str).unwrap_or("")),
                        Some("input_json_delta") => {
                            if let Some(p) = partial {
                                block["input"] = p;
                            }
                        }
                        _ => {}
                    }
                }
            }
            "content_block_stop" => {
                let idx = e.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                if let Some(buf) = i.partial_json.remove(&idx) {
                    if let (Ok(p), Some(block)) = (serde_json::from_str::<Value>(&buf), i.live.as_mut().and_then(|l| l.pointer_mut(&format!("/message/content/{idx}")))) {
                        block["input"] = p;
                    }
                }
                flush = true;
            }
            "message_stop" => flush = true,
            _ => {}
        }
        let every = config::ms("sdk_live_ms", 60);
        if i.live.is_some() && (flush || i.last_live_sent.is_none_or(|t| t.elapsed() >= every)) {
            i.last_live_sent = Some(Instant::now());
            let m = i.live.clone();
            drop(i);
            let _ = self.notices.send(Notice::Live(self.id.clone(), m));
        }
    }
}

fn append_text(block: &mut Value, key: &str, more: &str) {
    let cur = block.get(key).and_then(Value::as_str).unwrap_or("").to_string();
    block[key] = json!(format!("{cur}{more}"));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat() -> (Arc<SdkChat>, Receiver<Notice>) {
        let (tx, rx) = channel();
        let c = Arc::new(SdkChat {
            id: "chat-1".into(),
            stdin: Mutex::new(None),
            child: Mutex::new(None),
            inner: Mutex::new(Inner {
                alive: true,
                state: "idle".into(),
                waiting: None,
                model: None,
                mode: None,
                effort: None,
                session_id: None,
                live: None,
                partial_json: HashMap::new(),
                last_live_sent: None,
            }),
            pending: Mutex::new(HashMap::new()),
            notices: tx,
            seq: Mutex::new(0),
        });
        (c, rx)
    }

    #[test]
    fn a_permission_request_makes_the_chat_wait_with_its_options() {
        let (c, _rx) = chat();
        c.on_message(&json!({"type": "control_request", "request_id": "r1", "request": {"subtype": "can_use_tool", "tool_name": "Bash",
            "input": {"command": "ls"}, "description": "List files", "permission_suggestions": [{"type": "addRules"}], "tool_use_id": "t1"}}));
        let s = c.snapshot();
        assert_eq!(s.state, "waiting");
        let w = s.waiting_on.unwrap();
        assert_eq!(w["request_id"], "r1");
        assert_eq!(w["tool_input"]["command"], "ls");
        assert_eq!(w["message"], "Bash: List files");
        assert_eq!(w["suggestions"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn a_question_shows_its_first_question() {
        let (c, _rx) = chat();
        c.on_message(&json!({"type": "control_request", "request_id": "r2", "request": {"subtype": "can_use_tool", "tool_name": "AskUserQuestion",
            "input": {"questions": [{"question": "Which color?", "options": [{"label": "Red"}, {"label": "Blue"}]}]}}}));
        let w = c.snapshot().waiting_on.unwrap();
        assert_eq!(w["message"], "Which color?");
        assert_eq!(w["tool_input"]["questions"][0]["options"][1]["label"], "Blue");
    }

    #[test]
    fn streaming_builds_the_reply_and_result_ends_the_turn() {
        let (c, rx) = chat();
        c.on_message(&json!({"type": "stream_event", "event": {"type": "message_start", "message": {"id": "m1", "model": "claude-x"}}, "uuid": "u1"}));
        c.on_message(&json!({"type": "stream_event", "event": {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}}));
        c.on_message(&json!({"type": "stream_event", "event": {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Hel"}}}));
        c.on_message(&json!({"type": "stream_event", "event": {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "lo"}}}));
        c.on_message(&json!({"type": "stream_event", "event": {"type": "content_block_start", "index": 1, "content_block": {"type": "tool_use", "id": "t1", "name": "Bash", "input": {}}}}));
        c.on_message(&json!({"type": "stream_event", "event": {"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "{\"comm"}}}));
        c.on_message(&json!({"type": "stream_event", "event": {"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "and\": \"ls\"}"}}}));
        c.on_message(&json!({"type": "stream_event", "event": {"type": "content_block_stop", "index": 1}}));
        let mut last = None;
        while let Ok(n) = rx.try_recv() {
            if let Notice::Live(_, m) = n {
                last = m;
            }
        }
        let live = last.expect("a live message");
        assert_eq!(live["live"], true);
        assert_eq!(live["message"]["model"], "claude-x");
        assert_eq!(live["message"]["content"][0]["text"], "Hello");
        assert_eq!(live["message"]["content"][1]["input"]["command"], "ls");
        assert_eq!(c.snapshot().state, "working");
        c.on_message(&json!({"type": "result", "subtype": "success"}));
        assert_eq!(c.snapshot().state, "idle");
        let ended = std::iter::from_fn(|| rx.try_recv().ok()).any(|n| matches!(n, Notice::Live(_, None)));
        assert!(ended);
    }

    #[test]
    fn a_subagents_stream_is_not_the_chats_reply() {
        let (c, rx) = chat();
        c.on_message(&json!({"type": "stream_event", "parent_tool_use_id": "t9", "event": {"type": "message_start", "message": {"id": "m2"}}}));
        assert!(rx.try_recv().is_err());
        assert!(c.inner.lock().unwrap().live.is_none());
    }

    #[test]
    fn init_records_the_session_model_and_mode() {
        let (c, _rx) = chat();
        c.on_message(&json!({"type": "system", "subtype": "init", "session_id": "s1", "model": "claude-opus-5", "permissionMode": "plan"}));
        let s = c.snapshot();
        assert_eq!(s.session_id.as_deref(), Some("s1"));
        assert_eq!(s.model.as_deref(), Some("claude-opus-5"));
        assert_eq!(s.mode.as_deref(), Some("plan"));
    }
}
