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

use crate::{db, model, term, util};

struct Watch {
    path: PathBuf,
    offset: u64,
    next_line: usize,
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
        if e.event == "Notification" && !model::needs_user(&e.payload) {
            return;
        }
        if let (Some(sid), Some(state)) = (&e.session_id, model::state_for_event(&e.event)) {
            let waiting = (state == "waiting").then(|| e.payload.clone());
            self.states.lock().unwrap().insert(sid.clone(), (state.to_string(), waiting));
        }
    }
}

pub fn run() -> anyhow::Result<()> {
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

    spawn_loop("events", shared.clone(), event_loop);
    spawn_loop("model", shared.clone(), model_loop);
    spawn_loop("transcripts", shared.clone(), transcript_loop);
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

/** New hook rows: update chat states, fan out to subscribers, and wake the model loop. */
fn event_loop(shared: &Arc<Shared>) {
    let Ok(conn) = db::open() else {
        std::thread::sleep(Duration::from_secs(2));
        return;
    };
    loop {
        std::thread::sleep(Duration::from_millis(250));
        let seq = shared.last_seq.load(Ordering::SeqCst);
        let rows = db::since(&conn, seq, 500);
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
            woke = c.wait_timeout(woke, Duration::from_secs(2)).unwrap().0;
        }
        *woke = false;
    }
    std::thread::sleep(Duration::from_millis(250));
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
    let text = String::from_utf8_lossy(&buf[..end]);
    let out: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).unwrap_or(Value::Null)).collect();
    w.next_line += out.len();
    out
}

fn transcript_loop(shared: &Arc<Shared>) {
    std::thread::sleep(Duration::from_millis(300));
    for c in shared.conns.lock().unwrap().iter() {
        let mut watches = c.watches.lock().unwrap();
        for (chat_id, w) in watches.iter_mut() {
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

fn chat_tool(args: &[&str], timeout: u64) -> Result<Value, String> {
    let chat = util::agent_box_dir().join("bin").join("chat");
    let o = util::run(&chat.to_string_lossy(), args, &[], None, Duration::from_secs(timeout));
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
    let o = util::run("python3", &refs, &envs, Some(&cwd), Duration::from_secs(600));
    if o.ok {
        Ok(serde_json::from_str(&o.stdout).unwrap_or(json!({ "stdout": o.stdout })))
    } else {
        Err(o.message())
    }
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
        Call::Hello { .. } => Ok(json!({ "version": PROTOCOL_VERSION, "host": "box" })),
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
                    for e in db::since(&db, from, 1000).into_iter().filter(|e| e.seq <= last) {
                        conn.event(Event::Hook(e));
                    }
                }
            }
            conn.cursor.store(last, Ordering::SeqCst);
            conn.subscribed.store(true, Ordering::SeqCst);
            return Ok(json!({ "last_seq": last }));
        }
        Call::TranscriptRead { chat_id, from } => {
            let chat = registry_chat(&chat_id).ok_or("no such chat")?;
            let id = chat.get("id").and_then(|v| v.as_str()).unwrap_or(&chat_id).to_string();
            let dir = chat.get("dir").and_then(|v| v.as_str()).ok_or("chat has no folder")?;
            let path = util::transcript_path(dir, &id);
            let mut w = Watch { path: path.clone(), offset: 0, next_line: 0 };
            let all = read_lines(&mut w);
            let entries: Vec<Value> = all.into_iter().skip(from).collect();
            let next = w.next_line;
            conn.watches.lock().unwrap().insert(id, w);
            return Ok(json!({ "entries": entries, "from": from, "next": next, "path": path }));
        }
        Call::TranscriptUnwatch { chat_id } => {
            conn.watches.lock().unwrap().retain(|k, _| !k.starts_with(&chat_id));
            return Ok(json!({}));
        }
        Call::ChatNew { name, kind, branch, base, dir, repo, prompt, model } => {
            let mut args: Vec<String> = vec!["new".into(), name, "--kind".into(), kind];
            for (flag, v) in [("--branch", branch), ("--base", base), ("--dir", dir), ("--repo", repo),
                              ("--prompt", prompt), ("--model", model)] {
                if let Some(v) = v.filter(|v| !v.is_empty()) {
                    args.push(flag.into());
                    args.push(v);
                }
            }
            let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            chat_tool(&refs, 900)
        }
        Call::History { .. } => Err("history isn't implemented yet".into()),
        Call::ChatStart { chat_id } => chat_tool(&["start", &chat_id], 300),
        Call::ChatStop { chat_id } => chat_tool(&["stop", &chat_id], 120),
        Call::ChatArchive { chat_id } => chat_tool(&["archive", &chat_id], 120),
        Call::ChatSend { chat_id, text } => chat_tool(&["send", &chat_id, &text], 60),
        Call::ChatKeys { chat_id, keys } => {
            let target = format!("={}:", util::chat_session(&chat_id));
            let mut args = vec!["send-keys", "-t", target.as_str()];
            args.extend(keys.iter().map(|k| k.as_str()));
            let o = util::run("tmux", &args, &[], None, Duration::from_secs(10));
            if o.ok { Ok(json!({})) } else { Err(o.message()) }
        }
        Call::SectionCreate { name } => sc_tool(&["section", "create", &name, "--manual"], None),
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
        Call::Exec { cmd, cwd, timeout_secs } => {
            let home = util::home();
            let cwd = cwd.unwrap_or_else(|| home.to_string_lossy().into_owned());
            let env = util::agent_box_dir().join("bin").join("agent-env");
            let o = util::run(&env.to_string_lossy(), &["bash", "-lc", &cmd], &[], Some(&cwd), Duration::from_secs(timeout_secs.unwrap_or(120)));
            Ok(json!({ "code": if o.ok { 0 } else { 1 }, "stdout": o.stdout, "stderr": o.stderr }))
        }
        Call::SectionMove { id, index } => sc_tool(&["section", "move", &id, "--index", &index.to_string()], None),
        Call::ProjectMove { repo, index } => sc_tool(&["project", "move", &repo, "--index", &index.to_string()], None),
        Call::WorktreeDiff { path, base } => crate::diff::worktree_diff(&path, base.as_deref()),
        Call::TermOpen { session, cwd, cols, rows } => {
            let session = match (session, cwd) {
                (Some(s), _) => s,
                (None, Some(c)) => util::shell_session(&c),
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
    let sock = util::socket_path();
    let stream = match UnixStream::connect(&sock) {
        Ok(s) => s,
        Err(_) => {
            start_detached()?;
            let mut tries = 0;
            loop {
                std::thread::sleep(Duration::from_millis(200));
                if let Ok(s) = UnixStream::connect(&sock) {
                    break s;
                }
                tries += 1;
                if tries > 50 {
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
