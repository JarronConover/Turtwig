use agent_box_protocol::{encode, Event, ServerMessage};
use base64::Engine;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::sync::mpsc::Sender;

pub struct Term {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
}

/** A pty running `tmux attach` to `session`; output streams to `tx` as `TermOutput`. */
pub fn open(term_id: u64, session: &str, cols: u16, rows: u16, tx: Sender<String>) -> anyhow::Result<Term> {
    if session.starts_with("sh-") {
        crate::util::shell_options(session);
    }
    let pair = native_pty_system().openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;
    let mut cmd = CommandBuilder::new("tmux");
    cmd.args(["attach", "-t", &format!("={session}")]);
    cmd.env("TERM", "xterm-256color");
    cmd.env_remove("TMUX");
    let child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader()?;
    let writer = pair.master.take_writer()?;
    std::thread::spawn(move || {
        let mut buf = [0u8; 16384];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let data = base64::engine::general_purpose::STANDARD.encode(&buf[..n]);
                    if tx.send(encode(&ServerMessage::Event(Event::TermOutput { term_id, data }))).is_err() {
                        break;
                    }
                }
            }
        }
        let _ = tx.send(encode(&ServerMessage::Event(Event::TermExit { term_id })));
    });
    Ok(Term { master: pair.master, writer, child })
}

impl Term {
    pub fn input(&mut self, data: &str) -> std::io::Result<()> {
        self.writer.write_all(data.as_bytes())?;
        self.writer.flush()
    }

    pub fn resize(&self, cols: u16, rows: u16) -> anyhow::Result<()> {
        self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
    }

    pub fn close(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
