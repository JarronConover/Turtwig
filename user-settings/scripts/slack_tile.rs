// Slack tile: recent messages from the Slack channels you pick.
//
// Token: SLACK_TOKEN in the app's environment, or the file ~/.config/slack/token (a Slack user token,
// xoxp-…, with channels:read, groups:read, channels:history, groups:history, users:read).
// Picked channels: state.slack_channels [{id, name}] (kept across restarts).
//
// Publishes .data.slack: { ok, no_token, error, loading, updated,
//   all: [{id, name, private}] (for the picker), feed: { <channel id>: [{ts, who, text, time}] } }
//
// Actions: slack.refresh, slack.add {id, name}, slack.remove {id}, slack.load_channels

use std::cell::RefCell;
use std::collections::HashMap;

const EVERY_MS: u64 = 60_000;

thread_local! {
    static USERS: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
}

fn token() -> Option<String> {
    if let Ok(env) = ab::call_sync("env", json!({"name": "SLACK_TOKEN"})) {
        let t = env.as_str().or(env.get("value").and_then(|v| v.as_str())).unwrap_or("").trim().to_string();
        if !t.is_empty() {
            return Some(t);
        }
    }
    ab::read("~/.config/slack/token").map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn data() -> Value {
    let d = ab::context()["data"]["slack"].clone();
    if d.is_object() { d } else { json!({ "feed": {}, "all": [] }) }
}

fn set(f: impl FnOnce(&mut Value)) {
    let mut d = data();
    f(&mut d);
    ab::data("slack", d);
}

fn picked() -> Vec<Value> {
    ab::context()["state"]["slack_channels"].as_array().cloned().unwrap_or_default()
}

fn api(method: &str, query: &str, f: impl FnOnce(Result<Value, String>) + 'static) {
    let Some(t) = token() else { return f(Err("no_token".into())) };
    ab::http(
        json!({
            "url": format!("https://slack.com/api/{method}?{query}"),
            "method": "GET",
            "headers": { "Authorization": format!("Bearer {t}") },
            "timeout_secs": 20
        }),
        move |r: Value, err: Option<String>| {
            if let Some(e) = err {
                return f(Err(e));
            }
            let body = match r["body"].as_str() {
                Some(s) => s.parse::<Value>().unwrap_or(Value::Null),
                None => r["body"].clone(),
            };
            if body["ok"].as_bool() == Some(true) {
                f(Ok(body))
            } else {
                f(Err(body["error"].as_str().unwrap_or("request failed").to_string()))
            }
        },
    );
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { format!("{}…", s.chars().take(n).collect::<String>()) }
}

// Slack markup to plain-ish text: <@U123> → @name, <#C1|name> → #name, <url|label> → label.
fn tidy(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('>') else { out.push_str(&rest[start..]); return out };
        let inner = &rest[start + 1..start + end];
        let shown = if let Some(u) = inner.strip_prefix('@') {
            let id = u.split('|').next().unwrap_or(u);
            format!("@{}", USERS.with(|m| m.borrow().get(id).cloned()).unwrap_or_else(|| id.to_string()))
        } else if let Some(c) = inner.strip_prefix('#') {
            format!("#{}", c.split('|').nth(1).unwrap_or(c))
        } else if let Some(sp) = inner.strip_prefix('!') {
            format!("@{}", sp.split('|').next().unwrap_or(sp))
        } else {
            inner.split('|').nth(1).unwrap_or(inner).to_string()
        };
        out.push_str(&shown);
        rest = &rest[start + end + 1..];
    }
    out.push_str(rest);
    out.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">")
}

fn ensure_user(id: &str) {
    if id.is_empty() || USERS.with(|m| m.borrow().contains_key(id)) {
        return;
    }
    USERS.with(|m| m.borrow_mut().insert(id.to_string(), id.to_string()));
    let uid = id.to_string();
    api("users.info", &format!("user={id}"), move |r| {
        if let Ok(b) = r {
            let u = &b["user"];
            let name = u["profile"]["display_name"].as_str().filter(|s| !s.is_empty())
                .or(u["real_name"].as_str())
                .or(u["name"].as_str())
                .unwrap_or(uid.as_str())
                .to_string();
            USERS.with(|m| m.borrow_mut().insert(uid.clone(), name));
            ab::after(300, refresh_names);
        }
    });
}

// Re-render names once users.info answers.
fn refresh_names() {
    set(|d| {
        if let Some(feed) = d["feed"].as_object_mut() {
            for (_, msgs) in feed.iter_mut() {
                if let Some(a) = msgs.as_array_mut() {
                    for m in a.iter_mut() {
                        let uid = m["uid"].as_str().unwrap_or("").to_string();
                        if let Some(n) = USERS.with(|u| u.borrow().get(&uid).cloned()) {
                            m["who"] = json!(n);
                        }
                        let raw = m["raw"].as_str().unwrap_or("").to_string();
                        m["text"] = json!(clip(&tidy(&raw), 400));
                    }
                }
            }
        }
    });
}

fn load_history(id: String) {
    let cid = id.clone();
    api("conversations.history", &format!("channel={id}&limit=8"), move |r| {
        match r {
            Ok(b) => {
                let msgs: Vec<Value> = b["messages"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|m| m["subtype"].as_str().map(|s| s == "bot_message" || s == "thread_broadcast").unwrap_or(true))
                    .take(6)
                    .map(|m| {
                        let uid = m["user"].as_str().unwrap_or("").to_string();
                        ensure_user(&uid);
                        let who = m["user_profile"]["display_name"].as_str().filter(|s| !s.is_empty())
                            .or(m["user_profile"]["real_name"].as_str())
                            .map(|s| s.to_string())
                            .or(USERS.with(|u| u.borrow().get(&uid).cloned()))
                            .or(m["username"].as_str().map(|s| s.to_string()))
                            .or(m["bot_profile"]["name"].as_str().map(|s| s.to_string()))
                            .unwrap_or_else(|| "someone".into());
                        let raw = m["text"].as_str().unwrap_or("").to_string();
                        let ts = m["ts"].as_str().unwrap_or("0").to_string();
                        json!({
                            "ts": ts, "time": ts.parse::<f64>().unwrap_or(0.0), "uid": uid, "who": who,
                            "raw": raw, "text": clip(&tidy(&raw), 400),
                            "replies": m["reply_count"].as_i64().unwrap_or(0)
                        })
                    })
                    .collect();
                set(move |d| {
                    d["feed"][cid.as_str()] = Value::Array(msgs);
                    d["ok"] = json!(true);
                    d["error"] = Value::Null;
                    d["updated"] = ab::context()["now"].clone();
                });
            }
            Err(e) => set(move |d| {
                if e == "no_token" {
                    d["no_token"] = json!(true);
                } else {
                    d["error"] = json!(format!("#{cid}: {e}"));
                }
            }),
        }
    });
}

fn refresh() {
    if token().is_none() {
        set(|d| d["no_token"] = json!(true));
        return;
    }
    set(|d| d["no_token"] = json!(false));
    for c in picked() {
        if let Some(id) = c["id"].as_str() {
            load_history(id.to_string());
        }
    }
}

// The channel list for the picker (public and private you're in), up to 5 pages.
fn load_channels(cursor: String, mut acc: Vec<Value>, page: u32) {
    let q = format!("types=public_channel,private_channel&exclude_archived=true&limit=1000{}", if cursor.is_empty() { String::new() } else { format!("&cursor={cursor}") });
    set(|d| d["loading"] = json!(true));
    api("conversations.list", &q, move |r| match r {
        Ok(b) => {
            for c in b["channels"].as_array().cloned().unwrap_or_default() {
                acc.push(json!({ "id": c["id"], "name": c["name"], "private": c["is_private"], "member": c["is_member"] }));
            }
            let next = b["response_metadata"]["next_cursor"].as_str().unwrap_or("").to_string();
            if !next.is_empty() && page < 5 {
                load_channels(next, acc, page + 1);
            } else {
                acc.sort_by(|a, b| a["name"].as_str().unwrap_or("").cmp(b["name"].as_str().unwrap_or("")));
                set(move |d| {
                    d["all"] = Value::Array(acc);
                    d["loading"] = json!(false);
                });
            }
        }
        Err(e) => set(move |d| {
            d["loading"] = json!(false);
            if e == "no_token" { d["no_token"] = json!(true) } else { d["error"] = json!(e) }
        }),
    });
}

fn init() {
    ab::action("slack.refresh", |_, _| refresh());
    ab::action("slack.load_channels", |_, _| load_channels(String::new(), vec![], 0));
    ab::action("slack.add", |args, _| {
        let id = args["id"].as_str().unwrap_or("").to_string();
        if id.is_empty() {
            return;
        }
        let mut list = picked();
        if !list.iter().any(|c| c["id"].as_str() == Some(id.as_str())) {
            list.push(json!({ "id": id, "name": args["name"] }));
            ab::set("slack_channels", Value::Array(list));
        }
        load_history(id);
    });
    ab::action("slack.remove", |args, _| {
        let id = args["id"].as_str().unwrap_or("").to_string();
        let list: Vec<Value> = picked().into_iter().filter(|c| c["id"].as_str() != Some(id.as_str())).collect();
        ab::set("slack_channels", Value::Array(list));
        set(move |d| {
            if let Some(f) = d["feed"].as_object_mut() {
                f.remove(&id);
            }
        });
    });
    ab::action("slack.new_tile", |_, _| {
        let tiles = ab::context()["state"]["term_tiles"].as_array().cloned().unwrap_or_default();
        if tiles.iter().any(|t| t["kind"].as_str() == Some("slack")) {
            return;
        }
        let mut list = tiles;
        list.push(json!({ "id": "slack", "kind": "slack", "label": "Slack", "cwd": "" }));
        ab::set("term_tiles", Value::Array(list));
        ab::run(json!("tiles.pin_all"));
        refresh();
    });
    ab::action("slack.close", |_, _| {
        let list: Vec<Value> = ab::context()["state"]["term_tiles"].as_array().cloned().unwrap_or_default()
            .into_iter().filter(|t| t["kind"].as_str() != Some("slack")).collect();
        ab::set("term_tiles", Value::Array(list));
        let mut slots = ab::context()["state"]["tile_slots"].clone();
        if let Some(o) = slots.as_object_mut() {
            o.remove("term:slack");
        }
        ab::set("tile_slots", slots);
    });
    ab::on("start", |_, _| {
        let has_tile = ab::context()["state"]["term_tiles"].as_array().map(|a| a.iter().any(|t| t["kind"].as_str() == Some("slack"))).unwrap_or(false);
        if has_tile {
            refresh();
        }
        ab::every(EVERY_MS, || {
            let has_tile = ab::context()["state"]["term_tiles"].as_array().map(|a| a.iter().any(|t| t["kind"].as_str() == Some("slack"))).unwrap_or(false);
            if has_tile {
                refresh();
            }
        });
    });
}
