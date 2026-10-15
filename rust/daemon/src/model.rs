use agent_box_protocol::{Chat, MergeRequest, Model, Project, Section, Worktree};
use regex::Regex;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::util;

const GIT_TTL: Duration = Duration::from_secs(20);
const MR_TTL: Duration = Duration::from_secs(90);

/** Repo key and GitLab project for each checkout folder name. */
pub(crate) fn repo_for(project_name: &str) -> (&'static str, &'static str, f32) {
    match project_name {
        "redotech-internal-tools" | "internal-tools" => {
            ("internal-tools", "redo/redoapp/internal/redotech-internal-tools", 120.0)
        }
        "redo-data" => ("redo-data", "redo/redoapp/product/redo-data", 30.0),
        _ => ("redo", "redo/redoapp/product/redo", 210.0),
    }
}

#[derive(Clone, Default)]
pub struct GitInfo {
    pub uncommitted: u32,
    pub unpushed: u32,
    pub behind: bool,
    pub last_commit: Option<f64>,
}

/** Git and GitLab facts refreshed in background threads, so building the model never waits. */
#[derive(Default)]
pub struct Caches {
    git: Mutex<HashMap<String, (Instant, GitInfo)>>,
    mr: Mutex<HashMap<(String, String), (Instant, Option<MergeRequest>)>>,
    inflight: Mutex<HashSet<String>>,
}

impl Caches {
    pub fn git(self: &Arc<Self>, path: &str) -> GitInfo {
        let hit = self.git.lock().unwrap().get(path).cloned();
        if hit.as_ref().map(|(t, _)| t.elapsed() > GIT_TTL).unwrap_or(true) {
            self.refresh(format!("git:{path}"), {
                let me = self.clone();
                let path = path.to_string();
                move || {
                    let info = git_info(&path);
                    me.git.lock().unwrap().insert(path, (Instant::now(), info));
                }
            });
        }
        hit.map(|(_, g)| g).unwrap_or_default()
    }

    pub fn mr(self: &Arc<Self>, repo: &str, branch: &str) -> Option<MergeRequest> {
        let key = (repo.to_string(), branch.to_string());
        let hit = self.mr.lock().unwrap().get(&key).cloned();
        if hit.as_ref().map(|(t, _)| t.elapsed() > MR_TTL).unwrap_or(true) {
            self.refresh(format!("mr:{repo}:{branch}"), {
                let me = self.clone();
                move || {
                    let v = fetch_mr(&key.0, &key.1);
                    me.mr.lock().unwrap().insert(key, (Instant::now(), v));
                }
            });
        }
        hit.and_then(|(_, m)| m)
    }

    fn refresh(self: &Arc<Self>, key: String, job: impl FnOnce() + Send + 'static) {
        if !self.inflight.lock().unwrap().insert(key.clone()) {
            return;
        }
        let me = self.clone();
        std::thread::spawn(move || {
            job();
            me.inflight.lock().unwrap().remove(&key);
        });
    }
}

fn git_info(path: &str) -> GitInfo {
    let st = util::run("git", &["-C", path, "status", "--porcelain", "--branch"], &[], None, Duration::from_secs(30));
    let mut info = GitInfo::default();
    if st.ok {
        let mut lines = st.stdout.lines();
        let head = lines.next().unwrap_or("");
        info.uncommitted = lines.count() as u32;
        static AHEAD: OnceLock<Regex> = OnceLock::new();
        let ahead = AHEAD.get_or_init(|| Regex::new(r"ahead (\d+)").unwrap());
        info.unpushed = ahead.captures(head).and_then(|c| c[1].parse().ok()).unwrap_or(0);
        info.behind = head.contains("behind ");
    }
    let log = util::run("git", &["-C", path, "log", "-1", "--format=%ct"], &[], None, Duration::from_secs(10));
    info.last_commit = log.stdout.trim().parse().ok();
    info
}

fn glab_json(path: &str) -> Option<Value> {
    let o = util::run("glab", &["api", path], &[], None, Duration::from_secs(30));
    if o.ok { serde_json::from_str(&o.stdout).ok() } else { None }
}

/** The newest MR for a branch, as bin/dashboard's `_mr` reads it. */
fn fetch_mr(repo: &str, branch: &str) -> Option<MergeRequest> {
    let (_, project, _) = repo_for(repo);
    let pid = project.replace('/', "%2F");
    let list = glab_json(&format!(
        "projects/{pid}/merge_requests?source_branch={branch}&order_by=updated_at&per_page=1"
    ))?;
    let first = list.as_array()?.first()?.clone();
    let iid = first.get("iid")?.as_u64()?;
    let m = glab_json(&format!("projects/{pid}/merge_requests/{iid}")).unwrap_or(first);
    let state = m.get("state").and_then(|v| v.as_str()).unwrap_or("opened");
    let draft = m.get("draft").and_then(|v| v.as_bool()).unwrap_or(false);
    let pipe = m.get("head_pipeline").and_then(|p| p.get("status")).and_then(|v| v.as_str()).unwrap_or("");
    let checks = match pipe {
        "success" => Some("passing"),
        "failed" => Some("failing"),
        "running" | "pending" | "created" => Some("pending"),
        _ => None,
    };
    Some(MergeRequest {
        number: iid,
        url: m.get("web_url").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        state: if draft && state == "opened" { "draft".into() } else { state.into() },
        checks: checks.map(|s| s.into()),
    })
}

fn sc(args: &[&str]) -> Value {
    let shim = util::agent_box_dir().join("shim").join("sc");
    let mut all = vec![shim.to_string_lossy().into_owned()];
    all.extend(args.iter().map(|s| s.to_string()));
    let refs: Vec<&str> = all.iter().map(|s| s.as_str()).collect();
    let home = util::home();
    let o = util::run("python3", &refs, &[], Some(&home.to_string_lossy()), Duration::from_secs(60));
    serde_json::from_str(&o.stdout).unwrap_or(Value::Null)
}

/** Chat state from its latest hook: (state, waiting_on). */
pub type ChatStates = HashMap<String, (String, Option<Value>)>;

pub fn state_for_event(event: &str) -> Option<&'static str> {
    match event {
        "UserPromptSubmit" | "PreToolUse" | "PostToolUse" => Some("working"),
        "Stop" => Some("idle"),
        "Notification" | "PermissionRequest" => Some("waiting"),
        "SessionEnd" => Some("stopped"),
        _ => None,
    }
}

/** A Notification that blocks on the user (a permission or question), not the idle reminder. */
pub fn needs_user(payload: &Value) -> bool {
    let kind = payload.get("notification_type").and_then(Value::as_str).unwrap_or("");
    let message = payload.get("message").and_then(Value::as_str).unwrap_or("");
    match kind {
        "permission_prompt" | "elicitation_dialog" => true,
        "" => !message.starts_with("Claude is waiting for your input"),
        _ => false,
    }
}

/** A chat's tmux pane, with its colors. */
fn capture(session: &str) -> Option<String> {
    let o = util::run("tmux", &["capture-pane", "-p", "-e", "-t", &format!("={session}:")], &[], None, Duration::from_secs(5));
    o.ok.then_some(o.stdout)
}

fn strip_ansi(s: &str) -> String {
    static ANSI: OnceLock<Regex> = OnceLock::new();
    let re = ANSI.get_or_init(|| Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(\x07|\x1b\\)|\x1b[@-_]").unwrap());
    re.replace_all(s, "").into_owned()
}

fn pane_waiting(pane: &str) -> bool {
    static WAITING: OnceLock<Regex> = OnceLock::new();
    let re = WAITING
        .get_or_init(|| Regex::new(r"(?i)Enter to select|Esc to cancel|Do you want to proceed|❯ 1\. Yes").unwrap());
    let text = strip_ansi(pane);
    let lines: Vec<&str> = text.lines().collect();
    re.is_match(&lines[lines.len().saturating_sub(25)..].join("\n"))
}

/** The prompt suggestion in Claude Code's input box: the line after "❯\u{a0}" between two rules,
when everything on it is drawn dim or grey (typed text is drawn plain). */
pub(crate) fn suggestion(pane: &str) -> Option<String> {
    let raw: Vec<&str> = pane.lines().collect();
    let plain: Vec<String> = raw.iter().map(|l| strip_ansi(l)).collect();
    let rule = |i: usize| plain.get(i).is_some_and(|l| l.trim_start().starts_with("──"));
    let i = (1..plain.len()).rev().find(|&i| plain[i].trim_start().starts_with("❯\u{a0}") && rule(i - 1) && rule(i + 1))?;
    let marker = "❯\u{a0}";
    let rest = &raw[i][raw[i].find(marker)? + marker.len()..];
    static SGR: OnceLock<Regex> = OnceLock::new();
    let sgr = SGR.get_or_init(|| Regex::new(r"\x1b\[([0-9;]*)m").unwrap());
    let (mut dim, mut grey, mut text, mut pos) = (false, false, String::new(), 0);
    let visible = |chunk: &str, faint: bool, text: &mut String| -> Option<()> {
        let chunk = strip_ansi(chunk);
        if !faint && !chunk.trim().is_empty() {
            return None;
        }
        text.push_str(&chunk);
        Some(())
    };
    for m in sgr.captures_iter(rest) {
        let whole = m.get(0)?;
        visible(&rest[pos..whole.start()], dim || grey, &mut text)?;
        pos = whole.end();
        let params: Vec<u32> = m[1].split(';').map(|p| p.parse().unwrap_or(0)).collect();
        let mut k = 0;
        while k < params.len() {
            match params[k] {
                0 => (dim, grey) = (false, false),
                2 => dim = true,
                22 => dim = false,
                90 => grey = true,
                30..=37 | 39 | 91..=97 => grey = false,
                38 if params.get(k + 1) == Some(&5) => {
                    let n = params.get(k + 2).copied().unwrap_or(0);
                    grey = n == 8 || (240..=250).contains(&n);
                    k += 2;
                }
                38 if params.get(k + 1) == Some(&2) => {
                    let c = &params[(k + 2).min(params.len())..(k + 5).min(params.len())];
                    grey = c.len() == 3 && c[0] == c[1] && c[1] == c[2] && c[0] < 200;
                    k += 4;
                }
                _ => {}
            }
            k += 1;
        }
    }
    visible(&rest[pos..], dim || grey, &mut text)?;
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn chats(states: &ChatStates) -> Vec<Chat> {
    let dir = util::agent_home().join("state").join("registry").join("chats");
    let alive = util::tmux_sessions();
    let mut out = vec![];
    let mut paths: Vec<_> = std::fs::read_dir(&dir).map(|d| d.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    paths.sort();
    for p in paths {
        if p.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Some(v) = std::fs::read_to_string(&p).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()) else {
            continue;
        };
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(|x| x.to_string());
        let status = s("status").unwrap_or_default();
        if status == "archived" {
            continue;
        }
        let id = s("id").unwrap_or_default();
        let host = s("host").unwrap_or_else(|| "box".into());
        let tmux = util::chat_session(&id);
        let is_alive = host == "box" && alive.contains(&tmux);
        let dir = s("dir");
        let last = dir.as_deref().and_then(|d| util::mtime(&util::transcript_path(d, &id)));
        let hook = states.get(&id).filter(|(st, _)| st != "stopped");
        let pane = if is_alive && hook.is_none_or(|(st, _)| st == "idle") { capture(&tmux) } else { None };
        let (state, waiting_on) = match hook {
            _ if !is_alive => ("stopped".to_string(), None),
            Some((st, w)) => (st.clone(), w.clone()),
            None if pane.as_deref().is_some_and(pane_waiting) => ("waiting".to_string(), None),
            None => ("idle".to_string(), None),
        };
        let suggestion = if state == "idle" { pane.as_deref().and_then(suggestion) } else { None };
        out.push(Chat {
            id,
            name: s("name").unwrap_or_default(),
            kind: s("kind").unwrap_or_else(|| "primary".into()),
            host,
            status,
            dir,
            branch: s("branch"),
            rc_url: s("rc_url"),
            alive: is_alive,
            state,
            waiting_on,
            last_activity: last,
            tmux,
            suggestion,
        });
    }
    out
}

fn worktree(caches: &Arc<Caches>, d: &Value, chats: &[Chat]) -> Option<Worktree> {
    let path = d.get("path")?.as_str()?.to_string();
    let s = |k: &str| d.get(k).and_then(|x| x.as_str()).map(|x| x.to_string());
    let project = s("project_name").or_else(|| s("project_id")).unwrap_or_else(|| "redo".into());
    let (repo, _, hue) = repo_for(&project);
    let is_primary = d.get("is_primary").and_then(|v| v.as_bool()).unwrap_or(false);
    let branch = s("branch");
    let git = caches.git(&path);
    let real = util::realpath(&path);
    let mine: Vec<&Chat> = chats.iter().filter(|c| c.dir.as_deref().map(util::realpath).as_deref() == Some(&real)).collect();
    let age = mine.iter().filter_map(|c| c.last_activity).chain(git.last_commit).fold(None, |a: Option<f64>, b| {
        Some(a.map_or(b, |a| a.max(b)))
    });
    let mr = match (&branch, is_primary) {
        (Some(b), false) => caches.mr(repo, b),
        _ => None,
    };
    Some(Worktree {
        id: s("id").unwrap_or_default(),
        title: s("title").or_else(|| branch.clone()).unwrap_or_else(|| s("worktree").unwrap_or_default()),
        path,
        branch,
        project,
        repo: repo.into(),
        hue,
        is_primary,
        uncommitted: git.uncommitted,
        unpushed: git.unpushed,
        behind: git.behind,
        mr,
        age,
        chat_ids: mine.iter().map(|c| c.id.clone()).collect(),
        host: "box".into(),
    })
}

pub fn build(caches: &Arc<Caches>, states: &ChatStates, last_seq: i64) -> Model {
    if crate::mac::is_mac() {
        return crate::mac::build(caches, last_seq);
    }
    let ws = sc(&["workspace", "list", "--json"]);
    let secs = sc(&["section", "list", "--json"]);
    let chats = chats(states);
    let meta: HashMap<String, &Value> = secs
        .get("sections")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|s| Some((s.get("section_id")?.as_str()?.to_string(), s))).collect())
        .unwrap_or_default();
    let mut projects: Vec<Project> = vec![];
    let mut sections = vec![];
    let workspaces = ws.pointer("/response/workspaces").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    for w in &workspaces {
        for sec in w.get("sections").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
            let items: Vec<Worktree> = sec
                .get("items")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|d| worktree(caches, d, &chats)).collect())
                .unwrap_or_default();
            match sec.get("kind").and_then(|v| v.as_str()) {
                Some("projects") => {
                    for it in items {
                        let idx = match projects.iter().position(|p| p.name == it.project) {
                            Some(i) => i,
                            None => {
                                projects.push(Project { name: it.project.clone(), repo: it.repo.clone(), hue: it.hue, items: vec![] });
                                projects.len() - 1
                            }
                        };
                        projects[idx].items.push(it);
                    }
                }
                Some("worktrees") => {
                    let id = sec.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                    let m = meta.get(&id);
                    sections.push(Section {
                        name: m
                            .and_then(|m| m.get("name")?.as_str())
                            .or_else(|| sec.get("name").or_else(|| sec.get("title"))?.as_str())
                            .unwrap_or("Section")
                            .to_string(),
                        collapsed: m.and_then(|m| m.get("collapsed")?.as_bool()).unwrap_or(false),
                        id,
                        items,
                    });
                }
                _ => {}
            }
        }
    }
    Model {
        host: "box".into(),
        ssh_host: std::env::var("AGENT_BOX_SSH_HOST").unwrap_or_else(|_| "coder.tj-agents".into()),
        projects,
        sections,
        chats,
        last_seq,
    }
}

#[cfg(test)]
mod tests {
    use super::suggestion;

    fn pane(input: &str) -> String {
        let rule = "─".repeat(20);
        format!("⏺ done\n\x1b[38;5;244m{rule}\n\x1b[39m❯\u{a0}{input}\n\x1b[38;5;244m{rule}\n\x1b[39m  bypass permissions on")
    }

    #[test]
    fn dim_input_is_a_suggestion() {
        assert_eq!(suggestion(&pane("\x1b[2mcommit this\x1b[0m")).as_deref(), Some("commit this"));
    }

    #[test]
    fn typed_or_empty_input_is_not() {
        assert_eq!(suggestion(&pane("hello")), None);
        assert_eq!(suggestion(&pane("")), None);
        assert_eq!(suggestion(&pane("com\x1b[2mmit this\x1b[0m")), None);
    }
}
