/*! agent-boxd: the box daemon for the Agent Box app.
`run` serves the app's protocol on a unix socket, `connect` relays stdio to it, `hook` logs Claude Code hooks. */

mod db;
mod diff;
mod model;
mod server;
mod term;
mod util;

fn main() -> anyhow::Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("run") => server::run(),
        Some("connect") => server::connect(),
        Some("hook") => {
            db::hook();
            Ok(())
        }
        _ => {
            eprintln!("usage: agent-boxd run | connect | hook");
            std::process::exit(2);
        }
    }
}
