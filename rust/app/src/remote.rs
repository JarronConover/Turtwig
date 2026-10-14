/*! The connection to `agent-boxd` on the box: `ssh <host> <daemon> connect`, JSON lines both ways,
reconnecting forever. Responses resolve the matching call; everything else arrives as `Incoming`. */

use agent_box_protocol::{encode, Call, Event, Request, ServerMessage};
use anyhow::{anyhow, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub enum Incoming {
    Connected,
    Disconnected(String),
    Event(Event),
}

type Pending = Arc<Mutex<HashMap<u64, flume::Sender<Result<Value>>>>>;

#[derive(Clone)]
pub struct Remote {
    out: flume::Sender<String>,
    pending: Pending,
    next_id: Arc<AtomicU64>,
}

impl Remote {
    pub fn start(ssh_host: String, daemon: String, retry: Duration) -> (Remote, flume::Receiver<Incoming>) {
        let (out_tx, out_rx) = flume::unbounded::<String>();
        let (in_tx, in_rx) = flume::unbounded::<Incoming>();
        let pending: Pending = Arc::default();
        let remote = Remote { out: out_tx, pending: pending.clone(), next_id: Arc::new(AtomicU64::new(1)) };
        std::thread::spawn(move || loop {
            let err = session(&ssh_host, &daemon, &out_rx, &in_tx, &pending);
            for (_, waiter) in pending.lock().unwrap().drain() {
                let _ = waiter.send(Err(anyhow!("disconnected")));
            }
            let _ = in_tx.send(Incoming::Disconnected(err.to_string()));
            std::thread::sleep(retry);
        });
        (remote, in_rx)
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
    host: &str,
    daemon: &str,
    out_rx: &flume::Receiver<String>,
    in_tx: &flume::Sender<Incoming>,
    pending: &Pending,
) -> anyhow::Error {
    let child = Command::new("/usr/bin/ssh")
        .args(["-T", "-o", "ServerAliveInterval=15", "-o", "ServerAliveCountMax=3", host, &format!("{daemon} connect")])
        .env("PATH", "/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => return e.into(),
    };
    let mut stdin = child.stdin.take().unwrap();
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
                Err(_) => {}
            },
            Err(e) => break e.into(),
        }
    };
    let _ = stop_tx.send(());
    let _ = child.kill();
    let _ = child.wait();
    err
}
