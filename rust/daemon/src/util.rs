use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/home/coder".into()))
}

pub fn agent_home() -> PathBuf {
    home().join(".agent-box")
}

pub fn socket_path() -> PathBuf {
    agent_home().join("boxd.sock")
}

pub fn db_path() -> PathBuf {
    agent_home().join("boxd.db")
}

/** The agent-box checkout this binary was built in (rust/target/release/agent-boxd), else ~/agent-box. */
pub fn agent_box_dir() -> PathBuf {
    let built_in = std::env::current_exe()
        .ok()
        .and_then(|e| std::fs::canonicalize(e).ok())
        .and_then(|e| e.ancestors().nth(4).map(|p| p.to_path_buf()))
        .filter(|p| p.join("bin").join("agent-env").exists());
    built_in.unwrap_or_else(|| home().join("agent-box"))
}

pub fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

pub fn mtime(path: &Path) -> Option<f64> {
    let m = std::fs::metadata(path).ok()?.modified().ok()?;
    m.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs_f64())
}

pub fn realpath(path: &str) -> String {
    std::fs::canonicalize(path).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| path.to_string())
}

/** Claude Code's project folder name for a directory: every non-alphanumeric char becomes '-'. */
pub fn slug(path: &str) -> String {
    path.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

/** A chat's session file. Claude Code keys it by the resolved folder (~/dev/redo links to ~/project). */
pub fn transcript_path(dir: &str, chat_id: &str) -> PathBuf {
    let at = |d: &str| home().join(".claude").join("projects").join(slug(d)).join(format!("{chat_id}.jsonl"));
    let resolved = at(&realpath(dir));
    if resolved.exists() { resolved } else if at(dir).exists() { at(dir) } else { resolved }
}

pub struct Output {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    pub fn message(&self) -> String {
        let s = format!("{}\n{}", self.stderr.trim(), self.stdout.trim());
        s.trim().to_string()
    }
}

/** Run a command, killing it after `timeout`. */
pub fn run(cmd: &str, args: &[&str], envs: &[(&str, &str)], cwd: Option<&str>, timeout: Duration) -> Output {
    run_with(cmd, args, envs, &[], cwd, None, timeout)
}

/** `run`, also dropping env vars that start with any of `unset`, and feeding `input` on stdin. */
pub fn run_with(cmd: &str, args: &[&str], envs: &[(&str, &str)], unset: &[&str], cwd: Option<&str>,
                input: Option<&str>, timeout: Duration) -> Output {
    let mut c = Command::new(cmd);
    let stdin = if input.is_some() { Stdio::piped() } else { Stdio::null() };
    c.args(args).stdin(stdin).stdout(Stdio::piped()).stderr(Stdio::piped());
    for (k, _) in std::env::vars() {
        if unset.iter().any(|p| k.starts_with(p)) {
            c.env_remove(k);
        }
    }
    for (k, v) in envs {
        c.env(k, v);
    }
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    let mut child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) => return Output { ok: false, stdout: String::new(), stderr: format!("{cmd}: {e}") },
    };
    if let (Some(text), Some(mut w)) = (input, child.stdin.take()) {
        use std::io::Write;
        let text = text.to_string();
        std::thread::spawn(move || {
            let _ = w.write_all(text.as_bytes());
        });
    }
    let mut out = child.stdout.take();
    let mut err = child.stderr.take();
    let t_out = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(o) = out.as_mut() {
            let _ = o.read_to_string(&mut s);
        }
        s
    });
    let t_err = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(e) = err.as_mut() {
            let _ = e.read_to_string(&mut s);
        }
        s
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let stdout = t_out.join().unwrap_or_default();
    let mut stderr = t_err.join().unwrap_or_default();
    if status.is_none() {
        stderr.push_str(&format!("\n{cmd} timed out"));
    }
    Output { ok: status.map(|s| s.success()).unwrap_or(false), stdout, stderr }
}

pub fn tmux_sessions() -> std::collections::HashSet<String> {
    let o = run("tmux", &["ls", "-F", "#S"], &[], None, Duration::from_secs(5));
    o.stdout.lines().map(|l| l.to_string()).collect()
}

pub fn chat_session(chat_id: &str) -> String {
    format!("chat-{}", &chat_id[..chat_id.len().min(8)])
}

/** A folder's terminal session (`sh-<sha1(realpath)[:10]>`, created on first use): its first shell, or another one `extra` names; `command` runs instead of a
login shell (and the pane stays open after it ends). */
pub fn shell_session(path: &str, extra: Option<&str>, command: Option<&str>) -> String {
    let real = realpath(path);
    let base = format!("sh-{}", &sha1_smol::Sha1::from(real.as_bytes()).digest().to_string()[..10]);
    let name = match extra.filter(|e| !e.is_empty()) {
        Some(e) => format!("{base}-{}", e.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect::<String>()),
        None => base,
    };
    let target = format!("={name}");
    if !run("tmux", &["has-session", "-t", &target], &[], None, Duration::from_secs(5)).ok {
        let env = agent_box_dir().join("bin").join("agent-env");
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "bash".into());
        let env = env.to_string_lossy();
        let mut args = vec!["new-session", "-d", "-s", &name, "-c", &real, &env];
        match command {
            Some(c) => args.extend(["sh", "-c", c]),
            None => args.extend([shell.as_str(), "-l"]),
        }
        run("tmux", &args, &[], None, Duration::from_secs(10));
        run("tmux", &["set", "-t", &name, "status", "off"], &[], None, Duration::from_secs(5));
        if command.is_some() {
            run("tmux", &["set", "-t", &name, "remain-on-exit", "on"], &[], None, Duration::from_secs(5));
        }
    }
    name
}
