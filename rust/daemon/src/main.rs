/*! agent-boxd: the Turtwig app's daemon, one on each host (the box, the Mac, any other in `hosts`).
`run` serves the app's protocol on a unix socket, `connect` relays stdio to it, `hook` logs Claude Code hooks, `inbox` posts to and reads this host's inbox, `web` serves the phone app. */

mod agents;
mod codex;
mod config;
mod db;
mod diff;
mod elsewhere;
mod hosts;
mod model;
mod sdk;
mod server;
mod term;
mod titles;
mod usage;
mod util;
mod web;

fn main() -> anyhow::Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("run") => server::run(),
        Some("connect") => server::connect(std::env::args().any(|a| a == "--zlib")),
        Some("hook") => {
            db::hook();
            Ok(())
        }
        Some("web") => web::run(),
        Some("inbox") => std::process::exit(db::inbox_cli(&std::env::args().skip(2).collect::<Vec<_>>())),
        _ => {
            eprintln!("usage: agent-boxd run | connect | hook | inbox | web");
            std::process::exit(2);
        }
    }
}
