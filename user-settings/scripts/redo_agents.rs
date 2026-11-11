// Redo Agents (https://agents.redo.builders): the agents and their Work, for the sidebar.
//
// Publishes `.data.redo_agents`:
//   { ok, error, no_key, loading, updated, running_total, attention_total,
//     agents: [{ id, slug, name, url, archived, triggers_paused, runs_7d, last_invoked,
//                running, attention, works: [{ id, title, activity, updated, url }] }] }
//
// The key comes from REDO_AGENTS_API_KEY or ~/.config/redo-agents/api-key (the file the app's own
// client reads); a "Run agents" key is enough. The API is reachable only on the Tailscale tailnet.
// Refreshes every 30s and on the `redo_agents.refresh` action.

const BASE: &str = "https://agents.redo.builders";
// Once a day on its own; the header's refresh icon (redo_agents.refresh) fetches any time.
const EVERY_MS: u64 = 86_400_000;

// The last raw API responses, so a settings change (the include list) re-filters at once, before
// the next fetch.
thread_local! {
    static LAST: std::cell::RefCell<Option<(Value, Value, Value, Value, Option<String>)>> = std::cell::RefCell::new(None);
}

fn remember(agents: &Value, running: &Value, attention: &Value, projects: &Value, works_error: Option<&str>) {
    LAST.with(|l| {
        *l.borrow_mut() = Some((agents.clone(), running.clone(), attention.clone(), projects.clone(), works_error.map(|s| s.to_string())));
    });
    // Raw API responses stay in script memory only: in .data they'd be copied into the window's context on
    // every frame (hundreds of KB), which made scrolling lag. Clear any copy an earlier version left there.
    let _ = works_error;
    if ab::context()["data"]["redo_agents_raw"].is_object() {
        ab::data("redo_agents_raw", json!(false));
    }
}

fn last_raw() -> Option<(Value, Value, Value, Value, Option<String>)> {
    if let Some(l) = LAST.with(|l| l.borrow().clone()) {
        return Some(l);
    }
    let raw = ab::context()["data"]["redo_agents_raw"].clone();
    if !raw.is_object() {
        return None;
    }
    let t = (
        raw["agents"].clone(),
        raw["running"].clone(),
        raw["attention"].clone(),
        raw["projects"].clone(),
        raw["works_error"].as_str().map(|s| s.to_string()),
    );
    LAST.with(|l| *l.borrow_mut() = Some(t.clone()));
    Some(t)
}

fn republish() {
    if let Some((a, r, t, p, e)) = last_raw() {
        publish(build(&a, &r, &t, &p, e.as_deref()));
    } else {
        refilter();
    }
}

// Recompute `shown` / `included` on the agents already published from the current settings alone
// (no raw responses needed): what a settings save uses while a fetch is still in flight.
fn refilter() {
    let mut v = current();
    let Some(agents) = v["agents"].as_array().cloned() else { return };
    let cfg = ab::context()["settings"]["redo_agents"].clone();
    let listed = |key: &str, slug: &str| -> bool {
        cfg[key].as_array().map(|l| l.iter().any(|s| s.as_str() == Some(slug))).unwrap_or(false)
    };
    let out: Vec<Value> = agents
        .into_iter()
        .map(|mut a| {
            let slug = a["slug"].as_str().unwrap_or("").to_string();
            let mine = a["mine"].as_bool().unwrap_or(false);
            a["included"] = json!(listed("include", &slug));
            a["shown"] = json!(listed("include", &slug) || (mine && !listed("exclude", &slug)));
            a
        })
        .collect();
    v["agents"] = Value::Array(out);
    publish(v);
}

fn key() -> Option<String> {
    if let Ok(env) = ab::call_sync("env", json!({"name": "REDO_AGENTS_API_KEY"})) {
        let k = env.as_str().or(env.get("value").and_then(|v| v.as_str())).unwrap_or("").trim().to_string();
        if !k.is_empty() {
            return Some(k);
        }
    }
    ab::read("~/.config/redo-agents/api-key")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn publish(v: Value) {
    ab::data("redo_agents", v);
}

fn current() -> Value {
    ab::context()["data"]["redo_agents"].clone()
}

fn now() -> f64 {
    ab::context()["now"].as_f64().unwrap_or(0.0)
}

// "2026-11-02T19:08:30.123Z" -> unix seconds (UTC; offsets other than Z are ignored).
fn epoch(iso: &str) -> Option<f64> {
    let b = iso.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let num = |s: &str| s.parse::<i64>().ok();
    let y = num(&iso[0..4])?;
    let m = num(&iso[5..7])?;
    let d = num(&iso[8..10])?;
    let hh = num(&iso[11..13])?;
    let mm = num(&iso[14..16])?;
    let ss = num(&iso[17..19])?;
    // days from civil (Howard Hinnant)
    let (y2, m2) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let doy = (153 * m2 + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some((days * 86400 + hh * 3600 + mm * 60 + ss) as f64)
}

fn get(path: &str, k: &str, f: impl FnOnce(Result<Value, String>) + 'static) {
    let url = format!("{BASE}{path}");
    ab::http(
        json!({
            "url": url,
            "method": "GET",
            "headers": { "Authorization": format!("Bearer {k}"), "Accept": "application/json" },
            "timeout_secs": 20
        }),
        move |r: Value, err: Option<String>| {
            if let Some(e) = err {
                return f(Err(e));
            }
            let status = r["status"].as_i64().unwrap_or(0);
            let body = if let Some(s) = r["body"].as_str() {
                s.parse::<Value>().unwrap_or(Value::Null)
            } else {
                r["body"].clone()
            };
            if status == 401 {
                return f(Err("unauthenticated".into()));
            }
            if status == 403 {
                return f(Err("access_denied".into()));
            }
            if status == 0 {
                return f(Err("unreachable".into()));
            }
            if !(200..300).contains(&status) {
                let msg = body["error"]["message"].as_str().unwrap_or("").to_string();
                return f(Err(format!("http {status} {msg}").trim().to_string()));
            }
            f(Ok(body))
        },
    );
}

fn failed(error: String) {
    let mut v = current();
    if !v.is_object() {
        v = json!({ "agents": [] });
    }
    // A failed fetch keeps the agents from the last good one (the list refreshes once a day), with the
    // error shown beside them; only a list that never loaded is hidden.
    let has_agents = v["agents"].as_array().map(|a| !a.is_empty()).unwrap_or(false);
    v["ok"] = json!(has_agents);
    v["loading"] = json!(false);
    v["no_key"] = json!(false);
    v["error"] = json!(error);
    publish(v);
}

fn refresh() {
    let Some(k) = key() else {
        publish(json!({ "ok": false, "no_key": true, "loading": false, "error": null, "agents": [], "running_total": 0, "attention_total": 0 }));
        return;
    };
    let mut v = current();
    if !v.is_object() {
        v = json!({ "ok": false, "agents": [], "error": null, "running_total": 0, "attention_total": 0 });
    }
    v["loading"] = json!(true);
    v["no_key"] = json!(false);
    let has_agents = v["agents"].as_array().map(|a| !a.is_empty()).unwrap_or(false);
    v["ok"] = json!(has_agents);
    publish(v);

    // /v2/agents is the fast runtime list; /v2/agent-projects (git-backed, slow, sometimes 503) only adds
    // runs-per-week and last-used details, so its failure is ignored.
    let k1 = k.clone();
    get("/v2/agents", &k, move |agents| {
        let agents = match agents {
            Ok(a) => a,
            Err(e) => return failed(e),
        };
        // The agent list alone is enough to draw; the Work lists add live status and may fail on their
        // own (the service answers 503 now and then), in which case the rows show without it. The
        // project details (author, runs) from the last good fetch stay in use until a new fetch succeeds,
        // so an agent shown by author never blinks out while they reload.
        let last = last_raw();
        let old_projects = last.as_ref().map(|t| t.3.clone()).unwrap_or(Value::Null);
        let (old_running, old_attention) = last.map(|t| (t.1, t.2)).unwrap_or((Value::Null, Value::Null));
        remember(&agents, &old_running, &old_attention, &old_projects, Some("loading"));
        publish(build(&agents, &old_running, &old_attention, &old_projects, Some("loading")));
        let k2 = k1.clone();
        get("/v2/works?status=running&limit=100", &k1, move |running| {
            let (running, mut works_error) = match running {
                Ok(r) => (r, None),
                Err(e) => (old_running.clone(), Some(e)),
            };
            let k3 = k2.clone();
            get("/v2/works?status=attention&limit=100", &k2, move |attention| {
                let attention = match attention {
                    Ok(a) => a,
                    Err(e) => {
                        works_error = Some(e);
                        old_attention.clone()
                    }
                };
                remember(&agents, &running, &attention, &old_projects, works_error.as_deref());
                publish(build(&agents, &running, &attention, &old_projects, works_error.as_deref()));
                get("/v2/agent-projects", &k3, move |projects| {
                    if let Ok(p) = projects {
                        remember(&agents, &running, &attention, &p, works_error.as_deref());
                        publish(build(&agents, &running, &attention, &p, works_error.as_deref()));
                    }
                });
            });
        });
    });
}

fn work_item(w: &Value, default_activity: &str) -> Value {
    let id = w["id"].as_str().unwrap_or("");
    json!({
        "id": id,
        "agent_id": w["agentId"],
        "agent_slug": w["agentSlug"],
        "title": w["title"].as_str().unwrap_or("Untitled"),
        "activity": w["activity"].as_str().unwrap_or(default_activity),
        "updated": w["updatedAt"].as_str().or(w["createdAt"].as_str()).and_then(epoch),
        "url": format!("{BASE}/works/{id}")
    })
}

fn build(agents: &Value, running: &Value, attention: &Value, projects: &Value, works_error: Option<&str>) -> Value {
    let empty = vec![];
    let works: Vec<Value> = running["items"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .map(|w| work_item(w, "running"))
        .chain(attention["items"].as_array().unwrap_or(&empty).iter().map(|w| work_item(w, "waiting")))
        .collect();
    let project_of = |id: &str| -> Value {
        projects["items"]
            .as_array()
            .unwrap_or(&empty)
            .iter()
            .find(|p| p["agentId"].as_str() == Some(id))
            .cloned()
            .unwrap_or(Value::Null)
    };
    // Ownership is best-effort: the git author of the agent's last change, matched against
    // settings redo_agents.author_match (case-insensitive substring). The sidebar also has an explicit list.
    let cfg = ab::context()["settings"]["redo_agents"].clone();
    let author_match = cfg["author_match"].as_str().unwrap_or("").trim().to_lowercase();
    // Agents once seen as the user's own stay listed even while the (slow, flaky) project details are
    // missing, so they never blink out between fetches. Kept in .data across reloads.
    let mut mine_seen: Vec<String> = ab::context()["data"]["redo_agents_mine"]
        .as_array()
        .map(|a| a.iter().filter_map(|s| s.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();
    let listed = |key: &str, slug: &str| -> bool {
        cfg[key]
            .as_array()
            .map(|v| v.iter().any(|s| s.as_str() == Some(slug)))
            .unwrap_or(false)
    };
    let mut out: Vec<Value> = agents["items"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .map(|a| {
            let id = a["agentId"].as_str().unwrap_or("").to_string();
            let slug = a["agentSlug"].as_str().or(a["slug"].as_str()).unwrap_or("").to_string();
            let p = project_of(&id);
            let mine: Vec<Value> = works
                .iter()
                .filter(|w| w["agent_id"].as_str() == Some(id.as_str()) || w["agent_slug"].as_str() == Some(slug.as_str()))
                .cloned()
                .collect();
            let running_n = mine.iter().filter(|w| w["activity"] == "running").count();
            let attention_n = mine.len() - running_n;
            let author = p["authorName"].as_str().unwrap_or("").to_string();
            let owned_now = !author_match.is_empty() && author.to_lowercase().contains(&author_match);
            if owned_now && !mine_seen.iter().any(|s| s == &slug) {
                mine_seen.push(slug.clone());
            }
            let owned = owned_now || (p.is_null() && mine_seen.iter().any(|s| s == &slug));
            let shown = listed("include", &slug) || (owned && !listed("exclude", &slug));
            json!({
                "author": author,
                "mine": owned,
                "shown": shown,
                "included": listed("include", &slug),
                "id": id,
                "slug": slug,
                "name": a["name"].as_str().unwrap_or(slug.as_str()),
                "url": format!("{BASE}/agents/{id}"),
                "archived": p["archived"].as_str().is_some() || p["archived"].as_bool().unwrap_or(false),
                "triggers_paused": p["triggersPaused"].as_str().is_some() || p["triggersPaused"].as_bool().unwrap_or(false),
                "runs_7d": p["runsLast7Days"].as_i64().unwrap_or(0),
                "last_invoked": p["lastInvokedAt"].as_str().and_then(epoch),
                "running": running_n,
                "attention": attention_n,
                "works": mine
            })
        })
        .filter(|a| !a["archived"].as_bool().unwrap_or(false))
        .collect();
    // Busy agents first, then the most recently used.
    out.sort_by(|a, b| {
        let busy = |x: &Value| x["running"].as_i64().unwrap_or(0) + x["attention"].as_i64().unwrap_or(0);
        busy(b).cmp(&busy(a)).then(
            b["last_invoked"].as_f64().unwrap_or(0.0).partial_cmp(&a["last_invoked"].as_f64().unwrap_or(0.0)).unwrap_or(std::cmp::Ordering::Equal),
        )
    });
    ab::data("redo_agents_mine", json!(mine_seen));
    // Agents not in the sidebar only need what the "Add agent" picker shows; keeping their work lists out
    // of .data keeps the window's per-frame context small.
    let out: Vec<Value> = out
        .into_iter()
        .map(|a| {
            if a["shown"].as_bool().unwrap_or(false) {
                a
            } else {
                json!({ "id": a["id"], "slug": a["slug"], "name": a["name"], "author": a["author"], "mine": a["mine"],
                        "shown": false, "included": a["included"], "url": a["url"], "running": a["running"],
                        "attention": a["attention"], "last_invoked": a["last_invoked"], "works": [] })
            }
        })
        .collect();
    let running_total: i64 = out.iter().map(|a| a["running"].as_i64().unwrap_or(0)).sum();
    let attention_total: i64 = out.iter().map(|a| a["attention"].as_i64().unwrap_or(0)).sum();
    json!({
        "ok": true, "error": null, "no_key": false,
        "loading": works_error == Some("loading"),
        "works_error": if works_error == Some("loading") { Value::Null } else { json!(works_error) },
        "updated": now(),
        "running_total": running_total,
        "attention_total": attention_total,
        "agents": out
    })
}

fn init() {
    ab::action("redo_agents.refresh", |_args, _ctx| refresh());
    ab::on("start", |_, _| {
        // `start` fires on every settings save too: re-filter from what's cached, and only fetch when
        // the last fetch is older than the daily interval (or there is none), so the list sits still.
        republish();
        // Drop the raw copy an earlier version kept in .data (now held in memory by last_raw()).
        if ab::context()["data"]["redo_agents_raw"].is_object() {
            ab::data("redo_agents_raw", json!(false));
        }
        let updated = ab::context()["data"]["redo_agents"]["updated"].as_f64().unwrap_or(0.0);
        let age_ms = (now() - updated) * 1000.0;
        if updated <= 0.0 || age_ms >= EVERY_MS as f64 || ab::context()["data"]["redo_agents"]["agents"].as_array().map_or(true, |a| a.is_empty()) {
            refresh();
        }
        ab::every(EVERY_MS, || refresh());
    });
    // A settings save (the include list changes through the menus) re-filters without a fetch.
    ab::on("settings.reloaded", |_, _| republish());
}
