// Jarron Jr (the orchestrator): one special agent (a Claude chat named "Jarron Jr", state.orchestrator_chat) that
// manages the agents in the open tiles. Whenever a tile's agent finishes a turn or stops to wait, this
// script tells the orchestrator (batched, only while it's idle). The orchestrator answers with
// directives in an ```orchestrate block, which this script carries out:
//
//   {"to": "<chat id>", "say": "<message for that agent>"}   keep that agent going
//   {"user": "<chat id>", "why": "<what Jarron must decide>"} flag it for Jarron (notification + list)
//   {"done": "<chat id>", "why": "<short result>"}            resolved; leave it alone
//   {"none": "<chat id>"}                                     nothing to do for that tile
//
// Guard rails: at most MAX_ROUNDS messages to one agent before it's flagged for Jarron; never messages
// itself or the designer; pause with state.orchestrator_paused.
//
// Publishes .data.orchestrator: { status, flags: [{chat, name, why}], done: [{chat, name, why}],
//   rounds: {chat: n}, queued, last }.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};

const MAX_ROUNDS: i64 = 8;

thread_local! {
    // Tile updates waiting to be sent: chat id -> why ("finished" / "waiting").
    static QUEUE: RefCell<BTreeMap<String, String>> = RefCell::new(BTreeMap::new());
    // The latest non-user message text per chat (from chat.message).
    static LAST: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
    static PENDING_OPEN: RefCell<bool> = RefCell::new(false);
    // The running `keepawake30` (Prevent sleep), so a second click stops it.
    static AWAKE: RefCell<Option<ab::Process>> = RefCell::new(None);
}

// Prevent sleep: runs Jarron's own `keepawake30` (from ~/.zshrc) for 30 minutes; a click while it runs
// stops it. data.awake_until drives the chip's countdown.
fn keep_awake() {
    if let Some(p) = AWAKE.with(|a| a.borrow_mut().take()) {
        p.kill();
        ab::data("awake_until", Value::Null);
        return;
    }
    let until = ab::context()["now"].as_f64().unwrap_or(0.0) + 1800.0;
    ab::data("awake_until", json!(until));
    let p = ab::spawn(json!("exec /bin/zsh -c 'source ~/.zshrc >/dev/null 2>&1; keepawake30'"), json!({ "shell": true }), |what: ab::Proc, _data: Value| {
        if let ab::Proc::Exit = what {
            AWAKE.with(|a| *a.borrow_mut() = None);
            ab::data("awake_until", Value::Null);
        }
    });
    AWAKE.with(|a| *a.borrow_mut() = Some(p));
}

const PROMPT: &str = r#"You are Jarron Jr, Jarron's full-time agent and the orchestrator of his Turtwig workspace. Your job is to manage the other agents that have open tiles, so each one reaches a good stopping point: the issue is resolved, or it genuinely needs Jarron.

How this works:
- You will receive messages starting with "[tiles]" that describe agents whose turn just ended (or who are waiting on a permission prompt): their chat id, name, folder, state, and their latest message.
- For each agent, decide one thing and say it in an ```orchestrate block at the END of your reply, one JSON object per line:
  {"to": "<chat id>", "say": "<message to that agent>"}   push it forward: answer its question if the answer is clear from context, tell it to continue, to verify (run tests, typecheck), to fix what it reported broken, or to wrap up and summarize.
  {"user": "<chat id>", "why": "<one line: what Jarron must decide or do>"}   only when it truly needs Jarron: a product decision, credentials, a risky or destructive action, a permission prompt, or it is stuck after real attempts.
  {"done": "<chat id>", "why": "<one line result>"}   the work is finished and verified.
  {"none": "<chat id>"}   nothing useful to add right now. If a tile has nothing for you to do, do nothing with it.
- Do not do the agents' work yourself and do not edit files. You coordinate through messages only.
- Be brief and concrete in what you send agents. Never approve destructive actions (force pushes, deleting data, prod changes) on Jarron's behalf: flag them instead.
- Keep your reply outside the block to one or two lines of status.
- When Jarron says "good morning" (or "good morning turtwig"), a "[good morning]" brief with the tile data follows: do exactly what it asks and deliver the report as a Claude artifact.

Reply now with a one-line acknowledgement and an empty ```orchestrate block."#;

fn ctx() -> Value {
    ab::context()
}

fn orch_id() -> Option<String> {
    ctx()["state"]["orchestrator_chat"].as_str().filter(|s| !s.is_empty()).map(|s| s.to_string())
}

fn chat(id: &str) -> Option<Value> {
    ctx()["chats"].as_array()?.iter().find(|c| c["id"].as_str() == Some(id)).cloned()
}

fn data() -> Value {
    let d = ctx()["data"]["orchestrator"].clone();
    if d.is_object() { d } else { json!({ "status": "idle", "flags": [], "done": [], "rounds": {} }) }
}

fn publish(d: Value) {
    ab::data("orchestrator", d);
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { format!("{}…", s.chars().take(n).collect::<String>()) }
}

fn managed(c: &Value) -> bool {
    let id = c["id"].as_str().unwrap_or("");
    c["open"].as_bool().unwrap_or(false) && Some(id.to_string()) != orch_id() && c["kind"].as_str() != Some("designer")
}

fn enqueue(id: &str, why: &str) {
    QUEUE.with(|q| q.borrow_mut().insert(id.to_string(), why.to_string()));
    let mut d = data();
    d["queued"] = json!(QUEUE.with(|q| q.borrow().len()));
    publish(d);
}

fn last_text(c: &Value) -> String {
    let id = c["id"].as_str().unwrap_or("");
    LAST.with(|l| l.borrow().get(id).cloned())
        .or(c["last_message"].as_str().map(|s| s.to_string()))
        .unwrap_or_default()
}

// Send the queued tile updates to the orchestrator if it's idle.
fn flush() {
    if ctx()["state"]["orchestrator_paused"].as_bool() == Some(true) {
        return;
    }
    let Some(oid) = orch_id() else { return };
    let Some(o) = chat(&oid) else { return };
    if o["state"].as_str() == Some("working") || o["alive"].as_bool() == Some(false) {
        return;
    }
    let items: Vec<(String, String)> = QUEUE.with(|q| std::mem::take(&mut *q.borrow_mut()).into_iter().collect());
    if items.is_empty() {
        return;
    }
    let d = data();
    let mut lines = vec!["[tiles]".to_string()];
    for (id, why) in &items {
        let Some(c) = chat(id) else { continue };
        if !managed(&c) {
            continue;
        }
        let rounds = d["rounds"][id.as_str()].as_i64().unwrap_or(0);
        lines.push(format!(
            "\n## {} (chat id: {})\nfolder: {}\nstate: {}{}\nrounds you've sent it: {}/{}\nlatest message:\n{}",
            c["name"].as_str().unwrap_or("agent"),
            id,
            c["dir"].as_str().unwrap_or(""),
            if why == "waiting" { "waiting on a permission prompt (you can't approve it; flag Jarron if it matters)" } else { "turn finished" },
            if c["asks"].as_bool() == Some(true) { " (it asked a question)" } else { "" },
            rounds,
            MAX_ROUNDS,
            clip(last_text(&c).trim(), 3500)
        ));
    }
    if lines.len() == 1 {
        return;
    }
    let mut d = data();
    d["status"] = json!("thinking");
    d["queued"] = json!(0);
    publish(d);
    ab::run(json!({ "do": "chat.send", "chat": oid, "text": lines.join("\n") }));
}

// Carry out the directives in the orchestrator's latest reply.
fn act(text: &str) {
    let Some(start) = text.rfind("```orchestrate") else {
        let mut d = data();
        d["status"] = json!("idle");
        publish(d);
        return;
    };
    let body = &text[start + "```orchestrate".len()..];
    let body = body.split("```").next().unwrap_or("");
    let mut d = data();
    for line in body.lines() {
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        let Ok(v) = line.parse::<Value>() else { continue };
        if let (Some(to), Some(say)) = (v["to"].as_str(), v["say"].as_str()) {
            let Some(c) = chat(to) else { continue };
            if !managed(&c) {
                continue;
            }
            let n = d["rounds"][to].as_i64().unwrap_or(0) + 1;
            if n > MAX_ROUNDS {
                flag(&mut d, to, &format!("Orchestrator reached {MAX_ROUNDS} rounds without resolving it"));
                continue;
            }
            d["rounds"][to] = json!(n);
            unflag(&mut d, to);
            ab::run(json!({ "do": "chat.send", "chat": to, "text": say }));
        } else if let Some(to) = v["user"].as_str() {
            let why = v["why"].as_str().unwrap_or("Needs you");
            flag(&mut d, to, why);
        } else if let Some(to) = v["done"].as_str() {
            unflag(&mut d, to);
            let name = chat(to).and_then(|c| c["name"].as_str().map(|s| s.to_string())).unwrap_or_default();
            let mut done = d["done"].as_array().cloned().unwrap_or_default();
            done.retain(|x| x["chat"].as_str() != Some(to));
            done.push(json!({ "chat": to, "name": name, "why": v["why"].as_str().unwrap_or("Done") }));
            d["done"] = Value::Array(done);
            d["rounds"][to] = json!(0);
        }
    }
    d["status"] = json!("idle");
    d["last"] = json!(ctx()["now"]);
    publish(d);
    // Anything that came in meanwhile goes next.
    ab::after(1500, flush);
}

fn flag(d: &mut Value, id: &str, why: &str) {
    let name = chat(id).and_then(|c| c["name"].as_str().map(|s| s.to_string())).unwrap_or_else(|| "An agent".into());
    let mut flags = d["flags"].as_array().cloned().unwrap_or_default();
    flags.retain(|x| x["chat"].as_str() != Some(id));
    flags.push(json!({ "chat": id, "name": name, "why": why }));
    d["flags"] = Value::Array(flags);
    ab::run(json!({ "do": "notify.system", "chat": id, "title": format!("{name} needs you"), "text": why, "sound": true }));
}

fn unflag(d: &mut Value, id: &str) {
    let mut flags = d["flags"].as_array().cloned().unwrap_or_default();
    flags.retain(|x| x["chat"].as_str() != Some(id));
    d["flags"] = Value::Array(flags);
}

// "Good morning" (or "good morning turtwig") to the orchestrator: a morning report of every open tile,
// the Developer Platform Slack channels and Jarron's merge requests, delivered as a Claude artifact.
fn is_good_morning(text: &str) -> bool {
    let t = text.trim().to_lowercase();
    let t = t.trim_start_matches(|c: char| !c.is_alphanumeric());
    t.starts_with("good morning") || t.starts_with("gm turtwig") || t == "gm"
}

fn tile_snapshot() -> String {
    let ctx = ctx();
    let oid = orch_id().unwrap_or_default();
    let mut lines: Vec<String> = vec![];
    for c in ctx["chats"].as_array().cloned().unwrap_or_default() {
        if c["open"].as_bool() != Some(true) || c["id"].as_str() == Some(oid.as_str()) {
            continue;
        }
        let id = c["id"].as_str().unwrap_or("");
        let bg = &ctx["data"]["bg_wait"][id];
        let state = if c["alive"].as_bool() == Some(false) {
            "stopped".to_string()
        } else if !bg.is_null() && c["state"].as_str() == Some("idle") {
            format!("waiting on {}", bg["label"].as_str().unwrap_or("background work"))
        } else {
            c["state"].as_str().unwrap_or("?").to_string()
        };
        lines.push(format!(
            "- Agent \"{}\" ({}; {}{}) last said: {}",
            c["name"].as_str().unwrap_or("agent"),
            state,
            c["dir"].as_str().unwrap_or("").replace("/Users/jarron.conover/", "~/"),
            c["branch"].as_str().map(|b| format!(" @ {b}")).unwrap_or_default(),
            clip(last_text(&c).trim().replace('\n', " ").as_str(), 500)
        ));
    }
    for t in ctx["state"]["term_tiles"].as_array().cloned().unwrap_or_default() {
        let id = t["id"].as_str().unwrap_or("");
        match t["kind"].as_str().unwrap_or("") {
            "redo" => {
                let d = &ctx["data"]["redo_chat"][id];
                let last = d["items"].as_array().and_then(|a| a.iter().rev().find(|i| i["role"].as_str() == Some("agent"))).and_then(|i| i["text"].as_str()).unwrap_or("");
                lines.push(format!(
                    "- Redo agent \"{}\" (work: {}; {}) last said: {}",
                    t["label"].as_str().unwrap_or(""),
                    d["title"].as_str().unwrap_or("not started"),
                    if d["waiting"].as_bool() == Some(true) { "waiting for Jarron" } else if d["settled"].as_bool() == Some(true) { "done/idle" } else { "working" },
                    clip(last.replace('\n', " ").as_str(), 400)
                ));
            }
            "notes" => {
                let st = &ctx["state"];
                let title = st[format!("note_title:{id}").as_str()].as_str().unwrap_or("Notes");
                let open: Vec<String> = st[format!("notes:{id}").as_str()]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .filter(|i| !i["done"].as_bool().unwrap_or(false))
                    .filter_map(|i| st[format!("ni:{id}:{}", i["id"].as_str().unwrap_or("")).as_str()].as_str().map(|s| s.to_string()))
                    .filter(|s| !s.trim().is_empty())
                    .collect();
                lines.push(format!("- Notes \"{}\": open items: {}", if title.is_empty() { "Notes" } else { title }, if open.is_empty() { "none".into() } else { open.join("; ") }));
            }
            "slack" => {}
            _ => lines.push(format!("- Terminal \"{}\" in {}", t["label"].as_str().unwrap_or(""), t["cwd"].as_str().unwrap_or(""))),
        }
    }
    if lines.is_empty() { "(no tiles open)".into() } else { lines.join("\n") }
}

// The orchestrator's latest reply text, read from its transcript file (the last assistant turn).
fn read_last_reply(id: &str, f: impl FnOnce(String) + 'static) {
    let Some(path) = chat(id).and_then(|c| c["transcript"].as_str().map(|s| s.to_string())) else { return f(String::new()) };
    let cmd = r#"/usr/bin/python3 -I - "$T" <<'PY'
import json, sys
last = []
for line in open(sys.argv[1]):
    try: d = json.loads(line)
    except Exception: continue
    t = d.get("type")
    if t == "user" and not any(isinstance(c, dict) and c.get("type") == "tool_result" for c in ((d.get("message") or {}).get("content") or []) if isinstance(c, dict)):
        last = []
    if t == "assistant":
        for c in (d.get("message") or {}).get("content") or []:
            if isinstance(c, dict) and c.get("type") == "text":
                last.append(c["text"])
print("\n\n".join(last)[-20000:])
PY"#;
    ab::shell(cmd, json!({ "timeout": 20, "env": { "T": path } }), move |r, _| {
        f(r["stdout"].as_str().unwrap_or("").to_string());
    });
}

// Keep the latest Claude artifact link the orchestrator replied with (the morning report) for its bar.
fn remember_report(text: &str) {
    let Some(start) = text.rfind("https://claude.ai/artifact/") else { return };
    let url: String = text[start..].chars().take_while(|c| !c.is_whitespace() && *c != ')' && *c != ']').collect();
    let title = text[..start]
        .rfind('[')
        .and_then(|b| text[b + 1..start].split(']').next().map(|s| s.to_string()))
        .filter(|s| !s.is_empty() && s.len() < 80)
        .unwrap_or_else(|| "Latest report".into());
    let mut d = data();
    d["report"] = json!({ "url": url, "title": title, "at": ctx()["now"] });
    publish(d);
}

// The last 7 days as the app saw them: agent chats that were active, and Notes items checked off.
fn week_snapshot() -> String {
    let ctx = ctx();
    let now = ctx["now"].as_f64().unwrap_or(0.0);
    let since = now - 7.0 * 86400.0;
    let mut lines: Vec<String> = vec![];
    let mut chats: Vec<Value> = ctx["chats"].as_array().cloned().unwrap_or_default()
        .into_iter()
        .filter(|c| c["last_activity"].as_f64().unwrap_or(0.0) >= since && c["id"].as_str().map(|s| s.to_string()) != orch_id())
        .collect();
    chats.sort_by(|a, b| b["last_activity"].as_f64().unwrap_or(0.0).partial_cmp(&a["last_activity"].as_f64().unwrap_or(0.0)).unwrap_or(std::cmp::Ordering::Equal));
    for c in chats.iter().take(60) {
        lines.push(format!(
            "- Agent chat \"{}\" ({}{}; {}): {}",
            c["name"].as_str().unwrap_or("agent"),
            c["dir"].as_str().unwrap_or("").replace("/Users/jarron.conover/", "~/"),
            c["branch"].as_str().map(|b| format!(" @ {b}")).unwrap_or_default(),
            c["age_text"].as_str().unwrap_or(""),
            clip(last_text(c).trim().replace('\n', " ").as_str(), 300)
        ));
    }
    let st = &ctx["state"];
    for t in st["term_tiles"].as_array().cloned().unwrap_or_default() {
        if t["kind"].as_str() != Some("notes") {
            continue;
        }
        let id = t["id"].as_str().unwrap_or("");
        let done: Vec<String> = st[format!("notes:{id}").as_str()].as_array().cloned().unwrap_or_default().iter()
            .filter(|i| i["done"].as_bool() == Some(true) && i["done_at"].as_f64().unwrap_or(now) >= since)
            .filter_map(|i| st[format!("ni:{id}:{}", i["id"].as_str().unwrap_or("")).as_str()].as_str().map(|s| s.to_string()))
            .filter(|s| !s.trim().is_empty())
            .collect();
        if !done.is_empty() {
            lines.push(format!("- Notes items checked off: {}", done.join("; ")));
        }
    }
    if lines.is_empty() { "(nothing recorded in the app)".into() } else { lines.join("\n") }
}

fn week_review() {
    let Some(oid) = orch_id() else {
        ab::notify("Open Jarron Jr first (New tile → Jarron Jr).");
        return;
    };
    let now = ctx()["now"].as_f64().unwrap_or(0.0);
    let text = format!(
        "[week review] Jarron asked for a review of the last 7 days (since unix time {since:.0}, now {now:.0}). Gather what was done, then deliver it as a Claude artifact (a Claude Doc with your Claude Docs tools). In chat reply only with the link and one line. Leave the ```orchestrate block empty.\n\
\n\
Sources:\n\
1. GitLab: Jarron's MRs merged or opened in the window (group redo, and his other projects), MRs he reviewed or approved, and notable threads. Use titles and descriptions to understand impact.\n\
2. Redo agents (agents.redo.builders) work he started or that was about his areas, if your tools reach it.\n\
3. Slack: anything he shipped, announced or decided in the Developer Platform channels{pinned}.\n\
4. The app's own record below (agent chats active this week and Notes items he checked off).\n\
\n\
Report format: title \"Week in review\" with the date range. First \"The big three\": the 3 most important things done, one or two short sentences each, with links. Then \"Also done\": a tight bullet list of the other notable things (one line each, link where there is one). Optionally a last line \"Still open\" with what carries into next week. Plain words, no filler, under about 300 words.\n\
\n\
App record for the week:\n{week}",
        since = now - 7.0 * 86400.0,
        pinned = {
            let ch: Vec<String> = ctx()["settings"]["morning"]["slack_channels"].as_array().map(|a| a.iter().filter_map(|c| c.as_str().map(|s| format!("#{}", s.trim_start_matches('#')))).collect()).unwrap_or_default();
            if ch.is_empty() { String::new() } else { format!(" ({})", ch.join(", ")) }
        },
        week = week_snapshot()
    );
    let mut d = data();
    d["status"] = json!("week");
    publish(d);
    ab::run(json!({ "do": "chat.send", "chat": oid, "text": text }));
}

fn morning() {
    let Some(oid) = orch_id() else {
        ab::notify("Open Jarron Jr first (New tile → Jarron Jr).");
        return;
    };
    let channels: Vec<String> = ctx()["settings"]["morning"]["slack_channels"]
        .as_array()
        .map(|a| a.iter().filter_map(|c| c.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();
    let slack = if channels.is_empty() {
        "the 3 Developer Platform channels (find them with your Slack tools: channels named like devp-*, dev-platform* or developer-platform*; say which three you used so Jarron can pin them in settings morning.slack_channels)".to_string()
    } else {
        channels.iter().map(|c| format!("#{}", c.trim_start_matches('#'))).collect::<Vec<_>>().join(", ")
    };
    let date = ctx()["now"].as_f64().unwrap_or(0.0);
    let text = format!(
        "[good morning] Jarron asked for the morning report. Do these three things, then deliver the report as a Claude artifact (create a Claude Doc with your Claude Docs tools; if you have none, write it as a markdown artifact). In chat reply only with the link and one line. Leave the ```orchestrate block empty this time.\n\
\n\
1. Tiles: one line per tile below: status, what it's doing, and whether it needs Jarron. Put anything that needs him first.\n\
2. Slack: read the last 24 hours in {slack}. Per channel, up to 4 bullets: incidents or alerts, questions or asks aimed at Jarron, decisions. Skip noise; say \"quiet\" if nothing matters.\n\
3. Merge requests (GitLab tools, project redo/redoapp/product/redo and Jarron's other projects): his open and draft MRs, and MRs where he's a reviewer. For each: link, title, state (draft / ready / approved), pipeline, unresolved threads, and the one next step. Flag ones that need his action.\n\
\n\
Report format: title \"Good morning, Jarron\" with today's date (unix time {date:.0}), a 3-line \"Today\" summary at the top, then sections Tiles, Slack, Merge requests. Tables where they fit, short plain sentences, under about 350 words.\n\
\n\
Open tiles right now:\n{tiles}",
        tiles = tile_snapshot()
    );
    let mut d = data();
    d["status"] = json!("morning");
    publish(d);
    ab::run(json!({ "do": "chat.send", "chat": oid, "text": text }));
}

fn init() {
    ab::action("orchestrator.morning", |_, _| morning());
    ab::action("orchestrator.week", |_, _| week_review());
    ab::action("jr.keepawake", |_, _| keep_awake());
    // A message typed into the orchestrator's own prompt box.
    ab::action("orchestrator.ask", |args, _| {
        let text = args["text"].as_str().unwrap_or("").trim().to_string();
        if text.is_empty() {
            return;
        }
        if is_good_morning(&text) {
            return morning();
        }
        let Some(oid) = orch_id() else { return };
        let mut d = data();
        d["status"] = json!("thinking");
        d["asked"] = json!(text);
        publish(d);
        ab::run(json!({ "do": "chat.send", "chat": oid, "text": text }));
    });
    ab::action("orchestrator.open", |_, _| {
        if let Some(id) = orch_id() {
            if chat(&id).map(|c| c["open"].as_bool().unwrap_or(false)).unwrap_or(false) {
                ab::run(json!({ "do": "chat.show", "chat": id }));
                return;
            }
        }
        PENDING_OPEN.with(|p| *p.borrow_mut() = true);
        ab::run(json!({ "do": "tiles.pending", "label": "Starting Jarron Jr…" }));
        let dir = ctx()["worktrees"]
            .as_array()
            .and_then(|w| w.iter().find(|w| w["repo"].as_str() == Some("redo") && w["is_primary"].as_bool() == Some(true)))
            .and_then(|w| w["path"].as_str().map(|s| s.to_string()))
            .unwrap_or("/Users/jarron.conover/redo".into());
        ab::run(json!({ "do": "chat.new", "dir": dir, "name": "Jarron Jr", "model": "claude-opus-5-5", "prompt": PROMPT, "select": false }));
    });
    ab::action("orchestrator.pause", |_, _| {
        let p = ctx()["state"]["orchestrator_paused"].as_bool().unwrap_or(false);
        ab::set("orchestrator_paused", json!(!p));
        if p {
            ab::after(300, flush);
        }
    });
    // Ask it to look at every open tile now.
    ab::action("orchestrator.sweep", |_, _| {
        for c in ctx()["chats"].as_array().cloned().unwrap_or_default() {
            if managed(&c) && c["state"].as_str() != Some("working") {
                enqueue(c["id"].as_str().unwrap_or(""), if c["state"].as_str() == Some("waiting") { "waiting" } else { "finished" });
            }
        }
        flush();
    });
    ab::action("orchestrator.dismiss", |args, _| {
        let id = args["chat"].as_str().unwrap_or("").to_string();
        let mut d = data();
        unflag(&mut d, &id);
        let mut done = d["done"].as_array().cloned().unwrap_or_default();
        done.retain(|x| x["chat"].as_str() != Some(id.as_str()));
        d["done"] = Value::Array(done);
        publish(d);
    });
    ab::on("chat.added", |p, _| {
        let c = &p["chat"];
        if PENDING_OPEN.with(|x| *x.borrow()) && c["name"].as_str() == Some("Jarron Jr") {
            PENDING_OPEN.with(|x| *x.borrow_mut() = false);
            ab::set("orchestrator_chat", json!(c["id"]));
            publish(json!({ "status": "starting", "flags": [], "done": [], "rounds": {} }));
            ab::run(json!("tiles.pin_all"));
        }
    });
    ab::on("chat.message", |p, _| {
        let kind = p["message"]["kind"].as_str().unwrap_or("");
        if kind == "user" {
            // "Good morning" typed to the orchestrator: send it the morning brief with the tile data.
            let text = p["message"]["text"].as_str().unwrap_or("");
            if p["chat"]["id"].as_str().map(|s| s.to_string()) == orch_id() && is_good_morning(text) && !text.starts_with("[good morning]") {
                ab::after(400, morning);
            }
            return;
        }
        if let (Some(id), Some(t)) = (p["chat"]["id"].as_str(), p["message"]["text"].as_str()) {
            if !t.trim().is_empty() {
                LAST.with(|l| l.borrow_mut().insert(id.to_string(), t.to_string()));
            }
        }
    });
    ab::on("chat.finished", |p, _| {
        let id = p["chat"]["id"].as_str().unwrap_or("").to_string();
        if Some(id.clone()) == orch_id() {
            let first = data()["status"].as_str() == Some("starting");
            let known = LAST.with(|l| l.borrow().get(&id).cloned()).or(p["chat"]["last_message"].as_str().map(|s| s.to_string()));
            let after_reply = move |text: String| {
                remember_report(&text);
                // Its reply (without the directive block) for the bar, since SDK chats don't show yet.
                let shown = match text.rfind("```orchestrate") { Some(i) => text[..i].trim().to_string(), None => text.trim().to_string() };
                let mut d = data();
                d["reply"] = json!(clip(&shown, 4000));
                d["reply_at"] = ctx()["now"].clone();
                publish(d);
                act(&text);
                if first {
                    ab::after(800, || ab::run(json!("orchestrator.sweep")));
                }
            };
            match known {
                Some(t) if !t.trim().is_empty() => after_reply(t),
                // SDK chats don't report their messages to scripts yet: read the reply from the transcript.
                _ => read_last_reply(&id, after_reply),
            }
            return;
        }
        if orch_id().is_some() && chat(&id).map(|c| managed(&c)).unwrap_or(false) {
            enqueue(&id, "finished");
            ab::after(4000, flush);
        }
    });
    ab::on("chat.waiting", |p, _| {
        let id = p["chat"]["id"].as_str().unwrap_or("").to_string();
        if orch_id().is_some() && Some(id.clone()) != orch_id() && chat(&id).map(|c| managed(&c)).unwrap_or(false) {
            enqueue(&id, "waiting");
            ab::after(4000, flush);
        }
    });
    ab::on("start", |_, _| {
        ab::every(15_000, flush);
    });
}
