/*! Mac mode: the model and chat calls come from super.engineering's `sc` CLI and (read-only) its db.
A chat's id is its Claude session uuid, or the thread id of a chat in super.engineering's chat view. */

use agent_box_protocol::{Chat, MergeRequest, Model, Project, Section, Worktree};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::model::{repo_for, Caches};
use crate::util;

pub fn is_mac() -> bool {
    match std::env::var("AGENT_BOX_HOST").as_deref() {
        Ok("mac") => true,
        Ok("box") => false,
        _ => cfg!(target_os = "macos"),
    }
}

fn sc_path() -> PathBuf {
    util::home().join(".super.engineering").join("bin").join("sc")
}

/** Run super.engineering's CLI. The caller env is dropped so sc never treats us as one of its tabs. */
fn sc_raw(args: &[&str], worktree: Option<&str>, input: Option<&str>, timeout: u64) -> util::Output {
    let envs: Vec<(&str, &str)> = worktree.map(|p| vec![("SUPER_ENGINEERING_WORKTREE_PATH", p)]).unwrap_or_default();
    util::run_with(&sc_path().to_string_lossy(), args, &envs, &["SUPER_ENGINEERING_", "SUPERCONDUCTOR_"],
                   worktree, input, Duration::from_secs(timeout))
}

fn sc(args: &[&str], worktree: Option<&str>, input: Option<&str>) -> Result<Value, String> {
    let o = sc_raw(args, worktree, input, 60);
    let v: Value = serde_json::from_str(o.stdout.trim()).unwrap_or_else(|_| json!({ "stdout": o.stdout.trim() }));
    if !o.ok || v.get("kind").and_then(|k| k.as_str()) == Some("cli_error") {
        let msg = v.pointer("/error/message").and_then(|m| m.as_str()).map(|m| m.to_string());
        return Err(msg.unwrap_or_else(|| o.message()));
    }
    // Per-target failures (agent send/stop, layout state) come back inside an ok response.
    if let Some(err) = v.pointer("/response/targets").and_then(|t| t.as_array())
        .and_then(|t| t.iter().find(|x| x.get("ok") == Some(&Value::Bool(false))))
        .and_then(|x| x.pointer("/error/message")).and_then(|m| m.as_str()) {
        return Err(err.to_string());
    }
    Ok(v)
}

/** An open super.engineering tab running Claude. */
#[derive(Clone, Debug)]
struct Tab {
    chat_id: String,
    conversation_id: Option<String>,
    session_path: Option<String>,
    worktree: String,
    target: String,
    managed: bool,
    ui: String,
    state: String,
    title: String,
}

/** The tabs seen by the last model build, for the calls that act on a chat. */
fn tabs() -> &'static Mutex<HashMap<String, Tab>> {
    static TABS: OnceLock<Mutex<HashMap<String, Tab>>> = OnceLock::new();
    TABS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn chat_id_of(conversation_id: Option<&str>, session_id: Option<&str>) -> Option<String> {
    match conversation_id {
        Some(c) if c.starts_with("conv:claude:") => Some(c["conv:claude:".len()..].to_string()),
        Some(c) if !c.is_empty() && !c.starts_with("conv:") => Some(c.to_string()),
        _ => session_id.filter(|s| !s.is_empty()).map(|s| s.to_string()),
    }
}

fn str_of(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(|s| s.to_string())
}

/** Every worktree item in `sc workspace list`, with the section it sits in (None: Projects). */
fn items(ws: &Value) -> Vec<(Option<String>, Value)> {
    let mut out = vec![];
    for w in ws.pointer("/response/workspaces").and_then(|v| v.as_array()).into_iter().flatten() {
        for sec in w.get("sections").and_then(|v| v.as_array()).into_iter().flatten() {
            if sec.get("kind").and_then(|k| k.as_str()) == Some("projects") {
                for p in sec.get("projects").and_then(|v| v.as_array()).into_iter().flatten() {
                    for it in p.get("items").and_then(|v| v.as_array()).into_iter().flatten() {
                        out.push((None, it.clone()));
                    }
                }
            } else {
                for it in sec.get("items").and_then(|v| v.as_array()).into_iter().flatten() {
                    out.push((str_of(sec, "id"), it.clone()));
                }
            }
        }
    }
    out
}

/** The Claude tabs of one worktree, from its sessions in `sc workspace list` and `sc layout state`. */
fn worktree_tabs(item: &Value) -> Vec<Tab> {
    let Some(path) = str_of(item, "path") else { return vec![] };
    let mut selectors = vec![];
    let mut titles = HashMap::new();
    for s in item.get("sessions").and_then(|v| v.as_array()).into_iter().flatten() {
        if !matches!(s.get("kind").and_then(|k| k.as_str()), Some("terminal") | Some("api_chat")) {
            continue;
        }
        // "v1.p0.t5" is view 1, tab index 5; selectors count tabs from 1.
        let id = s.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let parts: Vec<&str> = id.split('.').collect();
        let view = parts.first().and_then(|p| p.strip_prefix('v'));
        let index = parts.last().and_then(|p| p.strip_prefix('t')).and_then(|t| t.parse::<usize>().ok());
        let (Some(v), Some(t)) = (view, index) else { continue };
        let sel = format!("view:{v}/tab:{}", t + 1);
        titles.insert(sel.clone(), str_of(s, "title").unwrap_or_default());
        selectors.push(sel);
    }
    if selectors.is_empty() {
        return vec![];
    }
    let mut args = vec!["layout", "state", "--output", "json", "--worktree", path.as_str()];
    for s in &selectors {
        args.extend(["--to", s.as_str()]);
    }
    let o = sc_raw(&args, None, None, 20);
    let v: Value = serde_json::from_str(&o.stdout).unwrap_or(Value::Null);
    let mut out = vec![];
    for t in v.pointer("/response/targets").and_then(|t| t.as_array()).into_iter().flatten() {
        if t.get("ok") != Some(&Value::Bool(true)) {
            continue;
        }
        let session_path = str_of(t, "session_path");
        let provider = str_of(t, "provider_key").unwrap_or_default();
        let is_claude = provider == "claude" || session_path.as_deref().is_some_and(|p| p.contains("/.claude/projects/"));
        let conversation_id = str_of(t, "conversation_id");
        let chat_id = chat_id_of(conversation_id.as_deref(), t.get("session_id").and_then(|s| s.as_str()));
        let (true, Some(chat_id)) = (is_claude, chat_id) else { continue };
        let both = format!("{} {}", str_of(t, "state").unwrap_or_default(), str_of(t, "phase").unwrap_or_default())
            .to_lowercase();
        let state = if t.get("has_active_turn") == Some(&Value::Bool(true)) {
            "working"
        } else if ["await", "waiting", "permission", "approval", "input"].iter().any(|w| both.contains(w)) {
            "waiting"
        } else if ["running", "working", "busy"].iter().any(|w| both.contains(w)) {
            "working"
        } else {
            "idle"
        };
        let selector = str_of(t, "selector").unwrap_or_default();
        let tab_sel = selector.rsplit_once("/pane:").map(|(a, _)| a.to_string()).unwrap_or(selector);
        out.push(Tab {
            chat_id,
            conversation_id,
            session_path,
            worktree: path.clone(),
            target: format!("id:{}", str_of(t, "stable_target_id").unwrap_or_default()),
            managed: t.get("managed") == Some(&Value::Bool(true)),
            ui: str_of(t, "ui").unwrap_or_default(),
            state: state.into(),
            title: titles.get(&tab_sel).cloned().unwrap_or_default(),
        });
    }
    out
}

/** Claude transcripts by session id: (path, mtime). Rescanned at most every few seconds. */
fn transcripts() -> HashMap<String, (PathBuf, f64)> {
    static CACHE: OnceLock<Mutex<(Option<Instant>, HashMap<String, (PathBuf, f64)>)>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new((None, HashMap::new())));
    let mut c = cache.lock().unwrap();
    if c.0.map(|t| t.elapsed() < Duration::from_secs(5)).unwrap_or(false) {
        return c.1.clone();
    }
    let mut map: HashMap<String, (PathBuf, f64)> = HashMap::new();
    let root = util::home().join(".claude").join("projects");
    for dir in std::fs::read_dir(root).into_iter().flatten().flatten() {
        for f in std::fs::read_dir(dir.path()).into_iter().flatten().flatten() {
            let p = f.path();
            if p.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let stem = p.file_stem().and_then(|s| s.to_str()).map(|s| s.to_string());
            let (Some(stem), Some(m)) = (stem, util::mtime(&p)) else { continue };
            if map.get(&stem).map(|old| old.1 < m).unwrap_or(true) {
                map.insert(stem, (p, m));
            }
        }
    }
    *c = (Some(Instant::now()), map.clone());
    map
}

/** Claude sessions bound to each super.engineering conversation, newest first. */
fn bindings() -> HashMap<String, Vec<String>> {
    let db = util::home().join(".superconductor").join("db").join("super.engineering.db");
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let Ok(conn) = rusqlite::Connection::open_with_flags(&db, flags) else { return HashMap::new() };
    let _ = conn.busy_timeout(Duration::from_secs(2));
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    if let Ok(mut st) = conn.prepare(
        "SELECT conversation_id, binding_value FROM conversation_bindings
         WHERE binding_kind = 'provider_session' AND provider_key = 'claude' ORDER BY updated_at DESC",
    ) {
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)));
        for (conv, session) in rows.into_iter().flatten().flatten() {
            out.entry(conv).or_default().push(session);
        }
    }
    out
}

/** Every session file a chat may have written: its own, and those super.engineering bound to it. */
fn sessions_of(chat_id: &str, binds: &HashMap<String, Vec<String>>) -> Vec<String> {
    let mut out = binds.get(chat_id).cloned().unwrap_or_default();
    out.extend(binds.get(&format!("conv:claude:{chat_id}")).cloned().unwrap_or_default());
    out.push(chat_id.to_string());
    out
}

/** A chat's transcript: its open tab's session file, else the newest session file it has. */
pub fn transcript_for(chat_id: &str) -> Option<PathBuf> {
    if let Some(p) = tabs().lock().unwrap().get(chat_id).and_then(|t| t.session_path.clone()) {
        if std::path::Path::new(&p).exists() {
            return Some(p.into());
        }
    }
    let index = transcripts();
    sessions_of(chat_id, &bindings()).iter().filter_map(|s| index.get(s))
        .max_by(|a, b| a.1.total_cmp(&b.1)).map(|(p, _)| p.clone())
}

fn mr_of(pr: &Value) -> Option<MergeRequest> {
    let number = pr.get("number")?.as_u64()?;
    let state = match pr.get("state").and_then(|s| s.as_str()).unwrap_or("open") {
        "open" | "opened" => "opened",
        other => other,
    };
    let checks = pr.pointer("/checks/status").and_then(|s| s.as_str()).and_then(|s| match s {
        "passing" | "success" => Some("passing"),
        "failing" | "failed" => Some("failing"),
        "pending" | "running" => Some("pending"),
        _ => None,
    });
    Some(MergeRequest { number, url: str_of(pr, "url").unwrap_or_default(), state: state.into(), checks: checks.map(|c| c.into()) })
}

fn worktree(caches: &Arc<Caches>, d: &Value, chats: &[Chat]) -> Option<Worktree> {
    let path = str_of(d, "path")?;
    let project = str_of(d, "project_name").unwrap_or_else(|| "redo".into());
    let (repo, _, hue) = repo_for(&project);
    let branch = str_of(d, "branch");
    let real = util::realpath(&path);
    let mine: Vec<&Chat> = chats.iter().filter(|c| c.dir.as_deref().map(util::realpath).as_deref() == Some(&real)).collect();
    let local = d.get("local").cloned().unwrap_or(Value::Null);
    let n = |k: &str| local.get(k).and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let age = mine.iter().filter_map(|c| c.last_activity).chain(caches.git(&path).last_commit)
        .fold(None, |a: Option<f64>, b| Some(a.map_or(b, |a| a.max(b))));
    Some(Worktree {
        id: str_of(d, "id").unwrap_or_default(),
        title: str_of(d, "title").or_else(|| branch.clone()).unwrap_or_else(|| str_of(d, "worktree").unwrap_or_default()),
        path,
        branch,
        project,
        repo: repo.into(),
        hue,
        is_primary: d.get("is_primary").and_then(|v| v.as_bool()).unwrap_or(false),
        uncommitted: n("uncommitted"),
        unpushed: n("unpushed"),
        behind: local.get("behind_target").and_then(|v| v.as_bool()).unwrap_or(false),
        mr: d.get("pr").and_then(mr_of),
        age,
        chat_ids: mine.iter().map(|c| c.id.clone()).collect(),
        host: "mac".into(),
    })
}

/** super.engineering's Claude chats by chat id: (title, its `sc chat list` entry). */
fn se_chats() -> HashMap<String, (Option<String>, Value)> {
    let o = sc_raw(&["chat", "list", "--json"], None, None, 20);
    let v: Value = serde_json::from_str(&o.stdout).unwrap_or(Value::Null);
    let mut out = HashMap::new();
    for s in v.get("sessions").and_then(|s| s.as_array()).into_iter().flatten() {
        if s.get("provider_key").and_then(|p| p.as_str()) != Some("claude") {
            continue;
        }
        if let Some(id) = chat_id_of(s.get("conversation_id").and_then(|c| c.as_str()), None) {
            out.insert(id, (str_of(s, "title"), s.clone()));
        }
    }
    out
}

pub fn build(caches: &Arc<Caches>, last_seq: i64) -> Model {
    let ws = sc(&["workspace", "list", "--json"], None, None).unwrap_or(Value::Null);
    let secs = sc(&["section", "list", "--json"], None, None).unwrap_or(Value::Null);
    let all = items(&ws);
    let titles = se_chats();
    let index = transcripts();

    // super.engineering can show one conversation twice: a terminal resuming its own session and a
    // chat view running a fresh one. The tab whose session is the chat itself wins.
    let own = |t: &Tab| t.session_path.as_deref().is_some_and(|p| p.ends_with(&format!("/{}.jsonl", t.chat_id)));
    let mut found: Vec<Tab> = vec![];
    for t in all.iter().flat_map(|(_, it)| worktree_tabs(it)) {
        match found.iter().position(|f| f.chat_id == t.chat_id) {
            Some(i) if own(&t) && !own(&found[i]) => found[i] = t,
            Some(_) => {}
            None => found.push(t),
        }
    }
    let mut chats = vec![];
    for t in &found {
        let item = all.iter().map(|(_, i)| i).find(|i| str_of(i, "path").as_deref() == Some(t.worktree.as_str()));
        let last = t.session_path.as_deref().and_then(|p| util::mtime(std::path::Path::new(p)))
            .or_else(|| index.get(&t.chat_id).map(|x| x.1));
        let name = titles.get(&t.chat_id).and_then(|x| x.0.clone()).filter(|s| !s.is_empty())
            .or_else(|| Some(t.title.clone()).filter(|s| !s.is_empty()))
            .unwrap_or_else(|| "Claude".into());
        let primary = item.and_then(|i| i.get("is_primary")).and_then(|v| v.as_bool()).unwrap_or(false);
        chats.push(Chat {
            id: t.chat_id.clone(),
            name,
            kind: if primary { "primary".into() } else { "feature".into() },
            host: "mac".into(),
            status: "running".into(),
            dir: Some(t.worktree.clone()),
            branch: item.and_then(|i| str_of(i, "branch")),
            rc_url: None,
            alive: true,
            state: t.state.clone(),
            waiting_on: None,
            last_activity: last,
            tmux: String::new(),
            // sc reads super.engineering terminals as plain text, so a suggestion looks like typed text.
            suggestion: None,
        });
    }
    *tabs().lock().unwrap() = found.into_iter().map(|t| (t.chat_id.clone(), t)).collect();

    let meta: HashMap<String, &Value> = secs.get("sections").and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|s| Some((s.get("section_id")?.as_str()?.to_string(), s))).collect())
        .unwrap_or_default();
    let mut projects: Vec<Project> = vec![];
    let mut sections: Vec<Section> = vec![];
    for (sec, it) in &all {
        let Some(w) = worktree(caches, it, &chats) else { continue };
        match sec {
            None => {
                let idx = match projects.iter().position(|p| p.name == w.project) {
                    Some(i) => i,
                    None => {
                        projects.push(Project { name: w.project.clone(), repo: w.repo.clone(), hue: w.hue, items: vec![] });
                        projects.len() - 1
                    }
                };
                projects[idx].items.push(w);
            }
            Some(id) => {
                let idx = match sections.iter().position(|s| &s.id == id) {
                    Some(i) => i,
                    None => {
                        let m = meta.get(id);
                        sections.push(Section {
                            id: id.clone(),
                            name: m
                                .and_then(|m| m.get("name")?.as_str())
                                .or_else(|| section_title(&ws, id))
                                .unwrap_or("Section")
                                .to_string(),
                            collapsed: m.and_then(|m| m.get("collapsed")?.as_bool()).unwrap_or(false),
                            items: vec![],
                        });
                        sections.len() - 1
                    }
                };
                sections[idx].items.push(w);
            }
        }
    }
    Model { host: "mac".into(), ssh_host: String::new(), projects, sections, chats, last_seq }
}

fn tab(chat_id: &str) -> Option<Tab> {
    tabs().lock().unwrap().get(chat_id).cloned()
}

/** Past chats, newest first: super.engineering's Claude chats with no open tab, plus Claude
sessions in its worktrees that it never indexed. */
pub fn history(limit: usize) -> Value {
    let open: HashSet<String> = tabs().lock().unwrap().keys().cloned().collect();
    let index = transcripts();
    let binds = bindings();
    let mut out: Vec<Value> = vec![];
    let mut known = HashSet::new();
    for (id, (title, s)) in se_chats() {
        let sessions = sessions_of(&id, &binds);
        let last = sessions.iter().filter_map(|x| index.get(x).map(|t| t.1)).fold(None, |a: Option<f64>, b| Some(a.map_or(b, |a| a.max(b))));
        known.extend(sessions);
        out.push(json!({
            "id": id, "name": title.unwrap_or_else(|| "Claude".into()), "dir": str_of(&s, "worktree_path"),
            "host": "mac", "last_activity": last, "closed": !open.contains(&id),
        }));
    }
    let ws = sc(&["workspace", "list", "--json"], None, None).unwrap_or(Value::Null);
    for (_, it) in items(&ws) {
        let Some(path) = str_of(&it, "path") else { continue };
        let common = util::run("git", &["-C", &path, "rev-parse", "--path-format=absolute", "--git-common-dir"],
                               &[], None, Duration::from_secs(5));
        let main = common.stdout.trim().trim_end_matches("/.git").to_string();
        let o = sc_raw(&["history", "list", "--cwd", &path, "--main-repo", &main, "--provider", "claude", "--limit", "20"],
                       None, None, 20);
        let v: Value = serde_json::from_str(&o.stdout).unwrap_or(Value::Null);
        for e in v.get("entries").and_then(|e| e.as_array()).into_iter().flatten() {
            let Some(sid) = str_of(e, "session_id") else { continue };
            if !known.insert(sid.clone()) {
                continue;
            }
            let name: String = str_of(e, "first_message").unwrap_or_default().chars().take(80).collect();
            let ts = e.get("timestamp_ms").and_then(|t| t.as_f64()).map(|t| t / 1000.0);
            out.push(json!({
                "id": sid, "name": if name.is_empty() { "Claude".to_string() } else { name }, "dir": path,
                "host": "mac", "last_activity": ts.or_else(|| index.get(&sid).map(|x| x.1)), "closed": !open.contains(&sid),
            }));
        }
    }
    out.retain(|c| c.get("closed") == Some(&Value::Bool(true)));
    let t = |v: &Value| v.get("last_activity").and_then(|x| x.as_f64()).unwrap_or(0.0);
    out.sort_by(|a, b| t(b).total_cmp(&t(a)));
    out.truncate(limit);
    json!({ "chats": out })
}

/** The folder a session ran in, from its transcript. */
fn transcript_cwd(chat_id: &str) -> Option<String> {
    let text = std::fs::read_to_string(transcript_for(chat_id)?).ok()?;
    text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).find_map(|v| str_of(&v, "cwd"))
}

pub fn chat_send(chat_id: &str, text: &str) -> Result<Value, String> {
    let t = tab(chat_id).ok_or("this chat isn't open in super.engineering; start it first")?;
    if t.ui == "chat" {
        let conv = t.conversation_id.clone().unwrap_or_else(|| chat_id.to_string());
        return sc(&["chat", "send", &conv, "--stdin"], None, Some(text));
    }
    if t.managed {
        return sc(&["agent", "send", "--to", &t.target, "--prompt", text, "--worktree", &t.worktree, "--output", "json"],
                  None, None);
    }
    // Claude in a plain terminal tab: terminal input takes one line, and a separate carriage
    // return submits it (sent together, Claude takes the burst as a paste).
    let line = text.split('\n').map(|l| l.trim_end()).collect::<Vec<_>>().join(" ");
    sc(&["terminal", "input", "--to", &t.target, "--text", &line, "--worktree", &t.worktree, "--output", "json"], None, None)?;
    std::thread::sleep(Duration::from_millis(500));
    sc(&["terminal", "input", "--to", &t.target, "--text", "", "--submit", "--worktree", &t.worktree, "--output", "json"],
       None, None)
}

/** Stop the current turn; the chat stays open in super.engineering. */
pub fn chat_stop(chat_id: &str) -> Result<Value, String> {
    let t = tab(chat_id).ok_or("this chat isn't open in super.engineering")?;
    if t.ui != "chat" && !t.managed {
        return sc(&["terminal", "input", "--to", &t.target, "--key", "Escape", "--worktree", &t.worktree, "--output", "json"],
                  None, None);
    }
    sc(&["agent", "stop", "--to", &t.target, "--worktree", &t.worktree, "--output", "json"], None, None)
}

/** Close the chat's tab in super.engineering; it stays in history. */
pub fn chat_archive(chat_id: &str) -> Result<Value, String> {
    let t = tab(chat_id).ok_or("this chat isn't open in super.engineering")?;
    sc(&["agent", "stop", "--to", &t.target, "--close", "--worktree", &t.worktree, "--output", "json"], None, None)
}

/** Reopen a chat as the same session: a chat-view chat in place, a terminal chat with `claude
--resume` in a new terminal tab (super.engineering would reopen it as a fresh chat-view session). */
pub fn chat_start(chat_id: &str) -> Result<Value, String> {
    if tab(chat_id).is_some() {
        return Ok(json!({ "already_open": true }));
    }
    let entry = se_chats().get(chat_id).map(|(_, s)| s.clone());
    let conv = entry.as_ref().and_then(|s| str_of(s, "conversation_id"));
    if let Some(c) = conv.as_deref().filter(|c| !c.starts_with("conv:")) {
        return sc(&["chat", "select", c], None, None);
    }
    if chat_id.is_empty() || !chat_id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        return Err(format!("not a Claude session id: {chat_id}"));
    }
    let dir = entry.as_ref().and_then(|s| str_of(s, "worktree_path")).or_else(|| transcript_cwd(chat_id))
        .ok_or("don't know which worktree this chat ran in")?;
    let label = format!("abx-{}", &chat_id[..chat_id.len().min(8)]);
    let command = format!("claude --resume {chat_id}");
    sc(&["terminal", "new", "--standalone", "--worktree", &dir, "--label", &label, "--command", &command, "--output", "json"],
       None, None)
}

/** A new chat in super.engineering's chat view; stdout starts with its id. */
pub fn chat_new(dir: Option<&str>, prompt: Option<&str>, model: Option<&str>) -> Result<Value, String> {
    let dir = match dir.filter(|d| !d.is_empty()) {
        Some(d) => d.to_string(),
        None => util::home().join("dev").join("redo").to_string_lossy().into_owned(),
    };
    let mut args = vec!["chat", "new", "--worktree", dir.as_str(), "--provider", "claude", "--activate", "--standalone"];
    if let Some(m) = model.filter(|m| !m.is_empty()) {
        args.extend(["--model", m]);
    }
    let o = sc_raw(&args, None, None, 60);
    let id = o.stdout.split_whitespace().next().unwrap_or("").to_string();
    if !o.ok || id.is_empty() {
        return Err(o.message());
    }
    if let Some(p) = prompt.filter(|p| !p.is_empty()) {
        sc(&["chat", "send", &id, "--stdin"], None, Some(p))?;
    }
    Ok(json!({ "stdout": format!("{id}  {dir}\n") }))
}

/** tmux key names into a chat's terminal. A managed agent waiting on a permission prompt takes only
Escape and C-c. */
pub fn chat_keys(chat_id: &str, keys: &[String]) -> Result<Value, String> {
    let t = tab(chat_id).ok_or("this chat isn't open in super.engineering")?;
    if t.ui == "chat" {
        // A lone Escape is the app's stop button; a chat view cancels its turn instead.
        if keys.len() == 1 && keys[0] == "Escape" {
            let conv = t.conversation_id.clone().unwrap_or_else(|| chat_id.to_string());
            return sc(&["chat", "cancel", &conv], None, None);
        }
        return Err("this chat is in super.engineering's chat view, which takes no keys; answer it there".into());
    }
    for k in keys {
        let mut args = vec!["terminal", "input", "--to", t.target.as_str(), "--worktree", t.worktree.as_str(), "--output", "json"];
        match k.as_str() {
            "C-c" => args.push("--interrupt"),
            // In a plain terminal, sc's Enter key reaches Claude as a newline; a carriage return submits.
            "Enter" if !t.managed => args.extend(["--text", "", "--submit"]),
            "Enter" | "Tab" | "Up" | "Down" | "Left" | "Right" | "Escape" | "Backspace" | "Delete" => args.extend(["--key", k.as_str()]),
            "BSpace" => args.extend(["--key", "Backspace"]),
            "DC" => args.extend(["--key", "Delete"]),
            other => return Err(format!("super.engineering can't send the key {other:?}")),
        }
        sc(&args, None, None)?;
    }
    Ok(json!({}))
}

pub fn section_create(name: &str) -> Result<Value, String> {
    sc(&["section", "create", name, "--manual", "--json"], None, None)
}

pub fn section_rename(id: &str, name: &str) -> Result<Value, String> {
    sc(&["section", "edit", id, "--name", name, "--json"], None, None)
}

pub fn section_delete(id: &str) -> Result<Value, String> {
    sc(&["section", "delete", id, "--json"], None, None)
}

/** Put a worktree in a manual section (None: back under its project). super.engineering keeps no
order within a section, so a requested position is ignored. */
pub fn section_assign(path: &str, section: Option<&str>) -> Result<Value, String> {
    match section.filter(|s| !s.is_empty() && *s != "none") {
        Some(s) => sc(&["section", "assign", s, "--json"], Some(path), None),
        None => sc(&["section", "unassign", "--json"], Some(path), None),
    }
}

/** Move a section to `index` among the manual sections. */
pub fn section_move(id: &str, index: usize) -> Result<Value, String> {
    let secs = sc(&["section", "list", "--json"], None, None)?;
    let order: Vec<String> = secs.get("sections").and_then(|s| s.as_array()).into_iter().flatten()
        .filter(|s| s.get("built_in") != Some(&Value::Bool(true)))
        .filter_map(|s| str_of(s, "section_id")).filter(|s| s != id).collect();
    match (order.get(index), order.last()) {
        (Some(before), _) => sc(&["section", "move", id, "--before", before, "--json"], None, None),
        (None, Some(last)) => sc(&["section", "move", id, "--after", last, "--json"], None, None),
        (None, None) => Ok(json!({})),
    }
}

pub fn worktree_delete(path: &str) -> Result<Value, String> {
    let ws = sc(&["workspace", "list", "--json"], None, None)?;
    let id = items(&ws).into_iter().find(|(_, it)| str_of(it, "path").as_deref() == Some(path))
        .and_then(|(_, it)| str_of(&it, "id")).ok_or("no such worktree in super.engineering")?;
    sc(&["worktree", "delete", &id, "--json"], None, None)
}

/** A manual section's title as `workspace list` reports it. */
fn section_title<'a>(ws: &'a Value, id: &str) -> Option<&'a str> {
    ws.pointer("/response/workspaces")?.as_array()?.iter().flat_map(|w| w.get("sections").and_then(|s| s.as_array()).into_iter().flatten())
        .find(|s| s.get("id").and_then(|v| v.as_str()) == Some(id))?
        .get("title")?
        .as_str()
}
