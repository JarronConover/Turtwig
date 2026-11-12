// Orchestrator: one special agent (a Claude chat named "Orchestrator", state.orchestrator_chat) that
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
}

const PROMPT: &str = r#"You are the Orchestrator for Jarron's Turtwig workspace. Your job is to manage the other agents that have open tiles, so each one reaches a good stopping point: the issue is resolved, or it genuinely needs Jarron.

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

fn init() {
    ab::action("orchestrator.open", |_, _| {
        if let Some(id) = orch_id() {
            if chat(&id).map(|c| c["open"].as_bool().unwrap_or(false)).unwrap_or(false) {
                ab::run(json!({ "do": "chat.show", "chat": id }));
                return;
            }
        }
        PENDING_OPEN.with(|p| *p.borrow_mut() = true);
        let dir = ctx()["worktrees"]
            .as_array()
            .and_then(|w| w.iter().find(|w| w["repo"].as_str() == Some("redo") && w["is_primary"].as_bool() == Some(true)))
            .and_then(|w| w["path"].as_str().map(|s| s.to_string()))
            .unwrap_or("/Users/jarron.conover/redo".into());
        ab::run(json!({ "do": "chat.new", "dir": dir, "name": "Orchestrator", "model": "claude-opus-5-5", "prompt": PROMPT, "select": false }));
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
        if PENDING_OPEN.with(|x| *x.borrow()) && c["name"].as_str() == Some("Orchestrator") {
            PENDING_OPEN.with(|x| *x.borrow_mut() = false);
            ab::set("orchestrator_chat", json!(c["id"]));
            publish(json!({ "status": "starting", "flags": [], "done": [], "rounds": {} }));
            ab::run(json!("tiles.pin_all"));
        }
    });
    ab::on("chat.message", |p, _| {
        let kind = p["message"]["kind"].as_str().unwrap_or("");
        if kind == "user" {
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
            let text = LAST.with(|l| l.borrow().get(&id).cloned()).or(p["chat"]["last_message"].as_str().map(|s| s.to_string())).unwrap_or_default();
            let first = data()["status"].as_str() == Some("starting");
            act(&text);
            if first {
                ab::after(800, || ab::run(json!("orchestrator.sweep")));
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
