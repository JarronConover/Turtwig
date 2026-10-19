use agent_box_protocol::{Activity, Chat, MergeRequest, Model, Project, Section, SyncStatus, Worktree};
use regex::Regex;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::{config, hosts, util};

/** How long a worktree's git status / merge request stays fresh (`git_ttl_secs`, `mr_ttl_secs`). */
fn git_ttl() -> Duration {
    crate::config::secs("git_ttl_secs", 90)
}
/** How many `git status` runs at once (`git_at_once`): in a big repo each takes seconds of disk and CPU,
and forty at once (one per worktree) slowed the whole Mac down. */
fn git_at_once() -> usize {
    crate::config::count("git_at_once", 2).max(1)
}
static GIT_SLOTS: (std::sync::Mutex<usize>, std::sync::Condvar) = (std::sync::Mutex::new(0), std::sync::Condvar::new());

fn with_git_slot<T>(f: impl FnOnce() -> T) -> T {
    let (lock, cv) = &GIT_SLOTS;
    let mut busy = cv.wait_while(lock.lock().unwrap(), |n| *n >= git_at_once()).unwrap();
    *busy += 1;
    drop(busy);
    let out = f();
    *lock.lock().unwrap() -= 1;
    cv.notify_one();
    out
}
fn mr_ttl() -> Duration {
    crate::config::secs("mr_ttl_secs", 90)
}

/** The host a registry chat without one is on (`registry_default_host`; chats from before hosts were named). */
pub fn registry_default_host() -> String {
    config::text("registry_default_host", "box")
}

fn git_wait() -> Duration {
    config::secs("git_timeout_secs", 30)
}

#[derive(Clone, Default)]
pub struct GitInfo {
    pub uncommitted: u32,
    pub unpushed: u32,
    pub behind: bool,
    pub behind_count: u32,
    pub upstream: Option<String>,
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
        if hit.as_ref().map(|(t, _)| t.elapsed() > git_ttl()).unwrap_or(true) {
            self.refresh(format!("git:{path}"), {
                let me = self.clone();
                let path = path.to_string();
                move || {
                    let info = with_git_slot(|| git_info(&path));
                    me.git.lock().unwrap().insert(path, (Instant::now(), info));
                }
            });
        }
        hit.map(|(_, g)| g).unwrap_or_default()
    }

    pub fn mr(self: &Arc<Self>, repo: &str, branch: &str, path: &str) -> Option<MergeRequest> {
        let key = (repo.to_string(), branch.to_string());
        let hit = self.mr.lock().unwrap().get(&key).cloned();
        if hit.as_ref().map(|(t, _)| t.elapsed() > mr_ttl()).unwrap_or(true) {
            self.refresh(format!("mr:{repo}:{branch}"), {
                let me = self.clone();
                let path = path.to_string();
                move || {
                    let v = fetch_mr(&key.0, &key.1, &path);
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

/** Counts and upstream from `git status --porcelain --branch`'s first line. */
fn parse_head(head: &str, info: &mut GitInfo) {
    static RE: OnceLock<(Regex, Regex, Regex)> = OnceLock::new();
    let (ahead, behind, upstream) = RE.get_or_init(|| {
        (Regex::new(r"ahead (\d+)").unwrap(), Regex::new(r"behind (\d+)").unwrap(), Regex::new(r"\.\.\.(\S+)").unwrap())
    });
    info.unpushed = ahead.captures(head).and_then(|c| c[1].parse().ok()).unwrap_or(0);
    info.behind_count = behind.captures(head).and_then(|c| c[1].parse().ok()).unwrap_or(0);
    info.behind = info.behind_count > 0;
    info.upstream = upstream.captures(head).map(|c| c[1].to_string());
}

fn git_info(path: &str) -> GitInfo {
    let st = util::run("git", &["-C", path, "status", "--porcelain", "--branch"], &[], None, git_wait());
    let mut info = GitInfo::default();
    if st.ok {
        let mut lines = st.stdout.lines();
        let head = lines.next().unwrap_or("");
        info.uncommitted = lines.count() as u32;
        parse_head(head, &mut info);
    }
    let log = util::run("git", &["-C", path, "log", "-1", "--format=%ct"], &[], None, git_wait() / 3);
    info.last_commit = log.stdout.trim().parse().ok();
    info
}

fn forge_wait() -> Duration {
    config::secs("forge_timeout_secs", 30)
}

fn json_of(cmd: &str, args: &[&str]) -> Option<Value> {
    let o = util::run(cmd, args, &[], None, forge_wait());
    if o.ok { serde_json::from_str(&o.stdout).ok() } else { None }
}

/** (host, project path) from a remote URL: git@host:a/b.git, https://host/a/b.git, ssh://git@host/a/b. */
pub(crate) fn parse_remote(url: &str) -> Option<(String, String)> {
    let url = url.trim().trim_end_matches('/');
    let url = url.strip_suffix(".git").unwrap_or(url);
    let (host, path) = if let Some(rest) = url.split_once("://").map(|(_, r)| r) {
        let rest = rest.rsplit_once('@').map_or(rest, |(_, r)| r);
        rest.split_once('/')?
    } else {
        let rest = url.rsplit_once('@').map_or(url, |(_, r)| r);
        rest.split_once(':')?
    };
    let host = host.split(':').next().unwrap_or(host);
    Some((host.to_string(), path.trim_start_matches('/').to_string()))
}

/** The branch's newest MR from `repos.REPO.forge` / `forge` (default: github for a github.com origin, else gitlab):
"gitlab" (glab), "github" (gh), "command" (`forge_cmd` argv, {project} {branch} {path} {repo}, prints one), "none". */
fn fetch_mr(repo: &str, branch: &str, path: &str) -> Option<MergeRequest> {
    let conf = hosts::repos().get(repo).cloned().unwrap_or(Value::Null);
    let s = |k: &str| conf.get(k).and_then(Value::as_str).map(str::to_string).or_else(|| config::get(k)?.as_str().map(str::to_string));
    let origin = util::run("git", &["-C", path, "remote", "get-url", "origin"], &[], None, git_wait() / 3).stdout;
    let remote = parse_remote(&origin);
    let project = conf.get("forge_project").and_then(Value::as_str).map(str::to_string).or_else(|| remote.as_ref().map(|r| r.1.clone()))?;
    let forge = s("forge").unwrap_or_else(|| {
        if remote.as_ref().is_some_and(|r| r.0 == "github.com") { "github".into() } else { "gitlab".into() }
    });
    match forge.as_str() {
        "gitlab" => gitlab_mr(&project, branch),
        "github" => github_mr(&project, branch),
        "command" => {
            let template = conf.get("forge_cmd").or(config::get("forge_cmd").as_ref()).and_then(Value::as_array).cloned()?;
            let argv: Vec<String> = template.iter().filter_map(Value::as_str)
                .map(|a| a.replace("{project}", &project).replace("{branch}", branch).replace("{path}", path).replace("{repo}", repo))
                .collect();
            let (cmd, args) = argv.split_first()?;
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            serde_json::from_value(json_of(cmd, &refs)?).ok()
        }
        _ => None,
    }
}

fn github_mr(project: &str, branch: &str) -> Option<MergeRequest> {
    let list = json_of("gh", &["pr", "list", "--repo", project, "--head", branch, "--state", "all", "--limit", "1",
                               "--json", "number,url,state,isDraft,title,statusCheckRollup"])?;
    let pr = list.as_array()?.first()?.clone();
    let checks: Vec<&Value> = pr.get("statusCheckRollup").and_then(Value::as_array).map(|a| a.iter().collect()).unwrap_or_default();
    let field = |c: &Value, k: &str| c.get(k).and_then(Value::as_str).unwrap_or("").to_ascii_uppercase();
    let failed = checks.iter().any(|c| ["FAILURE", "ERROR", "CANCELLED", "TIMED_OUT"].contains(&field(c, "conclusion").as_str())
        || ["FAILURE", "ERROR"].contains(&field(c, "state").as_str()));
    let pending = checks.iter().any(|c| {
        let st = field(c, "status");
        (!st.is_empty() && st != "COMPLETED") || field(c, "state") == "PENDING"
    });
    let state = match pr.get("state").and_then(Value::as_str).unwrap_or("OPEN") {
        "MERGED" => "merged",
        "CLOSED" => "closed",
        _ if pr.get("isDraft").and_then(Value::as_bool).unwrap_or(false) => "draft",
        _ => "opened",
    };
    Some(MergeRequest {
        number: pr.get("number")?.as_u64()?,
        url: pr.get("url").and_then(Value::as_str).unwrap_or("").to_string(),
        state: state.into(),
        checks: if checks.is_empty() { None } else if failed { Some("failing".into()) } else if pending { Some("pending".into()) } else { Some("passing".into()) },
        title: pr.get("title").and_then(Value::as_str).map(str::to_string),
        pipeline_url: None,
    })
}

/** The newest MR for a branch, as bin/dashboard's `_mr` reads it. */
fn gitlab_mr(project: &str, branch: &str) -> Option<MergeRequest> {
    let glab_json = |p: &str| json_of("glab", &["api", p]);
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
        title: m.get("title").and_then(Value::as_str).map(str::to_string),
        pipeline_url: m.pointer("/head_pipeline/web_url").and_then(Value::as_str).map(str::to_string),
    })
}

fn sc(args: &[&str]) -> Value {
    let shim = util::agent_box_dir().join("shim").join("sc");
    let mut all = vec![shim.to_string_lossy().into_owned()];
    all.extend(args.iter().map(|s| s.to_string()));
    let refs: Vec<&str> = all.iter().map(|s| s.as_str()).collect();
    let home = util::home();
    let o = util::run("python3", &refs, &[], Some(&home.to_string_lossy()), config::secs("sc_list_timeout_secs", 60));
    serde_json::from_str(&o.stdout).unwrap_or(Value::Null)
}

/** Chat state from its latest hook: (state, waiting_on). */
pub type ChatStates = HashMap<String, (String, Option<Value>)>;

/** The chat state a hook event puts a chat in; `hook_states` ({event: state, or null to ignore it})
overrides these. */
pub fn state_for_event(event: &str) -> Option<String> {
    if let Some(map) = crate::config::get("hook_states").and_then(|m| m.as_object().cloned()) {
        if let Some(v) = map.get(event) {
            return v.as_str().map(str::to_string);
        }
    }
    match event {
        "UserPromptSubmit" | "PreToolUse" | "PostToolUse" => Some("working"),
        "Stop" => Some("idle"),
        "Notification" | "PermissionRequest" => Some("waiting"),
        "SessionEnd" => Some("stopped"),
        _ => None,
    }
    .map(str::to_string)
}

/** A Notification that blocks on the user (a permission or question), not the idle reminder. */
/** Whether a Notification hook means the chat needs you: its type is in `waiting_notification_types`, or
it has no type and its message doesn't start with `idle_notification_prefix`. */
pub fn needs_user(payload: &Value) -> bool {
    let kind = payload.get("notification_type").and_then(Value::as_str).unwrap_or("");
    let message = payload.get("message").and_then(Value::as_str).unwrap_or("");
    let waiting: Vec<String> = crate::config::get("waiting_notification_types")
        .and_then(|v| v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()))
        .unwrap_or_else(|| vec!["permission_prompt".into(), "elicitation_dialog".into()]);
    match kind {
        k if waiting.iter().any(|w| w == k) => true,
        "" => !message.starts_with(&crate::config::text("idle_notification_prefix", "Claude is waiting for your input")),
        _ => false,
    }
}

/** A chat's tmux pane, with its colors. */
fn capture(session: &str) -> Option<String> {
    let o = util::run("tmux", &["capture-pane", "-p", "-e", "-t", &format!("={session}:")], &[], None, util::tmux_wait());
    o.ok.then_some(o.stdout)
}

fn strip_ansi(s: &str) -> String {
    static ANSI: OnceLock<Regex> = OnceLock::new();
    let re = ANSI.get_or_init(|| Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(\x07|\x1b\\)|\x1b[@-_]").unwrap());
    re.replace_all(s, "").into_owned()
}

const WAITING_PATTERN: &str = r"(?i)Enter to select|Esc to cancel|Do you want to proceed|❯ 1\. Yes";

/** Whether the pane's last `waiting_pane_lines` lines show a prompt matching the agent's `waiting_pattern`. */
fn pane_waiting(pane: &str, agent: &str) -> bool {
    static WAITING: Mutex<Option<HashMap<String, Regex>>> = Mutex::new(None);
    let pattern = crate::agents::text(agent, "waiting_pattern", WAITING_PATTERN);
    let mut cached = WAITING.lock().unwrap();
    let re = cached.get_or_insert_with(HashMap::new).entry(pattern.clone())
        .or_insert_with(|| Regex::new(&pattern).or_else(|_| Regex::new(WAITING_PATTERN)).unwrap());
    let text = strip_ansi(pane);
    let lines: Vec<&str> = text.lines().collect();
    re.is_match(&lines[lines.len().saturating_sub(crate::config::count("waiting_pane_lines", 25))..].join("\n"))
}

const STATUS_PATTERN: &str = r"^\s*[^\w\s]\s+(?P<verb>\w[^…]*?)…\s*\((?P<rest>[^)]*)\)\s*$";
const STATUS_DROP: &str = r"(?i)esc to interrupt";

/** The agent's working line, the last in its pane's bottom `waiting_pane_lines` lines matching its
`status_pattern` (groups `verb` and `rest`): `rest` splits on `status_separator` into a time ("4m 17s"),
a token count ("↓ 6.5k tokens") and notes, less parts matching `status_drop`. */
fn pane_activity(pane: &str, agent: &str) -> Option<(String, Option<f64>, Option<String>, Vec<String>)> {
    static RES: Mutex<Option<HashMap<String, Option<Regex>>>> = Mutex::new(None);
    let mut cached = RES.lock().unwrap();
    let cache = cached.get_or_insert_with(HashMap::new);
    let mut re = |key: &str, default: &str| {
        let pattern = crate::agents::text(agent, key, default);
        cache.entry(pattern.clone()).or_insert_with(|| Regex::new(&pattern).ok()).clone()
    };
    let (line, drop) = (re("status_pattern", STATUS_PATTERN)?, re("status_drop", STATUS_DROP));
    let sep = crate::agents::text(agent, "status_separator", " · ");
    let text = strip_ansi(pane);
    let lines: Vec<&str> = text.lines().collect();
    let tail = &lines[lines.len().saturating_sub(crate::config::count("waiting_pane_lines", 25))..];
    let m = tail.iter().rev().find_map(|l| line.captures(l))?;
    let (mut secs, mut tokens, mut notes) = (None, None, vec![]);
    for part in m.name("rest").map_or("", |r| r.as_str()).split(sep.as_str()).map(str::trim).filter(|p| !p.is_empty()) {
        if drop.as_ref().is_some_and(|d| d.is_match(part)) {
        } else if let Some(t) = duration_secs(part).filter(|_| secs.is_none()) {
            secs = Some(t);
        } else if part.ends_with("tokens") && tokens.is_none() {
            tokens = Some(part.to_string());
        } else {
            notes.push(part.to_string());
        }
    }
    Some((m.name("verb")?.as_str().trim().to_string(), secs, tokens, notes))
}

/** Seconds in "1h 2m 3s" / "4m 17s" / "12s". */
fn duration_secs(text: &str) -> Option<f64> {
    let mut total = 0.;
    for w in text.split_whitespace() {
        let n: f64 = w.get(..w.len() - 1)?.parse().ok()?;
        total += n * match w.chars().last()? {
            'h' => 3600.,
            'm' => 60.,
            's' => 1.,
            _ => return None,
        };
    }
    (!text.trim().is_empty()).then_some(total)
}

/** A chat's activity from its latest status line: `since` holds steady across reads (the shown time
rounds), and `changed_at` moves only when the token count does. */
fn track_activity(id: &str, read: Option<(String, Option<f64>, Option<String>, Vec<String>)>) -> Option<Activity> {
    static SEEN: Mutex<Option<HashMap<String, Activity>>> = Mutex::new(None);
    let mut seen = SEEN.lock().unwrap();
    let seen = seen.get_or_insert_with(HashMap::new);
    let Some((verb, secs, tokens, notes)) = read else {
        seen.remove(id);
        return None;
    };
    let now = util::now();
    let since = secs.map(|s| now - s);
    let next = match seen.get(id) {
        Some(prev) => Activity {
            since: match (prev.since, since) {
                (Some(p), Some(s)) if (p - s).abs() < 3. => Some(p),
                _ => since,
            },
            changed_at: if prev.tokens == tokens { prev.changed_at } else { now },
            verb,
            tokens,
            notes,
        },
        None => Activity { verb, since, tokens, changed_at: now, notes },
    };
    seen.insert(id.to_string(), next.clone());
    Some(next)
}

/** What's typed after the last line starting with `prompt` (the input box), without dim or grey text
(a placeholder or suggestion). None: no such line. */
pub(crate) fn typed_input(pane: &str, prompt: &str) -> Option<String> {
    let line = pane.lines().rev().find(|l| strip_ansi(l).trim_start().starts_with(prompt))?;
    // Map the plain text back onto the raw line: drop each SGR-faint run.
    static SGR: OnceLock<Regex> = OnceLock::new();
    let sgr = SGR.get_or_init(|| Regex::new(r"\x1b\[([0-9;]*)m").unwrap());
    let raw = &line[line.find(prompt)? + prompt.len()..];
    let (mut faint, mut out, mut pos) = (false, String::new(), 0);
    for m in sgr.captures_iter(raw) {
        let whole = m.get(0)?;
        if !faint {
            out.push_str(&strip_ansi(&raw[pos..whole.start()]));
        }
        pos = whole.end();
        for p in m[1].split(';').map(|p| p.parse::<u32>().unwrap_or(0)) {
            match p {
                0 | 22 | 39 => faint = false,
                2 | 90 => faint = true,
                _ => {}
            }
        }
    }
    if !faint {
        out.push_str(&strip_ansi(&raw[pos..]));
    }
    Some(out.trim().to_string())
}

/** The prompt suggestion in Claude Code's input box: the line after "❯\u{a0}" between two rules,
when everything on it is drawn dim or grey (typed text is drawn plain). */
pub(crate) fn suggestion(pane: &str) -> Option<String> {
    let raw: Vec<&str> = pane.lines().collect();
    let plain: Vec<String> = raw.iter().map(|l| strip_ansi(l)).collect();
    // `suggestion_prompt` (the input box's prompt and the no-break space after it) between two
    // `suggestion_rule` lines.
    let rule_mark = crate::config::text("suggestion_rule", "──");
    let marker = crate::config::text("suggestion_prompt", "❯\u{a0}");
    let rule = |i: usize| plain.get(i).is_some_and(|l| l.trim_start().starts_with(rule_mark.as_str()));
    let i = (1..plain.len()).rev().find(|&i| plain[i].trim_start().starts_with(marker.as_str()) && rule(i - 1) && rule(i + 1))?;
    let rest = &raw[i][raw[i].find(marker.as_str())? + marker.len()..];
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
    // A session with no turns yet shows a placeholder (`Try "fix lint errors"`) in the same style.
    let prefix = crate::config::text("suggestion_placeholder_prefix", "Try \"");
    let placeholder = !prefix.is_empty() && text.starts_with(prefix.as_str()) && text.ends_with('"');
    (!text.is_empty() && !placeholder).then_some(text)
}

/** A chat's state from its transcript's newest turn event and its newest hook: a finished turn is idle
whatever the hooks said last (Esc fires none), and a running one is working unless a hook says waiting. */
fn combine(turn: Option<String>, hook: Option<(String, Option<Value>)>) -> Option<(String, Option<Value>)> {
    match turn.as_deref() {
        None => hook,
        Some("working") => Some(hook.filter(|(st, _)| st == "waiting").unwrap_or(("working".into(), None))),
        Some(st) => Some((st.to_string(), None)),
    }
}

fn chats(states: &ChatStates) -> Vec<Chat> {
    let dir = util::agent_home().join("state").join("registry").join("chats");
    let here = hosts::here();
    let foreign = hosts::flag("foreign_sessions", cfg!(target_os = "macos"));
    let alive = util::tmux_sessions();
    let mut out = vec![];
    let mut elsewhere: Option<std::collections::HashSet<String>> = None;
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
        let host = s("host").unwrap_or_else(registry_default_host);
        if host != here {
            continue;
        }
        let tmux = util::chat_session(&id);
        let is_alive = alive.contains(&tmux);
        // A stopped chat another tool here still runs (super.engineering, on the Mac): starting it takes it over.
        let status = if !is_alive && foreign && elsewhere
            .get_or_insert_with(crate::elsewhere::sessions_running)
            .contains(&crate::agents::session_key(&v).unwrap_or_else(|| id.clone())) { "running_elsewhere".to_string() } else { status };
        let dir = s("dir");
        let agent = crate::agents::of(&v);
        let transcript = crate::agents::transcript_path(&v);
        let last = transcript.as_deref().and_then(util::mtime);
        let hook = crate::agents::session_key(&v).and_then(|k| states.get(&k)).filter(|(st, _)| st != "stopped").cloned();
        // Agents whose transcripts mark each turn (Codex) say there whether they're working; hooks add waiting.
        let turns = crate::agents::get(&agent, "turn_events").and_then(|t| t.as_object().cloned());
        let turn = turns.as_ref().zip(transcript.as_deref()).and_then(|(t, p)| crate::codex::turn_state(p, t));
        let hook = combine(turn, hook);
        let pane = if is_alive && (turns.is_some() || hook.as_ref().is_none_or(|(st, _)| st == "idle" || st == "working")) { capture(&tmux) } else { None };
        let pane_waits = pane.as_deref().is_some_and(|p| pane_waiting(p, &agent));
        let (state, waiting_on) = match hook {
            _ if !is_alive => ("stopped".to_string(), None),
            Some((st, w)) if !(turns.is_some() && pane_waits && st != "waiting") => (st, w),
            _ if pane_waits => ("waiting".to_string(), None),
            _ => ("idle".to_string(), None),
        };
        let suggestion = if state == "idle" { pane.as_deref().and_then(suggestion) } else { None };
        let activity = track_activity(&id, pane.as_deref().filter(|_| state == "working").and_then(|p| pane_activity(p, &agent)));
        // A name nobody chose follows the agent's own title (auto_title), and the registry learns it.
        let mut name = s("name").unwrap_or_default();
        if config::flag("auto_title", true) && crate::titles::auto_named(&v) {
            if let Some(t) = crate::titles::title(&v, &agent, transcript.as_deref()).filter(|t| *t != name) {
                crate::titles::persist(&id, &t);
                name = t;
            }
        }
        out.push(Chat {
            id,
            name,
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
            agent,
            session: s("session_id"),
            repo: s("repo"),
            base: s("base"),
            model: s("model"),
            parent: s("parent"),
            feature: s("feature"),
            created_at: s("created_at"),
            forked_from: s("forked_from"),
            launched_at: v.get("launched_at").and_then(Value::as_f64),
            transcript: transcript.as_ref().map(|p| p.to_string_lossy().into_owned()),
            status_line: activity,
        });
    }
    out
}

fn worktree(caches: &Arc<Caches>, d: &Value, chats: &[Chat]) -> Option<Worktree> {
    let path = d.get("path")?.as_str()?.to_string();
    let s = |k: &str| d.get(k).and_then(|x| x.as_str()).map(|x| x.to_string());
    let project = s("project_name").or_else(|| s("project_id")).unwrap_or_else(hosts::default_repo);
    let known = hosts::repos();
    let repo = s("project_id").filter(|id| known.contains_key(id)).unwrap_or_else(|| hosts::repo_for(&project));
    let hue = hosts::hue(&repo);
    let is_primary = d.get("is_primary").and_then(|v| v.as_bool()).unwrap_or(false);
    let branch = s("branch");
    let git = caches.git(&path);
    let real = util::realpath(&path);
    let mine: Vec<&Chat> = chats.iter().filter(|c| c.dir.as_deref().map(util::realpath).as_deref() == Some(&real)).collect();
    let age = mine.iter().filter_map(|c| c.last_activity).chain(git.last_commit).fold(None, |a: Option<f64>, b| {
        Some(a.map_or(b, |a| a.max(b)))
    });
    let mr = match (&branch, is_primary) {
        (Some(b), false) => caches.mr(&repo, b, &path),
        _ => None,
    };
    Some(Worktree {
        id: s("id").unwrap_or_default(),
        title: s("title").or_else(|| branch.clone()).unwrap_or_else(|| s("worktree").unwrap_or_default()),
        path,
        branch,
        project,
        repo,
        hue,
        is_primary,
        uncommitted: git.uncommitted,
        unpushed: git.unpushed,
        behind: git.behind,
        mr,
        age,
        chat_ids: mine.iter().map(|c| c.id.clone()).collect(),
        host: hosts::here(),
        external: d.get("external").and_then(|v| v.as_bool()).unwrap_or(false),
        target_branch: s("target_branch"),
        behind_count: git.behind_count,
        upstream: git.upstream,
    })
}

/** bin/sync-worker's last push (sync-status.json), whether one is pending, and the queue's length. */
fn sync_status() -> Option<SyncStatus> {
    let home = util::agent_home();
    let v: Value = std::fs::read_to_string(home.join("sync-status.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
    let queued = std::fs::read_dir(home.join("queue")).map(|d| d.flatten().count() as u32).unwrap_or(0);
    let pending = home.join("push-pending").exists();
    if v.is_null() && queued == 0 && !pending {
        return None;
    }
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    Some(SyncStatus { pending, queued, last_push_at: s("last_push_at"), error: s("error"), branch: s("branch") })
}

pub fn build(caches: &Arc<Caches>, states: &ChatStates, last_seq: i64) -> Model {
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
    let here = hosts::here();
    let ssh = hosts::ssh();
    let ssh_host = if ssh.is_empty() { ssh } else { std::env::var("AGENT_BOX_SSH_HOST").unwrap_or(ssh) };
    Model {
        host_label: Some(hosts::label(&here)),
        host: here,
        ssh_host,
        projects,
        sections,
        chats,
        last_seq,
        sync: sync_status(),
    }
}

#[cfg(test)]
mod tests {
    use super::{combine, pane_activity, parse_head, parse_remote, suggestion, track_activity, typed_input, GitInfo};
    use serde_json::json;

    #[test]
    fn remotes_name_their_host_and_project() {
        let p = |u: &str| parse_remote(u).map(|(h, p)| format!("{h} {p}"));
        assert_eq!(p("git@gitlab.com:redo/redoapp/product/redo.git").as_deref(), Some("gitlab.com redo/redoapp/product/redo"));
        assert_eq!(p("https://github.com/acme/app.git").as_deref(), Some("github.com acme/app"));
        assert_eq!(p("ssh://git@github.com:22/acme/app").as_deref(), Some("github.com acme/app"));
        assert_eq!(p("https://oauth2:tok@gitlab.com/g/s/r.git\n").as_deref(), Some("gitlab.com g/s/r"));
    }

    #[test]
    fn the_status_line_gives_upstream_and_counts() {
        let mut g = GitInfo::default();
        parse_head("## tj/x...origin/tj/x [ahead 2, behind 3]", &mut g);
        assert_eq!((g.unpushed, g.behind_count, g.behind, g.upstream.as_deref()), (2, 3, true, Some("origin/tj/x")));
        let mut g = GitInfo::default();
        parse_head("## main", &mut g);
        assert_eq!((g.unpushed, g.behind_count, g.upstream), (0, 0, None));
    }

    #[test]
    fn a_turn_event_outranks_a_stale_hook() {
        let hook = |s: &str| Some((s.to_string(), None));
        assert_eq!(combine(Some("idle".into()), hook("working")), hook("idle"));
        assert_eq!(combine(Some("working".into()), hook("idle")), hook("working"));
        let asks = Some(("waiting".to_string(), Some(json!({"tool_name": "Bash"}))));
        assert_eq!(combine(Some("working".into()), asks.clone()), asks);
        assert_eq!(combine(None, hook("working")), hook("working"));
        assert_eq!(combine(None, None), None);
    }

    #[test]
    fn typed_input_skips_a_dim_placeholder() {
        let codex = |input: &str| format!("› Run tests\n• done\n\x1b[1m›\x1b[0m {input}\n  GPT · 5h 100% left");
        assert_eq!(typed_input(&codex("\x1b[2mAsk Codex to do anything\x1b[0m"), "›").as_deref(), Some(""));
        assert_eq!(typed_input(&codex("hello typed"), "›").as_deref(), Some("hello typed"));
        assert_eq!(typed_input("no prompt here", "›"), None);
    }

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
        assert_eq!(suggestion(&pane("\x1b[2mTry \"fix lint errors\"\x1b[0m")), None);
    }

    #[test]
    fn the_spinner_line_reads_as_activity() {
        let p = "⏺ Reading\n    · Click to expand collapsed tool results\n\x1b[38;5;174m✢\x1b[39m Concocting… (1h 4m 17s · ↓ 6.5k tokens · esc to interrupt)\n❯ \n";
        let (verb, secs, tokens, notes) = pane_activity(p, "claude").unwrap();
        assert_eq!((verb.as_str(), secs, tokens.as_deref(), notes.len()), ("Concocting", Some(3857.), Some("↓ 6.5k tokens"), 0));
        let (_, secs, tokens, notes) = pane_activity("· Hullaballooing… (1s · thinking with high effort)", "claude").unwrap();
        assert_eq!((secs, tokens, notes), (Some(1.), None, vec!["thinking with high effort".to_string()]));
        assert!(pane_activity("✻ Worked for 5m 27s · done 10:58 PM\n❯ ", "claude").is_none());
    }

    #[test]
    fn activity_keeps_its_start_and_dates_the_last_new_output() {
        let read = |s: f64, t: &str| Some(("Thinking".to_string(), Some(s), Some(t.to_string()), vec![]));
        let a = track_activity("t1", read(10., "↓ 1k tokens")).unwrap();
        let b = track_activity("t1", read(11., "↓ 1k tokens")).unwrap();
        assert_eq!((b.since, b.changed_at), (a.since, a.changed_at));
        assert!(track_activity("t1", None).is_none());
        let c = track_activity("t1", read(500., "↓ 2k tokens")).unwrap();
        assert!(c.since.unwrap() < a.since.unwrap() - 400.);
    }
}
