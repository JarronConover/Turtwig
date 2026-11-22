// Redo agent tiles: chat with an agents.redo.builders agent from a tile in the grid.
//
// A Redo tile lives in `state.term_tiles` as `{id, kind: "redo", label, slug, agent_id, work}` so it
// shares the grid's slots, drag and focus list with the terminal tiles (grid id "term:<id>").
// `work` is the Work the tile shows; null until the first message starts one.
//
// Publishes `.data.redo_chat`: { <tile id>: { title, activity, settled, waiting, url, error, sending,
//   items: [{ k, role: user|agent|tools|question, who, text }] } }.
//
// Actions:
//   redo_chat.open  {slug, name, agent_id?, work?}  open a tile (an existing one for the same Work is reused)
//   redo_chat.send  {tile, text}                    start the Work, or send a follow-up / answer
//   redo_chat.stop  {tile}                          stop the running Work
//   redo_chat.close {tile}                          close the tile (the Work keeps going on the server)
//   redo_chat.refresh {tile?}

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

const BASE: &str = "https://agents.redo.builders";

thread_local! {
    static IN_FLIGHT: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    static LAST_FETCH: RefCell<HashMap<String, f64>> = RefCell::new(HashMap::new());
    static LAST_CURSOR: RefCell<HashMap<String, i64>> = RefCell::new(HashMap::new());
    // Messages sent but not yet in the Work's own list: shown greyed until they appear.
    static PENDING: RefCell<HashMap<String, Vec<String>>> = RefCell::new(HashMap::new());
    static SEQ: RefCell<u64> = RefCell::new(0);
    // When each tile last sent a message: keeps the spinner up until the agent's reply shows, even
    // while the server hasn't marked the Work running yet (otherwise it blinked off between polls).
    static SENT_AT: RefCell<HashMap<String, f64>> = RefCell::new(HashMap::new());
    // The latest full Work response per tile, with its events slimmed to what the view uses; live
    // stream events are appended to it, so polls and the stream render from one model with one set
    // of row keys (no re-keying, no jumps).
    static MODEL: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
    // The live event stream (curl on the Work's SSE endpoint) per tile, and which Work it follows.
    static STREAMS: RefCell<HashMap<String, (String, ab::Process)>> = RefCell::new(HashMap::new());
    static RETRY_AT: RefCell<HashMap<String, f64>> = RefCell::new(HashMap::new());
    // Tiles whose model changed since the last publish, and when each wants a full refetch.
    static DIRTY: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    static REFETCH_AT: RefCell<HashMap<String, f64>> = RefCell::new(HashMap::new());
    static TICKS: RefCell<u64> = RefCell::new(0);
}

// Event types the view draws; everything else is dropped from the model (tool outputs are large).
fn slim(e: &Value) -> Option<Value> {
    let ty = e["type"].as_str()?;
    let data = match ty {
        "message.appended" => json!({ "delta": e["data"]["delta"] }),
        "message.completed" => json!({ "text": e["data"]["text"] }),
        "actions.requested" => {
            let calls: Vec<Value> = e["data"]["calls"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|c| {
                            let args = &c["args"];
                            let pick = |k: &str| args[k].as_str().map(|s| clip(s.lines().next().unwrap_or(""), 120));
                            json!({ "toolName": c["toolName"], "args": {
                                "command": pick("command"), "query": pick("query"), "path": pick("path"), "url": pick("url")
                            } })
                        })
                        .collect()
                })
                .unwrap_or_default();
            json!({ "calls": calls })
        }
        _ => return None,
    };
    Some(json!({ "type": ty, "cursor": e["cursor"], "createdAt": e["createdAt"], "data": data }))
}

// Event types that only add to what's drawn; anything else (input accepted, turn or Work status,
// questions) may change the Work's messages or status, so it triggers a full refetch.
fn is_drawn_only(ty: &str) -> bool {
    matches!(
        ty,
        "message.appended" | "message.completed" | "actions.requested" | "actions.completed" | "action.result"
            | "reasoning.appended" | "reasoning.completed" | "artifact.tagged" | "native.binding.attested"
            | "step.started" | "delivery.updated" | "platform_action.recorded"
    )
}

fn model_cursor(m: &Value) -> i64 {
    let from_events = m["events"].as_array().and_then(|a| a.iter().filter_map(|e| e["cursor"].as_i64()).max()).unwrap_or(0);
    from_events.max(m["eventCursor"].as_i64().unwrap_or(0))
}

fn key() -> Option<String> {
    if let Ok(env) = ab::call_sync("env", json!({"name": "REDO_AGENTS_API_KEY"})) {
        let k = env.as_str().or(env.get("value").and_then(|v| v.as_str())).unwrap_or("").trim().to_string();
        if !k.is_empty() {
            return Some(k);
        }
    }
    ab::read("~/.config/redo-agents/api-key").map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn now() -> f64 {
    ab::context()["now"].as_f64().unwrap_or(0.0)
}

fn idem() -> String {
    let n = SEQ.with(|s| {
        *s.borrow_mut() += 1;
        *s.borrow()
    });
    format!("turtwig-{}-{}", (now() * 1000.0) as i64, n)
}

fn tiles() -> Vec<Value> {
    ab::context()["state"]["term_tiles"].as_array().cloned().unwrap_or_default()
}

fn redo_tiles() -> Vec<Value> {
    tiles().into_iter().filter(|t| t["kind"].as_str() == Some("redo")).collect()
}

fn tile(id: &str) -> Option<Value> {
    redo_tiles().into_iter().find(|t| t["id"].as_str() == Some(id))
}

fn set_tile_work(id: &str, work: &str) {
    let list: Vec<Value> = tiles()
        .into_iter()
        .map(|mut t| {
            if t["id"].as_str() == Some(id) {
                t["work"] = json!(work);
            }
            t
        })
        .collect();
    ab::set("term_tiles", Value::Array(list));
}

fn all_data() -> Value {
    let v = ab::context()["data"]["redo_chat"].clone();
    if v.is_object() { v } else { json!({}) }
}

fn update(id: &str, f: impl FnOnce(&mut Value)) {
    let mut all = all_data();
    let mut entry = all[id].clone();
    if !entry.is_object() {
        entry = json!({ "items": [] });
    }
    f(&mut entry);
    all[id] = entry;
    ab::data("redo_chat", all);
}

fn request(method: &str, path: &str, body: Option<Value>, f: impl FnOnce(Result<Value, String>) + 'static) {
    let Some(k) = key() else { return f(Err("No Redo agents API key".into())) };
    let mut headers = json!({ "Authorization": format!("Bearer {k}"), "Accept": "application/json" });
    if method != "GET" {
        headers["Content-Type"] = json!("application/json");
        headers["Idempotency-Key"] = json!(idem());
    }
    let mut req = json!({ "url": format!("{BASE}{path}"), "method": method, "headers": headers, "timeout_secs": 30 });
    if let Some(b) = body {
        req["body"] = json!(b.to_string());
    }
    ab::http(req, move |r: Value, err: Option<String>| {
        if let Some(e) = err {
            return f(Err(e));
        }
        let status = r["status"].as_i64().unwrap_or(0);
        let body = match r["body"].as_str() {
            Some(s) => s.parse::<Value>().unwrap_or(json!({ "raw": s })),
            None => r["body"].clone(),
        };
        match status {
            0 => f(Err("Can't reach agents.redo.builders (Tailscale on?)".into())),
            401 => f(Err("API key rejected".into())),
            403 => f(Err("Not allowed".into())),
            s if (200..300).contains(&s) => f(Ok(body)),
            s => {
                let msg = body["error"]["message"].as_str().or(body["message"].as_str()).unwrap_or("").to_string();
                f(Err(format!("HTTP {s} {msg}").trim().to_string()))
            }
        }
    });
}

// ISO time to seconds with fraction ("2026-11-11T20:40:44.617638+00:00" or "...Z").
fn ts(iso: &str) -> f64 {
    let b = iso.as_bytes();
    if b.len() < 19 {
        return 0.0;
    }
    let num = |s: &str| s.parse::<i64>().unwrap_or(0);
    let (y, m, d) = (num(&iso[0..4]), num(&iso[5..7]), num(&iso[8..10]));
    let (hh, mm, ss) = (num(&iso[11..13]), num(&iso[14..16]), num(&iso[17..19]));
    let (y2, m2) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let doy = (153 * m2 + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let mut frac = 0.0;
    if b.len() > 20 && b[19] == b'.' {
        let digits: String = iso[20..].chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            frac = format!("0.{digits}").parse::<f64>().unwrap_or(0.0);
        }
    }
    (days * 86400 + hh * 3600 + mm * 60 + ss) as f64 + frac
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(n).collect();
        t.push('…');
        t
    }
}

fn tool_label(call: &Value) -> String {
    let name = call["toolName"].as_str().unwrap_or("tool").trim_start_matches("redo__").replace('_', " ");
    let args = &call["args"];
    let detail = args["command"]
        .as_str()
        .or(args["query"].as_str())
        .or(args["path"].as_str())
        .or(args["url"].as_str())
        .unwrap_or("");
    let detail = detail.lines().next().unwrap_or("");
    if detail.is_empty() { name } else { format!("{name}: {}", clip(detail, 70)) }
}

// The conversation from a full Work response: people's messages, the agent's replies, and its tool
// use folded into one line per run of tools.
fn build(d: &Value, tile_id: &str) -> Value {
    let w = if d["work"].is_object() { &d["work"] } else { d };
    let empty = vec![];
    // (time, order, item)
    let mut rows: Vec<(f64, usize, Value)> = vec![];
    let mut order = 0usize;
    let mut user_texts: Vec<String> = vec![];
    for m in w["messages"].as_array().unwrap_or(&empty) {
        let role = m["role"].as_str().unwrap_or("");
        let text = m["content"].as_str().unwrap_or("").trim().to_string();
        if text.is_empty() {
            continue;
        }
        let t = ts(m["createdAt"].as_str().unwrap_or(""));
        if role == "user" {
            user_texts.push(text.clone());
            // Stable keys (the server's ids), so rows never re-key and jump between polls.
            rows.push((t, order, json!({ "k": format!("u:{}", m["id"].as_str().unwrap_or("")), "role": "user", "who": m["actorName"].as_str().unwrap_or("You"), "text": clip(&text, 6000) })));
        }
        order += 1;
    }
    let events = d["events"].as_array().unwrap_or(&empty);
    let mut has_completed = false;
    let mut live = String::new();
    let mut tools: Vec<String> = vec![];
    let mut tools_t = 0.0;
    let mut tools_k = 0i64;
    let mut live_k = 0i64;
    let mut live_t = 0.0;
    let flush = |tools: &mut Vec<String>, rows: &mut Vec<(f64, usize, Value)>, t: f64, k: i64, order: &mut usize| {
        if tools.is_empty() {
            return;
        }
        let n = tools.len();
        let text = if n == 1 { tools[0].clone() } else { format!("{n} tool calls · {}", tools.last().cloned().unwrap_or_default()) };
        rows.push((t, *order, json!({ "k": format!("t:{k}"), "role": "tools", "text": text, "all": tools.clone() })));
        *order += 1;
        tools.clear();
    };
    for e in events {
        let t = ts(e["createdAt"].as_str().unwrap_or(""));
        let cursor = e["cursor"].as_i64().unwrap_or(0);
        let data = &e["data"];
        match e["type"].as_str().unwrap_or("") {
            "actions.requested" => {
                if tools.is_empty() {
                    tools_t = t;
                    tools_k = cursor;
                }
                for c in data["calls"].as_array().unwrap_or(&empty) {
                    tools.push(tool_label(c));
                }
            }
            "message.appended" => {
                if live.is_empty() {
                    live_k = cursor;
                    live_t = t;
                }
                live.push_str(data["delta"].as_str().unwrap_or(""));
            }
            "message.completed" => {
                // A reply that streamed in keeps the key of its first chunk, so the live row simply
                // becomes the finished one (no pop).
                let k = if live_k > 0 { live_k } else { cursor };
                let start_t = if live_k > 0 { live_t } else { t };
                live.clear();
                live_k = 0;
                let text = data["text"].as_str().unwrap_or("").trim().to_string();
                if !text.is_empty() {
                    flush(&mut tools, &mut rows, tools_t, tools_k, &mut order);
                    has_completed = true;
                    rows.push((start_t, order, json!({ "k": format!("a:{k}"), "role": "agent", "text": clip(&text, 8000) })));
                    order += 1;
                }
            }
            _ => {}
        }
    }
    flush(&mut tools, &mut rows, tools_t, tools_k, &mut order);
    // Replies kept only as messages (no events in this window): use the Work's own assistant messages.
    if !has_completed {
        for m in w["messages"].as_array().unwrap_or(&empty) {
            let role = m["role"].as_str().unwrap_or("");
            let text = m["content"].as_str().unwrap_or("").trim().to_string();
            if role != "user" && !text.is_empty() {
                rows.push((ts(m["createdAt"].as_str().unwrap_or("")), order, json!({ "k": format!("m:{}", m["id"].as_str().unwrap_or("")), "role": "agent", "text": clip(&text, 8000) })));
                order += 1;
            }
        }
    }
    rows.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal).then(a.1.cmp(&b.1)));
    let mut items: Vec<Value> = rows.into_iter().map(|r| r.2).collect();
    if !live.trim().is_empty() {
        items.push(json!({ "k": format!("a:{live_k}"), "role": "agent", "text": clip(live.trim(), 8000), "live": true }));
    }
    // Questions the agent is waiting on.
    for (qi, o) in w["obligations"].as_array().unwrap_or(&empty).iter().enumerate() {
        let q = o["question"].as_str().or(o["prompt"].as_str()).or(o["text"].as_str()).or(o["data"]["question"].as_str()).unwrap_or("");
        if !q.is_empty() {
            let qk = o["id"].as_str().map(|s| s.to_string()).unwrap_or(qi.to_string());
            items.push(json!({ "k": format!("q:{qk}"), "role": "question", "text": clip(q, 4000) }));
        }
    }
    // Sent messages not echoed back yet.
    let pending: Vec<String> = PENDING.with(|p| {
        let mut p = p.borrow_mut();
        let list = p.entry(tile_id.to_string()).or_default();
        list.retain(|s| !user_texts.iter().any(|u| u.trim() == s.trim()));
        list.clone()
    });
    for (pi, s) in pending.into_iter().enumerate() {
        items.push(json!({ "k": format!("p:{pi}:{}", clip(&s, 24)), "role": "user", "who": "You", "text": s, "pending": true }));
    }
    let n = items.len();
    let items: Vec<Value> = items.into_iter().skip(n.saturating_sub(150)).collect();
    let activity = w["activity"].as_str().unwrap_or("").to_string();
    let waiting = w["obligations"].as_array().map(|a| !a.is_empty()).unwrap_or(false) || activity.contains("attention");
    let settled = !w["terminal"].is_null() || (activity != "running" && !waiting && activity != "queued");
    // Working on a reply: drives the "Thinking…" spinner under the conversation.
    let busy = !settled && !waiting;
    json!({
        "busy": busy,
        "title": w["title"].as_str().unwrap_or(""),
        "activity": activity,
        "waiting": waiting,
        "settled": settled,
        "paused": w["paused"].as_bool().unwrap_or(false),
        "messaging": w["messagingAvailable"].as_bool().unwrap_or(true),
        "url": format!("{BASE}/works/{}", w["id"].as_str().unwrap_or("")),
        "items": items
    })
}

fn fetch(tile_id: String) {
    let Some(t) = tile(&tile_id) else { return };
    let Some(work) = t["work"].as_str().map(|s| s.to_string()) else { return };
    if IN_FLIGHT.with(|f| !f.borrow_mut().insert(tile_id.clone())) {
        return;
    }
    LAST_FETCH.with(|l| l.borrow_mut().insert(tile_id.clone(), now()));
    let id = tile_id.clone();
    request("GET", &format!("/v2/works/{work}"), None, move |r| {
        IN_FLIGHT.with(|f| f.borrow_mut().remove(&id));
        match r {
            Ok(mut d) => {
                let cursor = d["eventCursor"].as_i64().or(d["work"]["eventCursor"].as_i64()).unwrap_or(0);
                LAST_CURSOR.with(|l| l.borrow_mut().insert(id.clone(), cursor));
                // Slim the events, then keep any streamed events newer than this response.
                let mut events: Vec<Value> = d["events"].as_array().map(|a| a.iter().filter_map(slim).collect()).unwrap_or_default();
                let got = events.iter().filter_map(|e| e["cursor"].as_i64()).max().unwrap_or(0).max(cursor);
                let newer: Vec<Value> = MODEL.with(|m| {
                    m.borrow()
                        .get(&id)
                        .and_then(|old| old["events"].as_array().cloned())
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|e| e["cursor"].as_i64().unwrap_or(0) > got)
                        .collect()
                });
                events.extend(newer);
                d["events"] = Value::Array(events);
                d["eventCursor"] = json!(got);
                MODEL.with(|m| m.borrow_mut().insert(id.clone(), d));
                rebuild(&id, true);
            }
            Err(err) => update(&id, move |e| e["error"] = json!(err)),
        }
    });
}

// Redraw a tile from its model; publishes only when something changed.
fn rebuild(id: &str, clear_error: bool) {
    let Some(d) = MODEL.with(|m| m.borrow().get(id).cloned()) else { return };
    let mut built = build(&d, id);
    let items = built["items"].as_array().cloned().unwrap_or_default();
    let last_role = items.last().and_then(|i| i["role"].as_str()).unwrap_or("").to_string();
    let streaming = items.last().map(|i| i["live"].as_bool().unwrap_or(false)).unwrap_or(false);
    // Sticky "working" right after a send: until a reply follows the latest user message (or 90 s
    // pass), the spinner stays; and always while a reply is streaming in.
    let sent = SENT_AT.with(|s| s.borrow().get(id).copied()).unwrap_or(0.0);
    if streaming || (now() - sent < 90.0 && (last_role == "user" || last_role == "tools") && !built["waiting"].as_bool().unwrap_or(false)) {
        built["busy"] = json!(true);
    } else if (last_role == "agent" || last_role == "question") && !streaming {
        SENT_AT.with(|s| s.borrow_mut().remove(id));
    }
    let cur = all_data()[id].clone();
    built["sending"] = json!(cur["sending"].as_bool().unwrap_or(false));
    built["error"] = if clear_error { Value::Null } else { cur["error"].clone() };
    if built != cur {
        let id = id.to_string();
        update(&id, move |e| *e = built);
    }
}

// Follow a tile's Work live over its SSE endpoint; events land in the model as they happen.
fn ensure_stream(id: &str, work: &str) {
    let running = STREAMS.with(|s| s.borrow().get(id).map(|(w, _)| w == work).unwrap_or(false));
    if running {
        return;
    }
    stop_stream(id);
    if now() < RETRY_AT.with(|r| r.borrow().get(id).copied().unwrap_or(0.0)) {
        return;
    }
    let Some(k) = key() else { return };
    let since = MODEL.with(|m| m.borrow().get(id).map(model_cursor)).unwrap_or(0);
    // The key goes in over stdin (curl -K -) so it never shows in the process list.
    let cmd = "exec curl -sN --keepalive-time 30 -K - -H 'Accept: text/event-stream' ${SINCE:+-H \"Last-Event-ID: $SINCE\"} \"https://agents.redo.builders/v2/works/$W/events\"";
    let since_s = if since > 0 { since.to_string() } else { String::new() };
    let tid = id.to_string();
    let wk = work.to_string();
    let stdin = format!("header = \"Authorization: Bearer {k}\"\n");
    let proc = ab::spawn(json!(cmd), json!({ "shell": true, "stdin": stdin, "env": { "W": wk, "SINCE": since_s } }), move |what: ab::Proc, data: Value| {
        match what {
            ab::Proc::Stdout => on_stream_line(&tid, data.as_str().unwrap_or("")),
            ab::Proc::Exit => {
                STREAMS.with(|s| s.borrow_mut().remove(&tid));
                // Reconnect in a couple of seconds (tick does it) and catch up on what was missed.
                RETRY_AT.with(|r| r.borrow_mut().insert(tid.clone(), now() + 2.0));
                REFETCH_AT.with(|r| r.borrow_mut().insert(tid.clone(), now() + 0.5));
            }
            _ => {}
        }
    });
    STREAMS.with(|s| s.borrow_mut().insert(id.to_string(), (work.to_string(), proc)));
}

fn stop_stream(id: &str) {
    if let Some((_, p)) = STREAMS.with(|s| s.borrow_mut().remove(id)) {
        p.kill();
    }
}

fn on_stream_line(id: &str, line: &str) {
    let Some(json) = line.strip_prefix("data:") else { return };
    let Ok(e) = json.trim().parse::<Value>() else { return };
    let ty = e["type"].as_str().unwrap_or("").to_string();
    let cursor = e["cursor"].as_i64().unwrap_or(0);
    if let Some(s) = slim(&e) {
        let added = MODEL.with(|m| {
            let mut m = m.borrow_mut();
            let Some(model) = m.get_mut(id) else { return false };
            if cursor <= model_cursor(model) {
                return false;
            }
            if let Some(a) = model["events"].as_array_mut() {
                a.push(s);
            }
            true
        });
        if added {
            DIRTY.with(|d| d.borrow_mut().insert(id.to_string()));
        }
    } else {
        MODEL.with(|m| {
            if let Some(model) = m.borrow_mut().get_mut(id) {
                if cursor > model["eventCursor"].as_i64().unwrap_or(0) {
                    model["eventCursor"] = json!(cursor);
                }
            }
        });
    }
    if !is_drawn_only(&ty) {
        // Messages or status may have changed: refetch the Work shortly (one fetch per burst).
        let when = now() + 1.2;
        REFETCH_AT.with(|r| {
            let mut r = r.borrow_mut();
            let e = r.entry(id.to_string()).or_insert(when);
            if *e > when {
                *e = when;
            }
        });
    }
}

// Every 120 ms: publish tiles whose model changed, run due refetches; about once a second keep
// streams up and reconcile with a full fetch now and then.
fn fast_tick() {
    let dirty: Vec<String> = DIRTY.with(|d| d.borrow_mut().drain().collect());
    for id in dirty {
        rebuild(&id, false);
    }
    let t = now();
    let due: Vec<String> = REFETCH_AT.with(|r| {
        let mut r = r.borrow_mut();
        let due: Vec<String> = r.iter().filter(|(_, w)| **w <= t).map(|(k, _)| k.clone()).collect();
        for k in &due {
            r.remove(k);
        }
        due
    });
    for id in due {
        fetch(id);
    }
    let n = TICKS.with(|c| {
        *c.borrow_mut() += 1;
        *c.borrow()
    });
    if n % 8 == 0 {
        tick();
    }
}

fn tick() {
    let t_now = now();
    let data = all_data();
    for t in redo_tiles() {
        let id = t["id"].as_str().unwrap_or("").to_string();
        if t["work"].as_str().is_none() {
            continue;
        }
        let work = t["work"].as_str().unwrap_or("").to_string();
        // Live updates come over the stream; a full fetch reconciles once a minute (or right away if
        // there's no model yet).
        let has_model = MODEL.with(|m| m.borrow().contains_key(&id));
        if has_model {
            ensure_stream(&id, &work);
        }
        let last = LAST_FETCH.with(|l| l.borrow().get(&id).copied().unwrap_or(0.0));
        if !has_model || t_now - last >= 60.0 {
            fetch(id);
        }
    }
    // Stop streams for closed tiles.
    let open_ids: Vec<String> = redo_tiles().iter().filter_map(|t| t["id"].as_str().map(|s| s.to_string())).collect();
    let stale: Vec<String> = STREAMS.with(|s| s.borrow().keys().filter(|k| !open_ids.contains(k)).cloned().collect());
    for id in stale {
        stop_stream(&id);
        MODEL.with(|m| m.borrow_mut().remove(&id));
    }
    // Drop data for closed tiles.
    let ids: Vec<String> = redo_tiles().iter().filter_map(|t| t["id"].as_str().map(|s| s.to_string())).collect();
    if let Some(obj) = data.as_object() {
        if obj.keys().any(|k| !ids.contains(k)) {
            let mut kept = json!({});
            for k in &ids {
                if let Some(v) = obj.get(k) {
                    kept[k.as_str()] = v.clone();
                }
            }
            ab::data("redo_chat", kept);
        }
    }
}

fn open(args: &Value) {
    let slug = args["slug"].as_str().unwrap_or("").to_string();
    let name = args["name"].as_str().unwrap_or(slug.as_str()).to_string();
    let work = args["work"].as_str().filter(|s| !s.is_empty()).map(|s| s.to_string());
    // Reuse a tile already showing this Work (or this agent's unstarted tile).
    if let Some(t) = redo_tiles().into_iter().find(|t| match &work {
        Some(w) => t["work"].as_str() == Some(w.as_str()),
        None => t["slug"].as_str() == Some(slug.as_str()) && t["work"].is_null(),
    }) {
        let id = t["id"].as_str().unwrap_or("").to_string();
        ab::set("tile_flash", json!(format!("term:{id}")));
        ab::after(900, || ab::set("tile_flash", Value::Null));
        return;
    }
    let id = format!("r{}", (now() * 1000.0) as i64 % 1_000_000_000);
    let mut list = tiles();
    list.push(json!({
        "id": id, "kind": "redo", "label": name, "slug": slug,
        "agent_id": args["agent_id"], "work": work, "cwd": ""
    }));
    ab::set("term_tiles", Value::Array(list));
    ab::run(json!("tiles.pin_all"));
    update(&id, |e| {
        e["items"] = json!([]);
        e["settled"] = json!(true);
    });
    if work.is_some() {
        fetch(id);
    }
}

fn send(args: &Value) {
    let id = args["tile"].as_str().unwrap_or("").to_string();
    let text = args["text"].as_str().unwrap_or("").trim().to_string();
    if text.is_empty() {
        return;
    }
    let Some(t) = tile(&id) else { return };
    SENT_AT.with(|s| s.borrow_mut().insert(id.clone(), now()));
    let pi = PENDING.with(|p| {
        let mut p = p.borrow_mut();
        let list = p.entry(id.clone()).or_default();
        list.push(text.clone());
        list.len() - 1
    });
    let shown = text.clone();
    update(&id, move |e| {
        e["sending"] = json!(true);
        e["busy"] = json!(true);
        e["error"] = Value::Null;
        let mut items = e["items"].as_array().cloned().unwrap_or_default();
        // Same key build() gives it, so the bubble stays put when the next poll confirms it.
        items.push(json!({ "k": format!("p:{pi}:{}", clip(&shown, 24)), "role": "user", "who": "You", "text": shown, "pending": true }));
        e["items"] = Value::Array(items);
    });
    let done_id = id.clone();
    let done = move |r: Result<Value, String>| {
        match r {
            Ok(_) => {
                update(&done_id, |e| e["sending"] = json!(false));
                // The stream shows the reply as it comes; one fetch confirms the sent message.
                let again = done_id.clone();
                ab::after(800, move || fetch(again));
            }
            Err(err) => update(&done_id, move |e| {
                e["sending"] = json!(false);
                e["error"] = json!(err);
            }),
        }
    };
    match t["work"].as_str() {
        Some(work) => {
            request("POST", &format!("/v2/works/{work}/messages"), Some(json!({ "content": text })), done);
        }
        None => {
            let slug = t["slug"].as_str().unwrap_or("").to_string();
            let title = clip(text.lines().next().unwrap_or(""), 80);
            let start_id = id.clone();
            request("POST", "/v2/works", Some(json!({ "content": text, "agentSlug": slug, "title": title })), move |r| {
                if let Ok(body) = &r {
                    let wid = body["id"].as_str().or(body["work"]["id"].as_str()).unwrap_or("").to_string();
                    if !wid.is_empty() {
                        set_tile_work(&start_id, &wid);
                    }
                }
                done(r);
            });
        }
    }
}

fn init() {
    ab::action("redo_chat.open", |args, _| open(&args));
    ab::action("redo_chat.send", |args, _| send(&args));
    ab::action("redo_chat.refresh", |args, _| {
        match args["tile"].as_str() {
            Some(id) => fetch(id.to_string()),
            None => {
                for t in redo_tiles() {
                    fetch(t["id"].as_str().unwrap_or("").to_string());
                }
            }
        }
    });
    ab::action("redo_chat.stop", |args, _| {
        let id = args["tile"].as_str().unwrap_or("").to_string();
        let Some(work) = tile(&id).and_then(|t| t["work"].as_str().map(|s| s.to_string())) else { return };
        request("POST", &format!("/v2/works/{work}/stop"), Some(json!({})), move |r| {
            if let Err(err) = r {
                update(&id, move |e| e["error"] = json!(err));
            } else {
                let again = id.clone();
                ab::after(1000, move || fetch(again));
            }
        });
    });
    ab::action("redo_chat.close", |args, _| {
        let id = args["tile"].as_str().unwrap_or("").to_string();
        let list: Vec<Value> = tiles().into_iter().filter(|t| t["id"].as_str() != Some(id.as_str())).collect();
        ab::set("term_tiles", Value::Array(list));
        let mut slots = ab::context()["state"]["tile_slots"].clone();
        if let Some(o) = slots.as_object_mut() {
            o.remove(&format!("term:{id}"));
        }
        ab::set("tile_slots", slots);
        if ab::context()["state"]["focus_term"].as_str() == Some(id.as_str()) {
            ab::set("focus_term", Value::Null);
        }
    });
    ab::on("start", |_, _| {
        for t in redo_tiles() {
            fetch(t["id"].as_str().unwrap_or("").to_string());
        }
        ab::every(120, || fast_tick());
    });
}
