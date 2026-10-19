/*! agent-boxd: the a.box app's daemon, one on each host (the box, the Mac, any other in `hosts`).
`run` serves the app's protocol on a unix socket, `connect` relays stdio to it, `hook` logs Claude Code hooks. */

mod agents;
mod codex;
mod config;
mod db;
mod diff;
mod elsewhere;
mod hosts;
mod model;
mod server;
mod term;
mod titles;
mod usage;
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
