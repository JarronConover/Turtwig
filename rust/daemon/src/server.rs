use agent_box_protocol::{encode, Call, Event, Model, Request, Response, ServerMessage, PROTOCOL_VERSION};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::{agents, config, db, hosts, model, term, util};

struct Watch {
    path: PathBuf,
    offset: u64,
    next_line: usize,
    parse: agents::Parser,
    /** A registry chat whose transcript isn't known yet (a new Codex chat): looked up again until it is. */
    resolve: Option<String>,
}

impl Watch {
    fn new(path: PathBuf, parse: agents::Parser) -> Self {
        Watch { path, offset: 0, next_line: 0, parse, resolve: None }
    }
}

struct Conn {
    tx: Sender<String>,
    subscribed: AtomicBool,
    cursor: AtomicI64,
    watches: Mutex<HashMap<String, Watch>>,
    terms: Mutex<HashMap<u64, term::Term>>,
}

impl Conn {
    fn event(&self, e: Event) {
        let _ = self.tx.send(encode(&ServerMessage::Event(e)));
    }
}

struct Shared {
    conns: Mutex<Vec<Arc<Conn>>>,
    model: Mutex<Option<Model>>,
    states: Mutex<model::ChatStates>,
    last_seq: AtomicI64,
    wake: (Mutex<bool>, Condvar),
    caches: Arc<model::Caches>,
    next_term: AtomicU64,
}

impl Shared {
    fn poke(&self) {
        let (m, c) = &self.wake;
        *m.lock().unwrap() = true;
        c.notify_all();
    }

    fn apply_event(&self, e: &agent_box_protocol::HookEvent) {
        model::apply_hook(&mut self.states.lock().unwrap(), e);
    }
}

pub fn run() -> anyhow::Result<()> {
    config::load();
    hosts::reload();
    let sock = util::socket_path();
    std::fs::create_dir_all(util::agent_home())?;
    if UnixStream::connect(&sock).is_ok() {
        eprintln!("agent-boxd is already running");
        return Ok(());
    }
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock)?;

    let conn = db::open()?;
    let shared = Arc::new(Shared {
        conns: Mutex::new(vec![]),
        model: Mutex::new(None),
        states: Mutex::new(HashMap::new()),
        last_seq: AtomicI64::new(db::max_seq(&conn)),
        wake: (Mutex::new(true), Condvar::new()),
        caches: Arc::new(model::Caches::default()),
        next_term: AtomicU64::new(1),
    });
    for e in db::latest_per_session(&conn) {
        shared.apply_event(&e);
    }
    drop(conn);

    std::thread::spawn(warm_line_starts);
    spawn_loop("events", shared.clone(), event_loop);
    spawn_loop("model", shared.clone(), model_loop);
    spawn_loop("transcripts", shared.clone(), transcript_loop);
    // Only where no app runs (`relay_app_actions`): there, run.json is the app's own to read.
    if hosts::flag("relay_app_actions", hosts::here() != "mac") {
        spawn_loop("app-actions", shared.clone(), app_action_loop);
    }
    ensure_supervisor();
    eprintln!("agent-boxd listening on {}", sock.display());
    for stream in listener.incoming().flatten() {
        let shared = shared.clone();
        std::thread::spawn(move || serve(shared, stream));
    }
    Ok(())
}

fn spawn_loop(name: &str, shared: Arc<Shared>, f: fn(&Arc<Shared>)) {
    std::thread::Builder::new().name(name.into()).spawn(move || loop { f(&shared) }).expect("spawn loop");
}

/** What the Mac app may send here (`app_files`) so agents on this machine can see it as they would on the Mac. */
fn app_files() -> Vec<String> {
    config::get("app_files")
        .and_then(|v| v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()))
        .unwrap_or_else(|| ["inspect.json", "activity.log", "problems.txt", "snapshot.png"].map(String::from).to_vec())
}

/** With `hosts.HOST.supervise`, the tmux session `supervise_session` runs `bin/chat supervise` (boot starts the
same one on the box), so this host's running chats come back after a crash or restart. */
fn ensure_supervisor() {
    if !hosts::flag("supervise", false) {
        return;
    }
    let name = config::text("supervise_session", "agent-box");
    if util::run("tmux", &["has-session", "-t", &format!("={name}")], &[], None, util::tmux_wait()).ok {
        return;
    }
    let log = util::agent_home().join("log");
    let _ = std::fs::create_dir_all(&log);
    let cmd = format!("{} supervise 2>&1 | tee -a {}", util::agent_box_dir().join("bin").join("chat").display(),
                      log.join("supervisor.log").display());
    util::run("tmux", &["new-session", "-d", "-s", &name, &cmd], &[], None, util::tmux_wait());
}

fn app_dir() -> std::path::PathBuf {
    util::agent_home().join("state").join("settings")
}

/** `abx do` here writes run.json in the settings folder; it goes to the connected app, which runs it. */
fn app_action_loop(shared: &Arc<Shared>) {
    std::thread::sleep(config::ms("app_action_poll_ms", 300));
    let path = app_dir().join("run.json");
    let Ok(text) = std::fs::read_to_string(&path) else { return };
    let subscribed: Vec<Arc<Conn>> = shared.conns.lock().unwrap().iter().filter(|c| c.subscribed.load(Ordering::SeqCst)).cloned().collect();
    if subscribed.is_empty() {
        return;
    }
    let _ = std::fs::remove_file(&path);
    let action = serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text.trim().to_string()));
    for c in subscribed {
        c.event(Event::AppAction { action: action.clone() });
    }
}

/** New hook rows: update chat states, fan out to subscribers, and wake the model loop. */
fn event_loop(shared: &Arc<Shared>) {
    let Ok(conn) = db::open() else {
        std::thread::sleep(Duration::from_secs(2));
        return;
    };
    loop {
        std::thread::sleep(config::ms("events_poll_ms", 250));
        let seq = shared.last_seq.load(Ordering::SeqCst);
        let rows = db::since(&conn, seq, config::count("events_batch", 500).max(1) as i64);
        if rows.is_empty() {
            continue;
        }
        for e in &rows {
            shared.apply_event(e);
        }
        let last = rows.last().map(|e| e.seq).unwrap_or(seq);
        shared.last_seq.store(last, Ordering::SeqCst);
        for c in shared.conns.lock().unwrap().iter() {
            if !c.subscribed.load(Ordering::SeqCst) {
                continue;
            }
            let cursor = c.cursor.load(Ordering::SeqCst);
            for e in rows.iter().filter(|e| e.seq > cursor) {
                c.event(Event::Hook(e.clone()));
            }
            c.cursor.store(last, Ordering::SeqCst);
        }
        shared.poke();
    }
}

/** Rebuild the model when poked (debounced) or every 2 s, and push it when it changed. */
fn model_loop(shared: &Arc<Shared>) {
    {
        let (m, c) = &shared.wake;
        let mut woke = m.lock().unwrap();
        if !*woke {
            woke = c.wait_timeout(woke, config::ms("model_poll_ms", 2000)).unwrap().0;
        }
        *woke = false;
    }
    std::thread::sleep(config::ms("model_debounce_ms", 250));
    // Where `build_unwatched` is off (the Mac, where every build polls super.engineering), build only while
    // someone is watching.
    if !hosts::flag("build_unwatched", !cfg!(target_os = "macos"))
        && !shared.conns.lock().unwrap().iter().any(|c| c.subscribed.load(Ordering::SeqCst)) {
        *shared.model.lock().unwrap() = None;
        return;
    }
    let states = shared.states.lock().unwrap().clone();
    let next = model::build(&shared.caches, &states, shared.last_seq.load(Ordering::SeqCst));
    let changed = shared.model.lock().unwrap().as_ref() != Some(&next);
    if changed {
        *shared.model.lock().unwrap() = Some(next.clone());
        for c in shared.conns.lock().unwrap().iter().filter(|c| c.subscribed.load(Ordering::SeqCst)) {
            c.event(Event::Model(next.clone()));
        }
    }
}

/** Complete new lines of watched transcripts, parsed. */
fn read_lines(w: &mut Watch) -> Vec<Value> {
    let Ok(mut f) = std::fs::File::open(&w.path) else { return vec![] };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len <= w.offset {
        return vec![];
    }
    let mut buf = Vec::new();
    if f.seek(SeekFrom::Start(w.offset)).is_err() || f.read_to_end(&mut buf).is_err() {
        return vec![];
    }
    let Some(end) = buf.iter().rposition(|b| *b == b'\n') else { return vec![] };
    w.offset += end as u64 + 1;
    let out: Vec<Value> = buf[..end].split(|b| *b == b'\n').map(|l| (w.parse)(l)).collect();
    w.next_line += out.len();
    out
}

/** Where each line of a transcript starts, per file, extended as the file grows: reading the newest lines
or an older page seeks straight to them instead of reading the whole file (some run past 100MB). */
static LINE_STARTS: Mutex<Option<HashMap<PathBuf, Vec<u64>>>> = Mutex::new(None);

/** The file's complete lines as byte offsets: entry i starts at `starts[i]`; the last entry is the end. */
fn line_starts(path: &PathBuf) -> Vec<u64> {
    let known = LINE_STARTS.lock().unwrap().as_ref().and_then(|m| m.get(path).cloned());
    let mut starts = known.unwrap_or_else(|| vec![0]);
    let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if len < *starts.last().unwrap_or(&0) {
        starts = vec![0];
    }
    let from = *starts.last().unwrap_or(&0);
    if len > from {
        if let Ok(mut f) = std::fs::File::open(path) {
            let mut buf = Vec::new();
            if f.seek(SeekFrom::Start(from)).is_ok() && f.read_to_end(&mut buf).is_ok() {
                starts.extend(buf.iter().enumerate().filter(|(_, b)| **b == b'\n').map(|(i, _)| from + i as u64 + 1));
            }
        }
        LINE_STARTS.lock().unwrap().get_or_insert_with(HashMap::new).insert(path.clone(), starts.clone());
    }
    starts
}

/** Indexes the newest chats' transcripts in the background, so opening one doesn't wait for a first scan. */
fn warm_line_starts() {
    let dir = util::agent_home().join("state").join("registry").join("chats");
    let mut chats: Vec<(std::time::SystemTime, Value)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let t = e.metadata().and_then(|m| m.modified()).ok()?;
            Some((t, serde_json::from_str(&std::fs::read_to_string(e.path()).ok()?).ok()?))
        })
        .collect();
    chats.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, c) in chats.into_iter().take(config::count("warm_index_chats", 60)) {
        if let Some(path) = agents::transcript_path(&c) {
            line_starts(&path);
        }
    }
}

/** Entries `from..to` (line numbers), parsed. */
fn read_range(path: &PathBuf, starts: &[u64], from: usize, to: usize, parse: &agents::Parser) -> Vec<Value> {
    let lines = starts.len().saturating_sub(1);
    let (from, to) = (from.min(lines), to.min(lines));
    if from >= to {
        return vec![];
    }
    let Ok(mut f) = std::fs::File::open(path) else { return vec![] };
    let mut buf = vec![0u8; (starts[to] - starts[from]) as usize];
    if f.seek(SeekFrom::Start(starts[from])).is_err() || f.read_exact(&mut buf).is_err() {
        return vec![];
    }
    buf.pop();
    buf.split(|b| *b == b'\n').map(|l| parse(l)).collect()
}

/** The last `n` entries of a transcript and the line they start at; leaves `w` at the end of the file. */
fn read_tail(w: &mut Watch, n: usize) -> (usize, Vec<Value>) {
    let starts = line_starts(&w.path);
    let total = starts.len() - 1;
    let skip = total.saturating_sub(n);
    let entries = read_range(&w.path, &starts, skip, total, &w.parse);
    w.offset = starts[total];
    w.next_line = total;
    (skip, entries)
}

/** Entries from line `from` on; leaves `w` at the end of the file. */
fn read_from(w: &mut Watch, from: usize) -> Vec<Value> {
    let starts = line_starts(&w.path);
    let total = starts.len() - 1;
    let entries = read_range(&w.path, &starts, from, total, &w.parse);
    w.offset = starts[total];
    w.next_line = total;
    entries
}

/** A transcript line as the app needs it: without what it never shows (request blobs, image bytes in
tool results, the `toolUseResult` copy), and long tool output cut to `tool_text_max`, so a chat full of
screenshots opens in a fraction of the bytes. */
pub(crate) fn parse_entry(line: &[u8]) -> Value {
    let Ok(mut v) = serde_json::from_slice::<Value>(line) else { return Value::Null };
    match v.get("type").and_then(Value::as_str) {
        Some("user" | "assistant" | "system") => {}
        // Background work finishing (its <task-notification>), whether or not the agent has read it yet.
        Some("queue-operation") => {
            let mut text = v.get("content").and_then(Value::as_str).unwrap_or("").to_string();
            cut(&mut text);
            return json!({ "type": "queue-operation", "operation": v.get("operation"), "content": text });
        }
        _ => return json!({ "type": v.get("type").cloned().unwrap_or(Value::Null) }),
    }
    if let Some(o) = v.as_object_mut().filter(|_| config::flag("drop_tool_use_result", true)) {
        o.remove("toolUseResult");
    }
    let drop_images = config::flag("drop_tool_images", true);
    if let Some(blocks) = v.pointer_mut("/message/content").and_then(Value::as_array_mut) {
        for b in blocks.iter_mut().filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result")) {
            match b.get_mut("content") {
                Some(Value::Array(parts)) => {
                    parts.retain(|p| !drop_images || p.get("type").and_then(Value::as_str) != Some("image"));
                    for p in parts.iter_mut() {
                        if let Some(Value::String(t)) = p.get_mut("text") {
                            cut(t);
                        }
                    }
                }
                Some(Value::String(t)) => cut(t),
                _ => {}
            }
        }
    }
    v
}

/** Long tool output cut to `tool_text_max` characters (0: never), with `tool_text_cut_note` after. */
pub(crate) fn cut(t: &mut String) {
    let max = config::count("tool_text_max", 20_000);
    if max > 0 && t.len() > max {
        let mut at = max;
        while !t.is_char_boundary(at) {
            at -= 1;
        }
        t.truncate(at);
        t.push_str(&config::text("tool_text_cut_note", "\n… (cut here; the full output is in the transcript file)"));
    }
}

fn transcript_loop(shared: &Arc<Shared>) {
    std::thread::sleep(config::ms("transcripts_poll_ms", 300));
    for c in shared.conns.lock().unwrap().iter() {
        let mut watches = c.watches.lock().unwrap();
        for (chat_id, w) in watches.iter_mut() {
            if let Some(id) = w.resolve.clone() {
                match registry_chat(&id).as_ref().and_then(agents::transcript_path) {
                    Some(p) => (w.path, w.resolve) = (p, None),
                    None => continue,
                }
            }
            let from = w.next_line;
            let entries = read_lines(w);
            if !entries.is_empty() {
                c.event(Event::Transcript { chat_id: chat_id.clone(), from, entries });
            }
        }
    }
}

fn serve(shared: Arc<Shared>, stream: UnixStream) {
    let (tx, rx) = channel::<String>();
    let Ok(mut out) = stream.try_clone() else { return };
    std::thread::spawn(move || {
        for line in rx {
            if out.write_all(line.as_bytes()).is_err() {
                break;
            }
        }
    });
    let conn = Arc::new(Conn {
        tx,
        subscribed: AtomicBool::new(false),
        cursor: AtomicI64::new(0),
        watches: Mutex::new(HashMap::new()),
        terms: Mutex::new(HashMap::new()),
    });
    shared.conns.lock().unwrap().push(conn.clone());
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let req: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let id = serde_json::from_str::<Value>(&line).ok().and_then(|v| v.get("id")?.as_u64()).unwrap_or(0);
                let _ = conn.tx.send(encode(&ServerMessage::Response(Response {
                    id,
                    result: None,
                    error: Some(format!("bad request: {e}")),
                })));
                continue;
            }
        };
        let (shared, conn) = (shared.clone(), conn.clone());
        std::thread::spawn(move || {
            let id = req.id;
            let res = handle(&shared, &conn, req.call);
            let (result, error) = match res {
                Ok(v) => (Some(v), None),
                Err(e) => (None, Some(e)),
            };
            let _ = conn.tx.send(encode(&ServerMessage::Response(Response { id, result, error })));
        });
    }
    shared.conns.lock().unwrap().retain(|c| !Arc::ptr_eq(c, &conn));
    for (_, t) in conn.terms.lock().unwrap().drain() {
        t.close();
    }
}

/** bin/chat with `args`, waiting up to the option `timeout_key` (else `default` seconds). */
fn chat_tool(args: &[&str], timeout_key: &str, default: u64) -> Result<Value, String> {
    bin_tool("chat", args, timeout_key, default)
}

fn bin_tool(name: &str, args: &[&str], timeout_key: &str, default: u64) -> Result<Value, String> {
    let tool = util::agent_box_dir().join("bin").join(name);
    let o = util::run(&tool.to_string_lossy(), args, &[], None, config::secs(timeout_key, default));
    if o.ok { Ok(json!({ "stdout": o.stdout })) } else { Err(o.message()) }
}

fn sc_tool(args: &[&str], path: Option<&str>) -> Result<Value, String> {
    let shim = util::agent_box_dir().join("shim").join("sc");
    let mut all = vec![shim.to_string_lossy().into_owned()];
    all.extend(args.iter().map(|s| s.to_string()));
    all.push("--json".into());
    let refs: Vec<&str> = all.iter().map(|s| s.as_str()).collect();
    let home = util::home();
    let cwd = path.map(|p| p.to_string()).unwrap_or_else(|| home.to_string_lossy().into_owned());
    let envs: Vec<(&str, &str)> = path.map(|p| vec![("SUPER_ENGINEERING_WORKTREE_PATH", p)]).unwrap_or_default();
    let o = util::run("python3", &refs, &envs, Some(&cwd), config::secs("sc_timeout_secs", 600));
    if o.ok {
        Ok(serde_json::from_str(&o.stdout).unwrap_or(json!({ "stdout": o.stdout })))
    } else {
        Err(o.message())
    }
}

/** A subagent's transcript next to its parent's: `<session>/subagents/agent-<id>.jsonl`, found by agent id
or by the `toolUseId` in its `.meta.json`. */
fn subagent_path(parent: &std::path::Path, agent: &str) -> Option<PathBuf> {
    let dir = parent.with_extension("").join("subagents");
    let direct = dir.join(format!("agent-{agent}.jsonl"));
    if direct.exists() {
        return Some(direct);
    }
    for e in std::fs::read_dir(&dir).ok()?.flatten() {
        let p = e.path();
        if p.to_string_lossy().ends_with(".meta.json") {
            let meta: Value = serde_json::from_str(&std::fs::read_to_string(&p).ok()?).ok()?;
            if meta.get("toolUseId").and_then(Value::as_str) == Some(agent) {
                return Some(PathBuf::from(p.to_string_lossy().replace(".meta.json", ".jsonl")));
            }
        }
    }
    None
}

fn registry_chat(chat_id: &str) -> Option<Value> {
    let dir = util::agent_home().join("state").join("registry").join("chats");
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with(chat_id) && name.ends_with(".json") {
            return serde_json::from_str(&std::fs::read_to_string(e.path()).ok()?).ok();
        }
    }
    None
}

fn handle(shared: &Arc<Shared>, conn: &Arc<Conn>, call: Call) -> Result<Value, String> {
    let result = match call {
        Call::Hello { .. } => {
            let here = hosts::here();
            return Ok(json!({
                "version": PROTOCOL_VERSION, "host": here, "host_id": here, "label": hosts::label(&here),
                "build": env!("CARGO_PKG_VERSION"), "commit": env!("AGENT_BOXD_COMMIT"),
            }));
        }
        Call::Subscribe { from_seq } => {
            let current = shared.model.lock().unwrap().clone();
            let model = match current {
                Some(m) => m,
                None => {
                    let states = shared.states.lock().unwrap().clone();
                    let m = model::build(&shared.caches, &states, shared.last_seq.load(Ordering::SeqCst));
                    *shared.model.lock().unwrap() = Some(m.clone());
                    m
                }
            };
            conn.event(Event::Model(model));
            let last = shared.last_seq.load(Ordering::SeqCst);
            if let Some(from) = from_seq {
                if let Ok(db) = db::open() {
                    let max = config::count("subscribe_replay_max", 1000) as i64;
                    for e in db::since(&db, from, max).into_iter().filter(|e| e.seq <= last) {
                        conn.event(Event::Hook(e));
                    }
                }
            }
            conn.cursor.store(last, Ordering::SeqCst);
            conn.subscribed.store(true, Ordering::SeqCst);
            return Ok(json!({ "last_seq": last }));
        }
        Call::TranscriptRead { chat_id, from, tail, before } => {
            // `sub:PARENT:AGENT`: a subagent's transcript, beside its parent's (AGENT is its id or the
            // tool_use id that started it).
            let (lookup, agent) = match chat_id.strip_prefix("sub:").and_then(|r| r.split_once(':')) {
                Some((parent, agent)) => (parent.to_string(), Some(agent.to_string())),
                None => (chat_id.clone(), None),
            };
            let chat = registry_chat(&lookup).ok_or("no such chat")?;
            let reg_id = chat.get("id").and_then(|v| v.as_str()).unwrap_or(&lookup).to_string();
            let kind = agents::of(&chat);
            let codex = agents::format(&kind) == "codex";
            if !codex && chat.get("dir").and_then(Value::as_str).is_none() {
                return Err("chat has no folder".into());
            }
            let found = agents::transcript_path(&chat);
            let mut path = found.clone().unwrap_or_default();
            let id = match &agent {
                // A Codex subagent is a session of its own (AGENT: its thread id).
                Some(a) if codex => {
                    path = crate::codex::find_rollout(&agents::sessions_dir(&kind), a).ok_or("no transcript for that subagent yet")?;
                    chat_id.clone()
                }
                Some(a) => {
                    path = subagent_path(&path, a).ok_or("no transcript for that subagent yet")?;
                    chat_id.clone()
                }
                None => reg_id.clone(),
            };
            let mut w = Watch::new(path.clone(), agents::parser(&kind));
            if found.is_none() && agent.is_none() {
                w.resolve = Some(reg_id);
            }
            // An older page: lines before `before`, leaving the watch alone.
            if let Some(b) = before {
                let n = tail.unwrap_or(config::count("transcript_page", 600));
                let starts = line_starts(&path);
                let start = b.saturating_sub(n);
                return Ok(json!({ "entries": read_range(&path, &starts, start, b, &w.parse), "from": start }));
            }
            let (from, entries) = match tail.filter(|_| from == 0) {
                Some(n) => read_tail(&mut w, n),
                None => (from, read_from(&mut w, from)),
            };
            let next = w.next_line;
            conn.watches.lock().unwrap().insert(id, w);
            return Ok(json!({ "entries": entries, "from": from, "next": next, "path": path }));
        }
        Call::TranscriptUnwatch { chat_id } => {
            conn.watches.lock().unwrap().retain(|k, _| !k.starts_with(&chat_id));
            return Ok(json!({}));
        }
        Call::ChatNew { name, kind, branch, base, dir, repo, prompt, model, agent, auto_name } => {
            let mut args: Vec<String> = vec!["new".into(), name, "--kind".into(), kind];
            if auto_name {
                args.push("--auto-name".into());
            }
            for (flag, v) in [("--branch", branch), ("--base", base), ("--dir", dir), ("--repo", repo),
                              ("--prompt", prompt), ("--model", model), ("--agent", agent)] {
                if let Some(v) = v.filter(|v| !v.is_empty()) {
                    args.push(flag.into());
                    args.push(v);
                }
            }
            let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            chat_tool(&refs, "chat_new_timeout_secs", 900)
        }
        Call::History { limit } => {
            let limit = limit.unwrap_or(config::count("history_limit", 200));
            return Ok(history(limit, None));
        }
        Call::HistoryQuery { limit, host } => {
            let limit = limit.unwrap_or(config::count("history_limit", 200));
            return Ok(history(limit, host.as_deref()));
        }
        Call::ChatFork { chat_id } => chat_tool(&["fork", &chat_id], "chat_timeout_secs", 120),
        Call::ChatStart { chat_id } => chat_tool(&["start", &chat_id], "chat_start_timeout_secs", 300),
        Call::ChatStop { chat_id } => chat_tool(&["stop", &chat_id], "chat_timeout_secs", 120),
        Call::ChatArchive { chat_id } => chat_tool(&["archive", &chat_id], "chat_timeout_secs", 120),
        Call::ChatRename { chat_id, name } => chat_tool(&["rename", &chat_id, &name], "chat_timeout_secs", 120),
        Call::ChatMove { chat_id, to, scope, dry_run } => {
            let mut args = vec!["move", chat_id.as_str(), "--to", to.as_str()];
            if let Some(s) = scope.as_deref() {
                args.extend(["--scope", s]);
            }
            if dry_run {
                args.push("--dry-run");
            }
            chat_tool(&args, "chat_move_timeout_secs", 1800)
        }
        Call::TestLocal { chat_id, branch, repo, from } => {
            let mut args = vec!["test-local".to_string()];
            args.extend(chat_id);
            for (flag, v) in [("--branch", branch), ("--repo", repo), ("--from", from)] {
                if let Some(v) = v.filter(|v| !v.is_empty()) {
                    args.extend([flag.to_string(), v]);
                }
            }
            let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            chat_tool(&refs, "chat_move_timeout_secs", 1800)
        }
        Call::StackStart { chat_id } => {
            let session = format!("stack-{}", &chat_id[..chat_id.len().min(8)]);
            let chat = util::agent_box_dir().join("bin").join("chat");
            let cmd = format!("{} stack-start {}", chat.display(), chat_id);
            let o = util::run("tmux", &["new-session", "-d", "-s", &session, &cmd], &[], None, util::tmux_wait());
            if o.ok { Ok(json!({ "session": session })) } else { Err(o.message()) }
        }
        Call::ChatSend { chat_id, text } => {
            // Pasted as one bracketed paste, then Enter: Claude Code sees exactly where the paste ends, so
            // the Enter submits it with no wait (typing it in took a half-second pause and a Python start).
            let target = format!("={}:", util::chat_session(&chat_id));
            let ok = |args: &[&str]| util::run("tmux", args, &[], None, util::tmux_wait() * 2).ok;
            let chat = registry_chat(&chat_id);
            let kind = chat.as_ref().map(agents::of).unwrap_or_else(|| "claude".into());
            if !ok(&["has-session", "-t", &target]) {
                // Not running: bin/chat resumes it first (send_resumes), which can take a while for a long chat.
                return chat_tool(&["send", &chat_id, &text], "chat_send_timeout_secs", 150);
            }
            let buf = format!("abx-{}", &chat_id[..chat_id.len().min(8)]);
            if !(ok(&["set-buffer", "-b", &buf, "--", &text]) && ok(&["paste-buffer", "-p", "-d", "-b", &buf, "-t", &target])) {
                return Err("couldn't type into the chat".into());
            }
            // Claude Code can drop an Enter that lands while it's still taking the paste in: press it, then
            // again (a few times at most) while the prompt line still holds the text. Not for a slash command:
            // it can open a menu (/artifacts, /model), slow to show, where another Enter picks the first entry.
            std::thread::sleep(config::ms("send_paste_wait_ms", 150));
            let prompt = agents::text(&kind, "prompt_char", "❯");
            let tries = if text.starts_with('/') { 1 } else { config::count("send_enter_tries", 4).max(1) };
            let head: String = text.trim().lines().next().unwrap_or("").chars().take(20).collect();
            for _ in 0..tries {
                if !ok(&["send-keys", "-t", &target, "C-m"]) {
                    return Err("couldn't submit the message".into());
                }
                std::thread::sleep(config::ms("send_enter_wait_ms", 350));
                if prompt.is_empty() {
                    break;
                }
                let pane = util::run("tmux", &["capture-pane", "-p", "-e", "-t", &target], &[], None, util::tmux_wait()).stdout;
                // A menu's selected row starts with the prompt too: retry only while the input holds this text.
                if model::typed_input(&pane, &prompt).is_none_or(|t| t.is_empty() || !t.starts_with(&head)) {
                    break;
                }
            }
            // A new Codex chat's session (and rollout) starts with its first message: record it now.
            if chat.as_ref().is_some_and(|c| agents::format(&kind) == "codex" && c.get("session_id").is_none()) {
                let _ = chat_tool(&["_session", &chat_id, "--background"], "chat_session_timeout_secs", 10);
            }
            Ok(json!({}))
        }
        Call::ChatKeys { chat_id, keys } => {
            let target = format!("={}:", util::chat_session(&chat_id));
            let kind = registry_chat(&chat_id).as_ref().map(agents::of).unwrap_or_else(|| "claude".into());
            let prompt = agents::text(&kind, "prompt_char", "❯");
            let typed = || model::typed_input(&util::run("tmux", &["capture-pane", "-p", "-e", "-t", &target], &[], None, util::tmux_wait()).stdout, &prompt).unwrap_or_default();
            let clear: Vec<String> = agents::get(&kind, "clear_input_keys").and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
            let watch = keys == ["Escape"] && !clear.is_empty() && !prompt.is_empty() && typed().is_empty();
            let mut args = vec!["send-keys", "-t", target.as_str()];
            args.extend(keys.iter().map(|k| k.as_str()));
            let o = util::run("tmux", &args, &[], None, util::tmux_wait() * 2);
            // Esc right after a message is sent takes it back: Claude Code drops it from the conversation and
            // puts it back in its input, where the next message would be appended to it. Clear it there and
            // say what it began with, for the app to put the message back in its own box.
            let mut restored = None;
            if o.ok && watch {
                let until = std::time::Instant::now() + config::ms("escape_restore_wait_ms", 1200);
                while std::time::Instant::now() < until {
                    std::thread::sleep(std::time::Duration::from_millis(150));
                    let t = typed();
                    if !t.is_empty() {
                        // Its first line goes last: press until that's gone.
                        for _ in 0..config::count("clear_input_rounds", 20) {
                            let mut args = vec!["send-keys", "-t", target.as_str()];
                            args.extend(std::iter::repeat_n(clear.iter().map(|k| k.as_str()), 10).flatten());
                            util::run("tmux", &args, &[], None, util::tmux_wait() * 2);
                            std::thread::sleep(std::time::Duration::from_millis(100));
                            if typed().is_empty() {
                                break;
                            }
                        }
                        restored = Some(t);
                        break;
                    }
                }
            }
            // Esc ends the turn, and no hook says so: the chat is idle from here.
            if o.ok && keys == ["Escape"] && config::flag("escape_marks_idle", true) {
                let sid = registry_chat(&chat_id).as_ref().and_then(agents::session_key).unwrap_or(chat_id.clone());
                shared.states.lock().unwrap().insert(sid, ("idle".into(), None));
            }
            if o.ok { Ok(json!({ "restored": restored })) } else { Err(o.message()) }
        }
        Call::SectionCreate { name } => sc_tool(&["section", "create", &name, "--manual"], None),
        Call::SectionCollapse { id, collapsed } => {
            sc_tool(&["section", "edit", &id, "--collapsed", if collapsed { "true" } else { "false" }], None)
        }
        Call::WorktreeSetTarget { path, branch } => sc_tool(&["worktree", "set-target-branch", &branch], Some(&path)),
        Call::WorktreeCreateWith { repo, branch, base, name } => {
            let mut args = vec!["worktree", "create", "--project", repo.as_str(), "--branch", branch.as_str()];
            if let Some(b) = base.as_deref().filter(|b| !b.is_empty()) {
                args.extend(["--base", b]);
            }
            if let Some(n) = name.as_deref().filter(|n| !n.is_empty()) {
                args.extend(["--worktree-name", n]);
            }
            sc_tool(&args, None)
        }
        Call::SectionRename { id, name } => sc_tool(&["section", "edit", &id, "--name", &name], None),
        Call::SectionDelete { id } => sc_tool(&["section", "delete", &id], None),
        Call::SectionAssign { path, section } => match section {
            Some(s) => sc_tool(&["section", "assign", &s], Some(&path)),
            None => sc_tool(&["section", "unassign"], Some(&path)),
        },
        Call::WorktreeDelete { path } => {
            let ws = sc_tool(&["workspace", "list"], None)?;
            let id = find_item_id(&ws, &path).ok_or("no such worktree")?;
            sc_tool(&["worktree", "delete", &id], None)
        }
        Call::WorktreeMove { path, section, before } => {
            let ws = sc_tool(&["workspace", "list"], None)?;
            let id = find_item_id(&ws, &path).ok_or("no such worktree")?;
            let section = section.unwrap_or_else(|| "none".into());
            let mut args = vec!["worktree", "move", id.as_str(), "--section", section.as_str()];
            if let Some(b) = before.as_deref() {
                args.extend(["--before", b]);
            }
            sc_tool(&args, None)
        }
        Call::WorktreeLabel { path, label } => {
            let ws = sc_tool(&["workspace", "list"], None)?;
            let id = find_item_id(&ws, &path).ok_or("no such worktree")?;
            sc_tool(&["worktree", "set-label", &id, &label], None)
        }
        Call::WorktreeRenameBranch { path, branch } => sc_tool(&["worktree", "rename-branch", &branch], Some(&path)),
        Call::WorktreeCreate { repo, branch } => sc_tool(&["worktree", "create", "--project", &repo, "--branch", &branch], None),
        Call::Exec { cmd, cwd, timeout_secs } => {
            if !config::flag("exec_allowed", true) {
                return Err("exec is off on this host (option exec_allowed)".into());
            }
            let home = util::home();
            let cwd = cwd.unwrap_or_else(|| home.to_string_lossy().into_owned());
            let env = util::agent_box_dir().join("bin").join("agent-env");
            let timeout = timeout_secs.map(Duration::from_secs).unwrap_or_else(|| config::secs("exec_timeout_secs", 120));
            let o = util::run(&env.to_string_lossy(), &["bash", "-lc", &cmd], &[], Some(&cwd), timeout);
            Ok(json!({ "code": if o.ok { 0 } else { 1 }, "stdout": o.stdout, "stderr": o.stderr }))
        }
        Call::HooksInstall { agent } => bin_tool("agent-hooks", &[&agent], "hooks_timeout_secs", 60),
        Call::EnvPush { host } => {
            let args: Vec<&str> = host.as_deref().into_iter().collect();
            bin_tool("env-push", &args, "env_push_timeout_secs", 300)
        }
        Call::SyncNow => {
            util::spawn_detached(&util::agent_box_dir().join("bin").join("sync-worker"), &[]).map_err(|e| e.to_string())?;
            return Ok(json!({}));
        }
        Call::WipList { path } => {
            let mut args = vec!["_wip_list"];
            if let Some(p) = path.as_deref() {
                args.extend(["--path", p]);
            }
            let out = chat_tool(&args, "wip_timeout_secs", 120)?;
            let text = out.get("stdout").and_then(Value::as_str).unwrap_or("[]");
            return Ok(json!({ "wips": serde_json::from_str::<Value>(text).unwrap_or(json!([])) }));
        }
        Call::WipRestore { path, from } => {
            let mut args = vec![path.as_str()];
            if let Some(f) = from.as_deref() {
                args.extend(["--from", f]);
            }
            bin_tool("wip-restore", &args, "wip_timeout_secs", 120)
        }
        Call::ConfigGet => {
            let agents: serde_json::Map<String, Value> = agents::names().into_iter().map(|a| {
                let v = agents::merged(&a);
                (a, v)
            }).collect();
            return Ok(json!({
                "options": config::all(), "agents": agents, "resolved": hosts::resolved(), "host_id": hosts::here(),
            }));
        }
        Call::HookEvents { session_id, from_seq, since, limit } => {
            let db = db::open().map_err(|e| e.to_string())?;
            let limit = limit.unwrap_or(config::count("hook_events_limit", 1000));
            let events = db::query(&db, session_id.as_deref(), from_seq, since, limit as i64);
            return Ok(json!({ "events": events, "last_seq": shared.last_seq.load(Ordering::SeqCst) }));
        }
        Call::LogTail { name, lines } => {
            let path = hosts::log_path(&name).ok_or_else(|| format!("no log named {name} (option logs)"))?;
            let path = agents::expand(&path);
            let text = util::tail(&path, lines.unwrap_or(config::count("log_tail_lines", 200)));
            return Ok(json!({ "name": name, "path": path, "text": text }));
        }
        Call::TermList { cwd } => return Ok(json!({ "terms": util::shell_sessions(cwd.as_deref()) })),
        Call::SectionMove { id, index } => sc_tool(&["section", "move", &id, "--index", &index.to_string()], None),
        Call::ProjectMove { repo, index } => sc_tool(&["project", "move", &repo, "--index", &index.to_string()], None),
        Call::WorktreeDiff { path, base } => crate::diff::worktree_diff(&path, base.as_deref()),
        Call::Configure { options, own, app } => {
            match (own, app) {
                (Some(own), Some(app)) => crate::config::configure(options, own, &app),
                _ => crate::config::set(options),
            }
            hosts::reload();
            ensure_supervisor();
            return Ok(json!({}));
        }
        Call::TermScroll { session, lines } => {
            let target = format!("={session}:");
            let tmux = |args: &[&str]| util::run("tmux", args, &[], None, util::tmux_wait());
            if lines == 0 {
                tmux(&["send-keys", "-t", &target, "-X", "cancel"]);
            } else {
                let n = lines.unsigned_abs().to_string();
                if lines > 0 {
                    tmux(&["copy-mode", "-e", "-t", &target]);
                    tmux(&["send-keys", "-t", &target, "-X", "-N", &n, "scroll-up"]);
                } else if tmux(&["display-message", "-p", "-t", &target, "#{pane_in_mode}"]).stdout.trim() == "1" {
                    tmux(&["send-keys", "-t", &target, "-X", "-N", &n, "scroll-down"]);
                }
            }
            return Ok(json!({}));
        }
        Call::Usage => return Ok(crate::usage::scan()),
        Call::AppFile { name, data } => {
            if !app_files().contains(&name) {
                return Err(format!("not an app file: {name}"));
            }
            use base64::Engine as _;
            let bytes = base64::engine::general_purpose::STANDARD.decode(data).map_err(|e| e.to_string())?;
            let dir = app_dir();
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let tmp = dir.join(format!(".{name}"));
            std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, dir.join(&name))).map_err(|e| e.to_string())?;
            return Ok(json!({}));
        }
        Call::TermKill { session } => {
            if session.starts_with("sh-") || session.starts_with("stack-") {
                util::run("tmux", &["kill-session", "-t", &format!("={session}")], &[], None, util::tmux_wait());
            }
            return Ok(json!({}));
        }
        Call::TermOpen { session, cwd, cols, rows, name, command } => {
            let session = match (session, cwd) {
                (Some(s), _) => s,
                (None, Some(c)) => util::shell_session(&c, name.as_deref(), command.as_deref()),
                (None, None) => return Err("term_open needs a session or a cwd".into()),
            };
            let term_id = shared.next_term.fetch_add(1, Ordering::SeqCst);
            let t = term::open(term_id, &session, cols.max(2), rows.max(1), conn.tx.clone()).map_err(|e| e.to_string())?;
            conn.terms.lock().unwrap().insert(term_id, t);
            return Ok(json!({ "term_id": term_id, "session": session }));
        }
        Call::TermInput { term_id, data } => {
            let mut terms = conn.terms.lock().unwrap();
            let t = terms.get_mut(&term_id).ok_or("no such terminal")?;
            t.input(&data).map_err(|e| e.to_string())?;
            return Ok(json!({}));
        }
        Call::TermResize { term_id, cols, rows } => {
            let terms = conn.terms.lock().unwrap();
            let t = terms.get(&term_id).ok_or("no such terminal")?;
            t.resize(cols.max(2), rows.max(1)).map_err(|e| e.to_string())?;
            return Ok(json!({}));
        }
        Call::TermClose { term_id } => {
            if let Some(t) = conn.terms.lock().unwrap().remove(&term_id) {
                t.close();
            }
            return Ok(json!({}));
        }
    };
    shared.poke();
    result
}

/** Registry chats of `host` (None: this one; "*": every host), archived ones too, newest first. Another host's
last activity is its registry entry's last update. */
fn history(limit: usize, host: Option<&str>) -> Value {
    let dir = util::agent_home().join("state").join("registry").join("chats");
    let alive = util::tmux_sessions();
    let here = hosts::here();
    let want = host.unwrap_or(&here).to_string();
    let mut chats: Vec<Value> = vec![];
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let Some(c) = std::fs::read_to_string(e.path()).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else {
            continue;
        };
        let s = |k: &str| c.get(k).and_then(|v| v.as_str()).map(|v| v.to_string());
        let (Some(id), dir) = (s("id"), s("dir")) else { continue };
        let host = s("host").unwrap_or_else(model::registry_default_host);
        if want != "*" && host != want {
            continue;
        }
        let local = host == here;
        let alive = local && alive.contains(&util::chat_session(&id));
        let last = if local { agents::transcript_path(&c).and_then(|p| util::mtime(&p)) } else { util::mtime(&e.path()) };
        chats.push(json!({
            "id": id, "name": s("name").unwrap_or_default(), "dir": dir, "host": host, "last_activity": last,
            "closed": s("status").as_deref() != Some("running") || (local && !alive),
            "agent": agents::of(&c), "kind": s("kind"), "branch": s("branch"), "status": s("status"),
        }));
    }
    let t = |v: &Value| v.get("last_activity").and_then(|x| x.as_f64()).unwrap_or(0.0);
    chats.sort_by(|a, b| t(b).total_cmp(&t(a)));
    chats.truncate(limit);
    json!({ "chats": chats })
}

fn find_item_id(v: &Value, path: &str) -> Option<String> {
    match v {
        Value::Object(m) => {
            if m.get("path").and_then(|p| p.as_str()) == Some(path) {
                if let Some(id) = m.get("id").and_then(|i| i.as_str()) {
                    return Some(id.to_string());
                }
            }
            m.values().find_map(|x| find_item_id(x, path))
        }
        Value::Array(a) => a.iter().find_map(|x| find_item_id(x, path)),
        _ => None,
    }
}

/** `agent-boxd connect`: relay stdio to the daemon, starting it first if needed. */
pub fn connect() -> anyhow::Result<()> {
    config::load();
    let sock = util::socket_path();
    let stream = match UnixStream::connect(&sock) {
        Ok(s) => s,
        Err(_) => {
            start_detached()?;
            let mut tries = 0;
            loop {
                std::thread::sleep(config::ms("connect_wait_ms", 200));
                if let Ok(s) = UnixStream::connect(&sock) {
                    break s;
                }
                tries += 1;
                if tries > config::count("connect_tries", 50) {
                    anyhow::bail!("agent-boxd didn't start (socket {})", sock.display());
                }
            }
        }
    };
    let mut to_daemon = stream.try_clone()?;
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut std::io::stdin().lock(), &mut to_daemon);
        let _ = to_daemon.shutdown(std::net::Shutdown::Write);
    });
    let mut from_daemon = stream;
    let mut stdout = std::io::stdout().lock();
    let mut buf = [0u8; 65536];
    loop {
        let n = from_daemon.read(&mut buf)?;
        if n == 0 {
            break;
        }
        stdout.write_all(&buf[..n])?;
        stdout.flush()?;
    }
    Ok(())
}

fn start_detached() -> anyhow::Result<()> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe()?;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(util::agent_home().join("boxd.log"))?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("run").stdin(std::process::Stdio::null()).stdout(log.try_clone()?).stderr(log);
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn()?;
    Ok(())
}
