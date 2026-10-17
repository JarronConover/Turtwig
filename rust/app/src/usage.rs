/*! What chats cost and how much of each plan is left, for every provider in settings `usage.providers`
(Claude Code on Anthropic, Codex on OpenAI, ...). Costs: each daemon's token counts at models.dev rates
(or settings `usage.rates`). Plans: Claude Code's OAuth usage cache, and the rate limits Codex writes
into its rollouts. */

use serde_json::{json, Map, Value};
use std::collections::HashMap;

pub struct Session {
    pub host: String,
    /** The real folder it ran in. */
    pub dir: String,
    pub cost: f64,
    pub provider: String,
}

#[derive(Default)]
pub struct Usage {
    /** Each host's last `Usage` reply. */
    pub raw: HashMap<String, Value>,
    /** models.dev provider -> model id -> {input, output, cache_read, cache_write, tiers} in dollars per million tokens. */
    pub rates: Map<String, Value>,
    pub rates_updated: Option<String>,
    pub sessions: HashMap<String, Session>,
    /** Provider -> cost in the last 7, 30 and 90 days and ever. */
    pub windows: HashMap<String, [f64; 4]>,
    pub unpriced: (u64, Vec<String>),
    /** Claude Code's limits and when its cache was written. */
    pub claude_plan: Vec<Value>,
    pub plan_updated: Option<f64>,
    /** Plans read from a file through a jq expression (`plan: {source: "file"}`): provider -> limits, when. */
    pub file_plans: HashMap<String, (Vec<Value>, Option<f64>)>,
    pub loading: bool,
    /** Settings this needs, set by the workspace: `app.local_host`, `usage.rate_fallbacks`, `strings`. */
    pub local_host: String,
    pub rate_fallbacks: Map<String, Value>,
    pub strings: Value,
}

impl Usage {
    /** A user-facing string from settings `strings` (else `default`), `{n}` filled in. */
    fn text(&self, key: &str, default: &str) -> String {
        self.strings.get(key).and_then(Value::as_str).unwrap_or(default).to_string()
    }

    fn is_local(&self, host: &str) -> bool {
        host == if self.local_host.is_empty() { "mac" } else { self.local_host.as_str() }
    }

    /** "Session" for windows under a day, "Daily", "Weekly", else the length (`strings.usage_*`). */
    fn window_name(&self, minutes: f64) -> String {
        match minutes as u64 {
            0..=1439 => self.text("usage_session", "Session"),
            1440 => self.text("usage_daily", "Daily"),
            10080 => self.text("usage_weekly", "Weekly"),
            m if m % 1440 == 0 => self.text("usage_days", "{n} days").replace("{n}", &(m / 1440).to_string()),
            m => self.text("usage_hours", "{n}h").replace("{n}", &(m / 60).to_string()),
        }
    }

    fn ago(&self, secs: f64) -> String {
        if secs < 60. { self.text("usage_just_now", "just now") } else { self.text("usage_ago", "{t} ago").replace("{t}", &duration_text(secs)) }
    }

    /** Reads a provider's plan from a file through `jq` (settings `plan: {source: "file", path, jq}`):
    the expression turns the file's JSON into `[{name, kind, window_minutes, used, resets_at}]` (used in
    percent; resets_at in unix seconds or RFC 3339). */
    pub fn load_file_plan(&mut self, provider: &str, path: &str, jq: &str, exprs: &crate::ui::expr::Exprs) {
        let path = crate::script::expand(path);
        let Some(v) = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else { return };
        let out = crate::ui::expr::to_json(&exprs.first(jq, &crate::ui::expr::to_val(&v)));
        let limits = out
            .as_array()
            .into_iter()
            .flatten()
            .map(|l| {
                let minutes = l["window_minutes"].as_f64().unwrap_or(0.);
                let resets = l["resets_at"].as_f64().or_else(|| l["resets_at"].as_str().and_then(parse_rfc3339));
                let name = l["name"].as_str().map(str::to_string).unwrap_or_else(|| self.window_name(minutes));
                limit(provider, name, l["kind"].as_str().unwrap_or(""), minutes, l["used"].as_f64().unwrap_or(0.), resets)
            })
            .collect();
        let updated = std::fs::metadata(&path).ok().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs_f64());
        self.file_plans.insert(provider.to_string(), (limits, updated));
    }
}

fn real(p: &str) -> String {
    std::fs::canonicalize(p).map(|x| x.to_string_lossy().into_owned()).unwrap_or_else(|_| p.to_string())
}

fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.)
}

fn limit(provider: &str, name: String, kind: &str, minutes: f64, used: f64, resets: Option<f64>) -> Value {
    // A window that has reset since the numbers were written starts over.
    let used = if resets.is_some_and(|t| t < now()) { 0. } else { used };
    json!({
        "provider": provider, "name": name, "kind": kind, "window_minutes": minutes,
        "used": used, "left": (100. - used).max(0.), "resets_at": resets, "resets_in": resets.map(|t| duration_text(t - now())),
    })
}

impl Usage {
    fn rate(&self, overrides: &Map<String, Value>, provider: &str, model: &str) -> Option<Value> {
        let base = model.split('[').next().unwrap_or(model);
        let pick = |m: &Map<String, Value>| {
            m.get(base).cloned().or_else(|| {
                let mut keys: Vec<&String> = m.keys().filter(|k| base.starts_with(k.as_str())).collect();
                keys.sort_by_key(|k| std::cmp::Reverse(k.len()));
                keys.first().and_then(|k| m.get(*k)).cloned()
            })
        };
        let own = self.rates.get(provider).and_then(Value::as_object);
        pick(overrides).or_else(|| own.and_then(pick)).or_else(|| self.rates.values().filter_map(Value::as_object).find_map(pick))
    }

    /** Dollars for one row: [model, day, size, input, output, write_5m, write_1h, read], where size is the
    smallest prompt size in its bucket (0, 1 and 2 from older daemons: up to 100k, 200k, more). */
    fn row_cost(&self, overrides: &Map<String, Value>, provider: &str, row: &[Value]) -> Option<f64> {
        let model = row.first()?.as_str()?;
        let mut c = self.rate(overrides, provider, model)?;
        let size = match row.get(2)?.as_u64().unwrap_or(0) {
            1 => 100_001,
            2 => 200_001,
            n => n,
        };
        // Long prompts can cost more (`tiers`: a context size and its rates).
        if let Some(tiers) = c.get("tiers").and_then(Value::as_array).cloned() {
            for t in tiers {
                if size > t.pointer("/tier/size").and_then(Value::as_u64).unwrap_or(u64::MAX) {
                    c = t;
                }
            }
        }
        let r = |k: &str| c.get(k).and_then(Value::as_f64);
        let input = r("input")?;
        let n = |i: usize| row.get(i).and_then(Value::as_u64).unwrap_or(0) as f64;
        // A rate a table lacks is the input rate times `usage.rate_fallbacks`.
        let times = |k: &str, d: f64| self.rate_fallbacks.get(k).and_then(Value::as_f64).unwrap_or(d);
        let cost = n(3) * input + n(4) * r("output").unwrap_or(input * times("output", 5.)) + n(5) * r("cache_write").unwrap_or(input * times("cache_write", 1.25))
            + n(6) * r("cache_write_1h").unwrap_or(input * times("cache_write_1h", 2.)) + n(7) * r("cache_read").unwrap_or(input * times("cache_read", 0.1));
        Some(cost / 1e6)
    }

    /** Re-prices everything after new usage or rates. */
    pub fn recompute(&mut self, overrides: &Map<String, Value>) {
        let today = chrono_today();
        let mut windows: HashMap<String, [f64; 4]> = HashMap::new();
        let mut sessions: HashMap<String, Session> = HashMap::new();
        let (mut unpriced, mut unpriced_models): (u64, Vec<String>) = (0, vec![]);
        for (host, v) in &self.raw {
            for s in v["sessions"].as_array().into_iter().flatten() {
                let id = s["id"].as_str().unwrap_or("").to_string();
                let provider = s["provider"].as_str().unwrap_or("anthropic").to_string();
                let dir = s["dir"].as_str().map(|d| if self.is_local(host) { real(d) } else { d.to_string() }).unwrap_or_default();
                let mut cost = 0.;
                for row in s["tokens"].as_array().into_iter().flatten().filter_map(Value::as_array) {
                    match self.row_cost(overrides, &provider, row) {
                        Some(c) => {
                            cost += c;
                            let w = windows.entry(provider.clone()).or_default();
                            let age = row.get(1).and_then(Value::as_str).and_then(days_since(&today)).unwrap_or(i64::MAX);
                            for (k, days) in [7, 30, 90].iter().enumerate() {
                                if age < *days {
                                    w[k] += c;
                                }
                            }
                            w[3] += c;
                        }
                        None => {
                            unpriced += (3..8).map(|i| row.get(i).and_then(Value::as_u64).unwrap_or(0)).sum::<u64>();
                            let m = row.first().and_then(Value::as_str).unwrap_or("").to_string();
                            if !unpriced_models.contains(&m) {
                                unpriced_models.push(m);
                            }
                        }
                    }
                }
                sessions.entry(id).or_insert(Session { host: host.clone(), dir, cost: 0., provider }).cost += cost;
            }
        }
        self.windows = windows;
        self.sessions = sessions;
        self.unpriced = (unpriced, unpriced_models);
    }

    /** `.usage` in the context: totals for the chat, its worktree and the worktree's tabs, every
    conversation by window (in all and per provider), and each provider's plan. */
    pub fn context(&self, chat: Option<&str>, worktree: Option<(&str, &str)>, tabs: &[(String, String)], providers: &Map<String, Value>, meter: &MeterOptions) -> Value {
        let money = |x: f64| format!("${}", thousands(x));
        let chat_cost = chat.and_then(|c| self.sessions.get(c)).map(|s| s.cost).unwrap_or(0.);
        let wt_cost: f64 = worktree
            .map(|(host, path)| {
                let p = if self.is_local(host) { real(path) } else { path.to_string() };
                self.sessions.values().filter(|s| s.host == host && s.dir == p).map(|s| s.cost).sum()
            })
            .unwrap_or(0.);
        let mut all = [0.; 4];
        for w in self.windows.values() {
            for (a, b) in all.iter_mut().zip(w) {
                *a += b;
            }
        }
        let plans: Vec<Value> = self
            .plans(providers)
            .into_iter()
            .map(|mut p| {
                p["limits"] = p["limits"].as_array().into_iter().flatten().map(|l| meter.decorate(l.clone())).collect();
                p
            })
            .collect();
        let by_provider: Vec<Value> = providers
            .iter()
            .filter(|(id, _)| self.windows.contains_key(*id))
            .map(|(id, p)| {
                let w = self.windows[id];
                json!({
                    "id": id, "name": p["name"].as_str().unwrap_or(id), "icon": p["icon"], "color": p["color"],
                    "last_7": w[0], "last_7_text": money(w[0]), "last_30": w[1], "last_30_text": money(w[1]),
                    "last_90": w[2], "last_90_text": money(w[2]), "lifetime": w[3], "lifetime_text": money(w[3]),
                })
            })
            .collect();
        json!({
            "loading": self.loading,
            "chat": chat_cost, "chat_text": money(chat_cost),
            "chat_provider": chat.and_then(|c| self.sessions.get(c)).map(|s| s.provider.clone()),
            "worktree": wt_cost, "worktree_text": money(wt_cost),
            "tabs": tabs.iter().map(|(id, name)| {
                let c = self.sessions.get(id).map(|s| s.cost).unwrap_or(0.);
                json!({"id": id, "name": name, "cost": c, "cost_text": money(c)})
            }).collect::<Vec<_>>(),
            "last_7": all[0], "last_7_text": money(all[0]),
            "last_30": all[1], "last_30_text": money(all[1]),
            "last_90": all[2], "last_90_text": money(all[2]),
            "lifetime": all[3], "lifetime_text": money(all[3]),
            "providers": by_provider,
            "unpriced_tokens": self.unpriced.0, "unpriced_models": self.unpriced.1,
            "rates_updated": self.rates_updated,
            "plan": self.plan_with_meters(&plans, meter),
            "plans": plans,
        })
    }

    /** Each provider's plan: `{provider, name, icon, color, limits, updated, updated_text}`, for those
    with a `plan` source (`claude_oauth`, `codex_rollouts`, or `{source: "file"}`) and numbers to show. */
    fn plans(&self, providers: &Map<String, Value>) -> Vec<Value> {
        let mut out = vec![];
        for (id, p) in providers {
            let source = p["plan"].as_str().or_else(|| p["plan"]["source"].as_str());
            let (limits, updated) = match source {
                Some("claude_oauth") => (self.claude_plan.iter().map(|l| { let mut l = l.clone(); l["provider"] = json!(id); l }).collect(), self.plan_updated),
                // The daemon files Codex's limits under its own provider name (`plan_provider`, default the id).
                Some("codex_rollouts") => self.codex_plan(id, p["plan_provider"].as_str().unwrap_or(id)),
                Some("file") => self.file_plans.get(id).cloned().unwrap_or_default(),
                _ => continue,
            };
            if limits.is_empty() {
                continue;
            }
            let name = p["name"].as_str().unwrap_or(id);
            let limits: Vec<Value> = limits.into_iter().map(|mut l| { l["provider_name"] = json!(name); l }).collect();
            out.push(json!({
                "provider": id, "name": name, "icon": p["icon"], "color": p["color"], "limits": limits,
                "updated": updated, "updated_text": updated.map(|t| self.ago(now() - t)),
            }));
        }
        out
    }

    /** The newest rate limits any host's Codex rollouts hold: its primary and secondary windows. */
    fn codex_plan(&self, provider: &str, filed_as: &str) -> (Vec<Value>, Option<f64>) {
        let newest = self
            .raw
            .values()
            .flat_map(|v| v["plans"].as_array().cloned().unwrap_or_default())
            .filter(|p| p["provider"] == filed_as)
            .max_by(|a, b| a["at"].as_str().unwrap_or("").cmp(b["at"].as_str().unwrap_or("")));
        let Some(p) = newest else { return (vec![], None) };
        let limits = ["primary", "secondary"]
            .iter()
            .filter_map(|k| {
                let w = p["rate_limits"].get(*k).filter(|w| w.is_object())?;
                let minutes = w["window_minutes"].as_f64().unwrap_or(0.);
                Some(limit(provider, self.window_name(minutes), k, minutes, w["used_percent"].as_f64().unwrap_or(0.), w["resets_at"].as_f64()))
            })
            .collect();
        (limits, p["at"].as_str().and_then(parse_rfc3339))
    }

    /** `.plan`: every provider's limits in one list, plus the gauge: `meter` for the chosen mode and
    `meters` for every mode (the picker's previews), over the providers in `usage.meter_providers`. */
    fn plan_with_meters(&self, plans: &[Value], o: &MeterOptions) -> Value {
        let limits: Vec<Value> = plans.iter().flat_map(|p| p["limits"].as_array().cloned().unwrap_or_default()).collect();
        let counted: Vec<Value> = limits.iter().filter(|l| o.providers.as_ref().is_none_or(|ps| ps.iter().any(|p| l["provider"] == p.as_str()))).cloned().collect();
        let shown = |mode: &str| meter(&counted, mode).unwrap_or(Value::Null);
        let updated = plans.iter().filter_map(|p| p["updated"].as_f64()).reduce(f64::max);
        json!({
            "limits": limits,
            "updated": updated, "updated_text": updated.map(|t| self.ago(now() - t)),
            "meter_mode": o.mode, "meter": shown(&o.mode),
            "meters": { "smart": shown("smart"), "session": shown("session"), "weekly": shown("weekly") },
        })
    }

    /** Claude Code's limits from its OAuth usage cache. */
    pub fn load_plan(&mut self, path: &str) {
        let path = crate::script::expand(path);
        let Ok(text) = std::fs::read_to_string(&path) else { return };
        let Ok(v) = serde_json::from_str::<Value>(&text) else { return };
        self.claude_plan = v["limits"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|l| {
                let kind = l["kind"].as_str().unwrap_or("");
                let minutes = if kind == "session" { 300. } else if kind.starts_with("weekly") { 10080. } else { 0. };
                let name = match kind {
                    "weekly_scoped" => self.text("usage_weekly_model", "Weekly · {model}").replace("{model}", l.pointer("/scope/model/display_name").and_then(Value::as_str).unwrap_or("model")),
                    _ if minutes > 0. => self.window_name(minutes),
                    other => other.replace('_', " "),
                };
                let mut out = limit("", name, kind, minutes, l["percent"].as_f64().unwrap_or(0.), l["resets_at"].as_str().and_then(parse_rfc3339));
                out["severity"] = l["severity"].clone();
                out
            })
            .collect();
        self.plan_updated = std::fs::metadata(&path).ok().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs_f64());
    }
}

/** Settings `usage.meter` (or state `usage_meter`) and `usage.meter_levels`. */
pub struct MeterOptions {
    pub mode: String,
    /** Only these providers' limits drive the gauge (None: all). */
    pub providers: Option<Vec<String>>,
    pub warning_below: f64,
    pub danger_below: f64,
}

impl MeterOptions {
    fn decorate(&self, mut l: Value) -> Value {
        let left = l["left"].as_f64().unwrap_or(0.);
        let level = if left < self.danger_below { "danger" } else if left < self.warning_below { "warning" } else { "ok" };
        l["level"] = json!(level);
        l["icon"] = json!(format!("abx-gauge-{}", gauge_step(left)));
        l
    }
}

/** The limit a meter mode reports: smart is the one closest to running out, session the shortest
window, weekly the longest; ties go to the one with less left. */
pub fn meter(limits: &[Value], mode: &str) -> Option<Value> {
    let left = |l: &Value| l["left"].as_f64().unwrap_or(100.);
    let window = |l: &Value| l["window_minutes"].as_f64().unwrap_or(0.);
    let by = |a: &&Value, b: &&Value| match mode {
        "session" => window(a).total_cmp(&window(b)).then(left(a).total_cmp(&left(b))),
        "weekly" => window(b).total_cmp(&window(a)).then(left(a).total_cmp(&left(b))),
        _ => left(a).total_cmp(&left(b)).then(window(a).total_cmp(&window(b))),
    };
    limits.iter().min_by(by).cloned()
}

/** Percent left, rounded to the 5% steps the gauge icons come in. */
fn gauge_step(left: f64) -> u32 {
    ((left.clamp(0., 100.) / 5.).round() * 5.) as u32
}

/** The toolbar gauge for `icons/abx-gauge-N.svg`: a faint 270° track, the arc filled to N% left, and
a needle at its end. Drawn in currentColor, so the icon's color is the meter's level. */
pub fn gauge_svg(name: &str) -> Option<String> {
    let pct: f64 = name.strip_prefix("abx-gauge-")?.strip_suffix(".svg")?.parse().ok()?;
    let (cx, cy, r) = (12., 13., 9.);
    let at = |deg: f64, rr: f64| {
        let a = deg.to_radians();
        (cx + rr * a.cos(), cy + rr * a.sin())
    };
    let f = (pct / 100.).clamp(0., 1.);
    let (sx, sy) = at(135., r);
    let (tx, ty) = at(45., r);
    let end = 135. + 270. * f;
    let (ex, ey) = at(end, r);
    let (nx, ny) = at(end, r - 4.);
    let arc = if f > 0.005 { format!(r#"<path d="M{sx:.3} {sy:.3}A{r} {r} 0 {} 1 {ex:.3} {ey:.3}"/>"#, (270. * f > 180.) as u8) } else { String::new() };
    Some(format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path stroke-opacity="0.3" d="M{sx:.3} {sy:.3}A{r} {r} 0 1 1 {tx:.3} {ty:.3}"/>{arc}<path d="M{cx} {cy}L{nx:.3} {ny:.3}"/></svg>"#
    ))
}

/** 1234.5 -> "1,234.50" */
fn thousands(x: f64) -> String {
    let s = format!("{x:.2}");
    let (int, frac) = s.split_once('.').unwrap_or((&s, "00"));
    let neg = int.starts_with('-');
    let digits: Vec<char> = int.trim_start_matches('-').chars().collect();
    let mut out = String::new();
    for (i, c) in digits.iter().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*c);
    }
    format!("{}{out}.{frac}", if neg { "-" } else { "" })
}

fn duration_text(secs: f64) -> String {
    let s = secs.max(0.) as u64;
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{}m", s / 60),
        3600..=86399 => format!("{}h {}m", s / 3600, (s % 3600) / 60),
        _ => format!("{}d {}h", s / 86400, (s % 86400) / 3600),
    }
}

/** Seconds since the epoch for "2026-10-17T01:09:59.67+00:00" (UTC offsets only). */
fn parse_rfc3339(s: &str) -> Option<f64> {
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-').map(|x| x.parse::<i64>().ok());
    let (y, m, dd) = (d.next()??, d.next()??, d.next()??);
    let t = time.get(..8)?;
    let mut tt = t.split(':').map(|x| x.parse::<i64>().ok());
    let (h, mi, se) = (tt.next()??, tt.next()??, tt.next()??);
    Some((days_from_civil(y, m, dd) * 86400 + h * 3600 + mi * 60 + se) as f64)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn chrono_today() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64 / 86400).unwrap_or(0)
}

fn days_since(today: &i64) -> impl Fn(&str) -> Option<i64> + '_ {
    move |day: &str| {
        let mut p = day.split('-').map(|x| x.parse::<i64>().ok());
        let (y, m, d) = (p.next()??, p.next()??, p.next()??);
        Some(today - days_from_civil(y, m, d))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn meter_modes_pick_their_window() {
        use serde_json::json;
        let limits = vec![
            json!({"kind": "session", "window_minutes": 300., "left": 89.}),
            json!({"kind": "weekly_all", "window_minutes": 10080., "left": 12.}),
            json!({"kind": "weekly_scoped", "window_minutes": 10080., "left": 100.}),
        ];
        let kind = |mode: &str| super::meter(&limits, mode).unwrap()["kind"].clone();
        assert_eq!(kind("smart"), "weekly_all");
        assert_eq!(kind("session"), "session");
        assert_eq!(kind("weekly"), "weekly_all");
        assert_eq!(super::meter(&limits[..1], "weekly").unwrap()["kind"], "session");
        let u = super::Usage::default();
        assert_eq!(u.window_name(300.), "Session");
        assert_eq!(u.window_name(10080.), "Weekly");
        let u = super::Usage { strings: serde_json::json!({"usage_weekly": "Semana"}), ..Default::default() };
        assert_eq!(u.window_name(10080.), "Semana");
        assert!(super::meter(&[], "smart").is_none());
        let o = super::MeterOptions { mode: "smart".into(), providers: None, warning_below: 50., danger_below: 20. };
        let m = o.decorate(limits[1].clone());
        assert_eq!((m["level"].as_str(), m["icon"].as_str()), (Some("danger"), Some("abx-gauge-10")));
        assert!(super::gauge_svg("abx-gauge-10.svg").unwrap().contains("<svg"));
        assert!(super::gauge_svg("clock.svg").is_none());
    }

    #[test]
    fn prices_rows_and_formats_money() {
        let mut u = super::Usage::default();
        u.rates.insert("anthropic".into(), serde_json::json!({"claude-opus-5-5": {"input": 4, "output": 20, "cache_read": 0.2, "cache_write": 5}}));
        u.rates.insert("openai".into(), serde_json::json!({"gpt-6-sol": {"input": 2, "output": 10, "cache_read": 0.2, "tiers": [{"input": 4, "output": 15, "cache_read": 0.4, "tier": {"size": 272000}}]}}));
        let row = |v: serde_json::Value| v.as_array().unwrap().clone();
        let claude = row(serde_json::json!(["claude-opus-5-5[1m]", "2026-10-16", 0, 1_000_000, 1_000_000, 0, 1_000_000, 1_000_000]));
        assert_eq!(u.row_cost(&Default::default(), "anthropic", &claude), Some(4. + 20. + 8. + 0.2));
        let small = row(serde_json::json!(["gpt-6-sol", "2026-10-16", 200_001, 1_000_000, 0, 0, 0, 0]));
        let long = row(serde_json::json!(["gpt-6-sol", "2026-10-16", 272_001, 1_000_000, 0, 0, 0, 0]));
        assert_eq!(u.row_cost(&Default::default(), "openai", &small), Some(2.));
        assert_eq!(u.row_cost(&Default::default(), "openai", &long), Some(4.));
        assert_eq!(super::thousands(4454.333), "4,454.33");
        assert_eq!(super::thousands(1.6), "1.60");
        assert_eq!(super::parse_rfc3339("1970-01-02T00:00:01.5+00:00"), Some(86401.));
        u.rate_fallbacks.insert("output".into(), serde_json::json!(2));
        u.rates.insert("x".into(), serde_json::json!({"m": {"input": 1}}));
        assert_eq!(u.row_cost(&Default::default(), "x", &row(serde_json::json!(["m", "2026-10-16", 0, 0, 1_000_000, 0, 0, 0]))), Some(2.));
    }

    #[test]
    fn a_file_plan_comes_through_its_jq() {
        let dir = std::env::temp_dir().join(format!("abx-plan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("plan.json");
        std::fs::write(&f, r#"{"windows": [{"mins": 300, "pct": 40}]}"#).unwrap();
        let mut u = super::Usage::default();
        let e = crate::ui::expr::Exprs::default();
        u.load_file_plan("acme", &f.to_string_lossy(), "[.windows[] | {window_minutes: .mins, used: .pct, kind: \"session\"}]", &e);
        let (limits, updated) = u.file_plans["acme"].clone();
        assert_eq!((limits[0]["name"].as_str(), limits[0]["left"].as_f64()), (Some("Session"), Some(60.)));
        assert!(updated.is_some());
        let providers: serde_json::Map<String, serde_json::Value> = serde_json::from_value(serde_json::json!({"acme": {"name": "Acme", "plan": {"source": "file"}}})).unwrap();
        assert_eq!(u.plans(&providers)[0]["provider"], "acme");
        let _ = std::fs::remove_dir_all(dir);
    }
}
