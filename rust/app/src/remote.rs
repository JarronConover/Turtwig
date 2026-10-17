/*! A connection to an `agent-boxd`: `ssh <host> <daemon> connect` for the box, or `<daemon> connect`
run here when the host is empty (this Mac). JSON lines both ways, reconnecting forever. Responses resolve the matching call; everything else arrives as `Incoming`. */

use agent_box_protocol::{encode, Call, Event, Request, ServerMessage};
use anyhow::{anyhow, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub enum Incoming {
    Connected,
    Disconnected(String),
    Event(Event),
}

type Pending = Arc<Mutex<HashMap<u64, flume::Sender<Result<Value>>>>>;

/** Closes the connection for good when dropped (the window that owned it closed). */
pub struct RemoteGuard(Arc<AtomicBool>);

impl Drop for RemoteGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Relaxed);
    }
}

/** The last lines the connection's process (ssh, or the local daemon) wrote to stderr. */
type ErrLines = Arc<Mutex<std::collections::VecDeque<String>>>;

const ERR_LINES: usize = 20;

#[derive(Clone)]
pub struct Remote {
    out: flume::Sender<String>,
    pending: Pending,
    next_id: Arc<AtomicU64>,
    errors: ErrLines,
}

impl Remote {
    /** Connects to `spec`'s daemon with `path` as the process's PATH, reconnecting forever. */
    pub fn start(spec: crate::behavior::HostSpec, path: String) -> (Remote, flume::Receiver<Incoming>, RemoteGuard) {
        let alive = Arc::new(AtomicBool::new(true));
        let (out_tx, out_rx) = flume::unbounded::<String>();
        let (in_tx, in_rx) = flume::unbounded::<Incoming>();
        let pending: Pending = Arc::default();
        let errors: ErrLines = Arc::default();
        let remote = Remote { out: out_tx, pending: pending.clone(), next_id: Arc::new(AtomicU64::new(1)), errors: errors.clone() };
        let flag = alive.clone();
        std::thread::spawn(move || loop {
            if !flag.load(Ordering::Relaxed) {
                break;
            }
            let err = session(&spec, &path, &out_rx, &in_tx, &pending, &flag, &errors);
            for (_, waiter) in pending.lock().unwrap().drain() {
                let _ = waiter.send(Err(anyhow!("disconnected")));
            }
            // ssh's own reason (an unknown host, a refused key) says more than "connection closed".
            let last = errors.lock().unwrap().back().cloned();
            let why = match last {
                Some(l) => format!("{err}: {l}"),
                None => err.to_string(),
            };
            let _ = in_tx.send(Incoming::Disconnected(why));
            std::thread::sleep(Duration::from_secs(spec.retry_secs));
        });
        (remote, in_rx, RemoteGuard(alive))
    }

    /** A connection to nothing: every call fails (for a host with no daemon configured). */
    pub fn detached() -> Remote {
        let (out, _) = flume::unbounded();
        Remote { out, pending: Arc::default(), next_id: Arc::new(AtomicU64::new(1)), errors: Arc::default() }
    }

    /** What the connection's process last wrote to stderr, oldest first. */
    pub fn stderr(&self) -> Vec<String> {
        self.errors.lock().unwrap().iter().cloned().collect()
    }

    pub async fn call(&self, call: Call) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = flume::bounded(1);
        self.pending.lock().unwrap().insert(id, tx);
        self.out.send(encode(&Request { id, call })).map_err(|_| anyhow!("connection closed"))?;
        rx.recv_async().await.map_err(|_| anyhow!("disconnected"))?
    }

    /** Fire and forget, for input that doesn't need an answer (terminal keystrokes). */
    pub fn send(&self, call: Call) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let _ = self.out.send(encode(&Request { id, call }));
    }
}

fn session(
    spec: &crate::behavior::HostSpec,
    path: &str,
    out_rx: &flume::Receiver<String>,
    in_tx: &flume::Sender<Incoming>,
    pending: &Pending,
    alive: &Arc<AtomicBool>,
    errors: &ErrLines,
) -> anyhow::Error {
    let mut cmd = if spec.ssh_host.is_empty() {
        let mut c = Command::new(crate::script::expand(&spec.daemon));
        c.arg("connect");
        c
    } else {
        let mut c = Command::new("/usr/bin/ssh");
        c.arg("-T").args(&spec.ssh_options).args([&spec.ssh_host, &format!("{} connect", spec.daemon)]);
        c
    };
    let child = cmd
        .env("PATH", path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => return e.into(),
    };
    errors.lock().unwrap().clear();
    if let Some(stderr) = child.stderr.take() {
        let errors = errors.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }
                let mut e = errors.lock().unwrap();
                e.push_back(line);
                while e.len() > ERR_LINES {
                    e.pop_front();
                }
            }
        });
    }
    let mut stdin = child.stdin.take().unwrap();
    let pid = child.id();
    let watch = alive.clone();
    let every = Duration::from_millis(spec.watch_ms.max(50));
    std::thread::spawn(move || {
        while watch.load(Ordering::Relaxed) {
            std::thread::sleep(every);
        }
        let _ = Command::new("/bin/kill").arg(pid.to_string()).status();
    });
    let stdout = child.stdout.take().unwrap();
    while out_rx.try_recv().is_ok() {}
    let _ = in_tx.send(Incoming::Connected);

    let writer_rx = out_rx.clone();
    let (stop_tx, stop_rx) = flume::bounded::<()>(1);
    std::thread::spawn(move || loop {
        let keep_going = flume::Selector::new()
            .recv(&writer_rx, |line| match line {
                Ok(line) => stdin.write_all(line.as_bytes()).and_then(|_| stdin.flush()).is_ok(),
                Err(_) => false,
            })
            .recv(&stop_rx, |_| false)
            .wait();
        if !keep_going {
            break;
        }
    });

    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    let err = loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break anyhow!("connection closed"),
            Ok(_) => match serde_json::from_str::<ServerMessage>(&line) {
                Ok(ServerMessage::Response(r)) => {
                    if let Some(waiter) = pending.lock().unwrap().remove(&r.id) {
                        let _ = waiter.send(match r.error {
                            Some(e) => Err(anyhow!(e)),
                            None => Ok(r.result.unwrap_or(Value::Null)),
                        });
                    }
                }
                Ok(ServerMessage::Event(e)) => {
                    let _ = in_tx.send(Incoming::Event(e));
                }
                Err(e) => {
                    use std::io::Write as _;
                    let log = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".agent-box/remote.log");
                    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log) {
                        let _ = writeln!(f, "unreadable message from the box ({e}): {}", line.chars().take(300).collect::<String>());
                    }
                }
            },
            Err(e) => break e.into(),
        }
    };
    let _ = stop_tx.send(());
    let _ = child.kill();
    let _ = child.wait();
    err
}
