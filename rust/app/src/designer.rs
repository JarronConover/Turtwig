/*! The designer: a local Claude Code session in the settings folder that redesigns the app live.
It runs as one long `claude -p` stream-json process; each saved edit hot-reloads the window. */

use crate::settings;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};

pub const CHAT_ID: &str = "designer";

const PROMPT: &str = "You are the live designer for the a.box app. The user is watching the app while you work: every save of settings.jsonc (in the current folder) reloads the window and animates the change. Before your first change, read SETTINGS.md here; it documents every setting and holds the full defaults. Edit only settings.jsonc and keep the user's existing overrides. Prefer small `patch` ops on default nodes by id (move, set, insert, replace, remove) over copying whole components or views: copies stop picking up improvements to the defaults. Give nodes you add or move a stable `id` and keep it when moving them, so they glide to their new place. After each save, read problems.txt and fix whatever it lists. To see the app as the user does, read inspect.json (live state and layout) and activity.log (what they just did), or write {\"do\": \"app.snapshot\"} to run.json and then read snapshot.png. When a change needs behavior the settings can't express (new data, new actions, timers, calls to the box or the web), write a Rust script in scripts/<name>.rs (see Scripts in SETTINGS.md; it compiles in a few seconds, and problems.txt shows compile errors) instead of saying it can't be done. Reply in one or two plain sentences.";

#[derive(Default)]
pub struct Designer {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    pub working: bool,
}

pub enum Out {
    Entry(Value),
    Done { error: Option<String> },
    Exited,
}

fn session_file() -> PathBuf {
    settings::dir().join("designer-session")
}

pub fn history_dir() -> PathBuf {
    settings::dir().join("history")
}

/** Settings `designer` and the app's PATH, as of the last load (`configure`). */
static CONFIG: std::sync::RwLock<Option<(Value, String)>> = std::sync::RwLock::new(None);

pub fn configure(designer: &Value, path: &str) {
    *CONFIG.write().unwrap() = Some((designer.clone(), path.to_string()));
}

fn config() -> (Value, String) {
    CONFIG.read().unwrap().clone().unwrap_or_else(|| (Value::Null, "/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin".into()))
}

/** Where the designer's conversations are kept: `designer.sessions_dir` (`{slug}` is the settings
folder as Claude Code names its project folders), else Claude Code's folder for the settings folder. */
fn sessions_dir() -> PathBuf {
    let slug: String = settings::dir().to_string_lossy().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    match config().0.get("sessions_dir").and_then(Value::as_str).filter(|d| !d.is_empty()) {
        Some(d) => PathBuf::from(crate::script::expand(&d.replace("{slug}", &slug))),
        None => PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".claude/projects").join(slug),
    }
}

pub fn current() -> Option<String> {
    std::fs::read_to_string(session_file()).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/** The saved session's earlier messages, so the panel shows the conversation after a restart. */
pub fn past_entries() -> Vec<Value> {
    let Some(id) = current() else { return vec![] };
    std::fs::read_to_string(sessions_dir().join(format!("{id}.jsonl"))).map(|t| t.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()).unwrap_or_default()
}

/** The text of a transcript entry's message, when it's the user's. */
fn user_text(e: &Value) -> Option<String> {
    if e.get("type").and_then(Value::as_str) != Some("user") {
        return None;
    }
    let c = &e["message"]["content"];
    c.as_str().map(str::to_string).or_else(|| c.as_array()?.iter().find_map(|b| (b["type"] == "text").then(|| b["text"].as_str().map(str::to_string)).flatten()))
}

/** Every designer conversation, newest first: `{id, title, first, messages, last, current}` (title: the
latest thing you asked, which is how you'd recognize it; first: what it started with; last: seconds since
the epoch). One-shot `settings.agent` runs share the folder and are left out. */
pub fn sessions() -> Vec<Value> {
    let current = current();
    let one_line = |t: &str| -> String { t.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(160).collect() };
    let mut out: Vec<(f64, Value)> = std::fs::read_dir(sessions_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let id = p.file_name()?.to_str()?.strip_suffix(".jsonl")?.to_string();
            let last = e.metadata().ok()?.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs_f64();
            let text = std::fs::read_to_string(&p).ok()?;
            // What you typed: not a compaction summary or an attached image's caption.
            let asked: Vec<String> = text
                .lines()
                .filter_map(|l| serde_json::from_str::<Value>(l).ok())
                .filter_map(|e| user_text(&e))
                .filter(|t| !t.starts_with("This session is being continued") && !t.starts_with("[Image"))
                .collect();
            let first = asked.first()?;
            if first.starts_with("You are customizing the ") {
                return None;
            }
            let is_current = current.as_deref() == Some(id.as_str());
            Some((last, json!({
                "id": id, "title": one_line(asked.last()?), "first": one_line(first), "messages": asked.len(),
                "last": last, "current": is_current,
            })))
        })
        .collect();
    out.sort_by(|a, b| b.0.total_cmp(&a.0));
    out.into_iter().map(|(_, v)| v).collect()
}

/** Saves a copy of the settings file so `settings.undo` can put it back. */
pub fn snapshot() {
    let dir = history_dir();
    let _ = std::fs::create_dir_all(&dir);
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let _ = std::fs::copy(settings::path(), dir.join(format!("{stamp}.jsonc")));
}

/** Restores the newest snapshot (and drops it); false when there is none. */
pub fn undo() -> bool {
    let mut files: Vec<PathBuf> = std::fs::read_dir(history_dir()).into_iter().flatten().flatten().map(|e| e.path()).collect();
    files.sort();
    let Some(last) = files.pop() else { return false };
    std::fs::copy(&last, settings::path()).is_ok() && std::fs::remove_file(last).is_ok()
}

impl Designer {
    /** The process ended; the next message starts it again, resuming the session. */
    pub fn reset_process(&mut self) {
        self.child = None;
        self.stdin = None;
        self.working = false;
    }

    /** Continue an earlier conversation: the next message resumes it. */
    pub fn switch_to(&mut self, id: &str) {
        self.reset();
        let _ = std::fs::write(session_file(), id);
    }

    pub fn reset(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
        }
        self.stdin = None;
        self.working = false;
        let _ = std::fs::remove_file(session_file());
    }

    /** Sends a message, starting (or resuming) the session first; output lines arrive on `tx`. */
    pub fn send(&mut self, text: &str, tx: flume::Sender<Out>) -> Result<(), String> {
        if self.stdin.is_none() {
            self.start(tx)?;
        }
        let line = json!({"type": "user", "message": {"role": "user", "content": text}}).to_string();
        let stdin = self.stdin.as_mut().ok_or("the designer didn't start")?;
        writeln!(stdin, "{line}").and_then(|_| stdin.flush()).map_err(|e| e.to_string())?;
        self.working = true;
        Ok(())
    }

    fn start(&mut self, tx: flume::Sender<Out>) -> Result<(), String> {
        let home = std::env::var("HOME").unwrap_or_default();
        // settings `designer`: what it may touch. It runs headless, so it can't stop to ask for a permission;
        // anything not allowed here is refused outright.
        let (cfg, path) = config();
        let list = |k: &str, d: &[&str]| -> Vec<String> {
            cfg.get(k).and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_else(|| d.iter().map(|x| x.to_string()).collect())
        };
        let mode = cfg.get("permission_mode").and_then(|v| v.as_str()).unwrap_or("acceptEdits").to_string();
        let claude = cfg.get("claude").and_then(|v| v.as_str()).map(crate::script::expand).unwrap_or_else(|| format!("{home}/.local/bin/claude"));
        let prompt = cfg.get("prompt").and_then(|v| v.as_str()).unwrap_or(PROMPT).to_string();
        let mut cmd = Command::new(claude);
        cmd.current_dir(settings::dir())
            .env("PATH", path)
            .args(["-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose"])
            .args(["--permission-mode", &mode, "--tools"])
            .args(list("tools", &["Read", "Edit", "Write", "Glob", "Grep"]));
        for dir in list("add_dirs", &["~/.agent-box/uploads"]) {
            cmd.args(["--add-dir", &crate::script::expand(&dir)]);
        }
        cmd.args(["--append-system-prompt", &prompt]);
        // The app plays its own sounds (settings `sounds`); unless `designer.hooks`, Claude Code hooks (such
        // as a Stop sound) would fire on every designer reply too.
        if cfg.get("hooks").and_then(|v| v.as_bool()) != Some(true) {
            cmd.args(["--settings", r#"{"disableAllHooks": true}"#]);
        }
        cmd.args(list("extra_args", &[]))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Ok(id) = std::fs::read_to_string(session_file()) {
            cmd.args(["--resume", id.trim()]);
        }
        let mut child = cmd.spawn().map_err(|e| format!("couldn't start claude: {e}"))?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        self.stdin = child.stdin.take();
        self.child = Some(child);
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                match v.get("type").and_then(Value::as_str) {
                    Some("system") if v.get("subtype").and_then(Value::as_str) == Some("init") => {
                        if let Some(id) = v.get("session_id").and_then(Value::as_str) {
                            let _ = std::fs::write(session_file(), id);
                        }
                    }
                    Some("assistant") | Some("user") => {
                        let _ = tx.send(Out::Entry(v));
                    }
                    Some("result") => {
                        let error = (v.get("is_error").and_then(Value::as_bool) == Some(true))
                            .then(|| v.get("result").and_then(Value::as_str).unwrap_or("The designer hit an error").to_string());
                        let _ = tx.send(Out::Done { error });
                    }
                    _ => {}
                }
            }
            let _ = tx.send(Out::Exited);
        });
        Ok(())
    }
}

impl Drop for Designer {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
        }
    }
}
