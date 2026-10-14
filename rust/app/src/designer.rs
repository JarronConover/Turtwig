/*! The designer: a local Claude Code session in the settings folder that redesigns the app live.
It runs as one long `claude -p` stream-json process; each saved edit hot-reloads the window. */

use crate::settings;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};

pub const CHAT_ID: &str = "designer";

const PROMPT: &str = "You are the live designer for the Agent Box app. The user is watching the app while you work: every save of settings.jsonc (in the current folder) reloads the window and animates the change. Before your first change, read SETTINGS.md here; it documents every setting and holds the full defaults. Edit only settings.jsonc and keep the user's existing overrides. Give nodes you add or move a stable `id` and keep it when moving them, so they glide to their new place. After each save, read problems.txt and fix whatever it lists. Reply in one or two plain sentences.";

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

/** The saved session's earlier messages, so the panel shows the conversation after a restart. */
pub fn past_entries() -> Vec<Value> {
    let Ok(id) = std::fs::read_to_string(session_file()) else { return vec![] };
    let slug: String = settings::dir().to_string_lossy().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let home = std::env::var("HOME").unwrap_or_default();
    let path = PathBuf::from(home).join(".claude/projects").join(slug).join(format!("{}.jsonl", id.trim()));
    std::fs::read_to_string(path).map(|t| t.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()).unwrap_or_default()
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
        let mut cmd = Command::new(format!("{home}/.local/bin/claude"));
        cmd.current_dir(settings::dir())
            .env("PATH", format!("{home}/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"))
            .args(["-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose"])
            .args(["--permission-mode", "acceptEdits", "--tools", "Read", "Edit", "Write", "Glob", "Grep"])
            .args(["--append-system-prompt", PROMPT])
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
