// Datadog monitors pinned in the sidebar (settings `datadog.monitors`: monitor ids).
//
// Publishes `.data.datadog`: { ok, no_key, error, loading, updated,
//   monitors: [{ id, name, state, url, tags }] }   state: OK | Alert | Warn | No Data | Ignored | Unknown
//
// Keys: DD_API_KEY and DD_APP_KEY in ~/.config/datadog/keys.env (KEY=value lines) or the environment.
// Site: settings `datadog.site` (default datadoghq.com).
// Actions: datadog.refresh, datadog.add {text: a monitor URL or id}, datadog.remove {id}.
// Refreshes every `datadog.refresh_minutes` (default 5) and on demand; the last good result stays shown.

fn cfg() -> Value {
    ab::context()["settings"]["datadog"].clone()
}

fn site() -> String {
    cfg()["site"].as_str().unwrap_or("datadoghq.com").to_string()
}

fn ids() -> Vec<String> {
    cfg()["monitors"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string()).or(v.as_i64().map(|n| n.to_string()))).collect())
        .unwrap_or_default()
}

fn keys() -> Option<(String, String)> {
    let mut api = String::new();
    let mut app = String::new();
    for name in ["DD_API_KEY", "DD_APP_KEY", "DD_APPLICATION_KEY"] {
        if let Ok(v) = ab::call_sync("env", json!({"name": name})) {
            let s = v.as_str().or(v.get("value").and_then(|x| x.as_str())).unwrap_or("").trim().to_string();
            if !s.is_empty() {
                if name == "DD_API_KEY" { api = s } else { app = s }
            }
        }
    }
    if let Some(text) = ab::read("~/.config/datadog/keys.env") {
        for line in text.lines() {
            let line = line.trim().trim_start_matches("export ");
            if let Some((k, v)) = line.split_once('=') {
                let v = v.trim().trim_matches('"').trim_matches('\'').to_string();
                match k.trim() {
                    "DD_API_KEY" if api.is_empty() => api = v,
                    "DD_APP_KEY" | "DD_APPLICATION_KEY" if app.is_empty() => app = v,
                    _ => {}
                }
            }
        }
    }
    if api.is_empty() || app.is_empty() { None } else { Some((api, app)) }
}

fn current() -> Value {
    let v = ab::context()["data"]["datadog"].clone();
    if v.is_object() { v } else { json!({ "monitors": [] }) }
}

// Rows for every configured id, from the last fetched details where known.
fn rows(details: &Value) -> Vec<Value> {
    let base = format!("https://app.{}", site());
    ids()
        .into_iter()
        .map(|id| {
            let d = details[id.as_str()].clone();
            json!({
                "id": id,
                "name": d["name"].as_str().unwrap_or(&format!("Monitor {id}")),
                "state": d["state"].as_str().unwrap_or("Unknown"),
                "tags": d["tags"],
                "url": format!("{base}/monitors/{id}")
            })
        })
        .collect()
}

fn details() -> Value {
    let d = ab::context()["data"]["datadog_details"].clone();
    if d.is_object() { d } else { json!({}) }
}

fn publish(extra: Value) {
    let mut v = current();
    v["monitors"] = Value::Array(rows(&details()));
    if let Some(o) = extra.as_object() {
        for (k, x) in o {
            v[k.as_str()] = x.clone();
        }
    }
    ab::data("datadog", v);
}

fn refresh() {
    let Some((api, app)) = keys() else {
        publish(json!({ "ok": false, "no_key": true, "loading": false, "error": null }));
        return;
    };
    let list = ids();
    if list.is_empty() {
        publish(json!({ "ok": true, "no_key": false, "loading": false, "error": null }));
        return;
    }
    publish(json!({ "no_key": false, "loading": true }));
    let url = format!("https://api.{}/api/v1/monitor?monitor_ids={}", site(), list.join(","));
    ab::http(
        json!({ "url": url, "method": "GET", "timeout_secs": 20,
                "headers": { "DD-API-KEY": api, "DD-APPLICATION-KEY": app, "Accept": "application/json" } }),
        |r: Value, err: Option<String>| {
            let status = r["status"].as_i64().unwrap_or(0);
            if err.is_some() || status == 0 {
                return publish(json!({ "loading": false, "error": "unreachable" }));
            }
            if status == 401 || status == 403 {
                return publish(json!({ "loading": false, "error": "Datadog rejected the keys" }));
            }
            if !(200..300).contains(&status) {
                return publish(json!({ "loading": false, "error": format!("Datadog answered {status}") }));
            }
            let body = r["body"].as_str().and_then(|s| s.parse::<Value>().ok()).unwrap_or(r["body"].clone());
            let mut d = details();
            for m in body.as_array().cloned().unwrap_or_default() {
                let id = m["id"].as_i64().map(|n| n.to_string()).unwrap_or_default();
                if id.is_empty() { continue }
                d[id.as_str()] = json!({ "name": m["name"], "state": m["overall_state"], "tags": m["tags"] });
            }
            ab::data("datadog_details", d);
            publish(json!({ "ok": true, "loading": false, "error": null, "updated": ab::context()["now"] }));
        },
    );
}

fn set_ids(list: Vec<String>) {
    let vals: Vec<Value> = list.into_iter().map(Value::String).collect();
    ab::run(json!({ "do": "settings.set", "key": "datadog.monitors", "value": vals }));
}

fn init() {
    ab::action("datadog.refresh", |_, _| refresh());
    ab::action("datadog.add", |args, _| {
        let text = args["text"].as_str().unwrap_or("");
        // A monitor URL (…/monitors/123456…) or a bare id: take the first run of 4+ digits.
        let digits: String = {
            let after = text.split("/monitors/").nth(1).unwrap_or(text);
            after.chars().skip_while(|c| !c.is_ascii_digit()).take_while(|c| c.is_ascii_digit()).collect()
        };
        if digits.len() < 3 {
            ab::notify("That doesn't look like a Datadog monitor link or id");
            return;
        }
        let mut list = ids();
        if !list.contains(&digits) {
            list.push(digits);
            set_ids(list);
        }
    });
    ab::action("datadog.remove", |args, _| {
        let id = args["id"].as_str().map(|s| s.to_string()).or(args["id"].as_i64().map(|n| n.to_string())).unwrap_or_default();
        set_ids(ids().into_iter().filter(|x| x != &id).collect());
    });
    ab::on("start", |_, _| {
        // start fires on every settings save: redraw from cache, fetch only for new ids or a stale cache.
        publish(json!({}));
        let d = details();
        let missing = ids().iter().any(|id| d[id.as_str()].is_null());
        let updated = current()["updated"].as_f64().unwrap_or(0.0);
        let mins = cfg()["refresh_minutes"].as_f64().unwrap_or(5.0).max(1.0);
        if missing || ab::context()["now"].as_f64().unwrap_or(0.0) - updated > mins * 60.0 {
            refresh();
        }
        ab::every((mins * 60_000.0) as u64, || refresh());
    });
}
