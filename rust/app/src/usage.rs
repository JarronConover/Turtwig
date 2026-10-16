/*! What chats cost (each daemon's token counts at models.dev rates, or settings `usage.rates`) and how
much of the Claude plan is left (Claude Code's OAuth usage, as its status line caches it). */

use serde_json::{json, Map, Value};
use std::collections::HashMap;

#[derive(Default)]
pub struct Usage {
    /** Each host's last `Usage` reply. */
    pub raw: HashMap<String, Value>,
    /** Model id -> {input, output, cache_read, cache_write, tiers} in dollars per million tokens. */
    pub rates: Map<String, Value>,
    pub rates_updated: Option<String>,
    /** Session id -> (host, real folder, cost, last activity). */
    pub sessions: HashMap<String, (String, String, f64, String)>,
    pub windows: [f64; 4],
    pub unpriced: (u64, Vec<String>),
    pub plan: Value,
    pub plan_updated: Option<f64>,
    pub loading: bool,
}

fn real(p: &str) -> String {
    std::fs::canonicalize(p).map(|x| x.to_string_lossy().into_owned()).unwrap_or_else(|_| p.to_string())
}

impl Usage {
    fn rate(&self, overrides: &Map<String, Value>, model: &str) -> Option<Value> {
        let base = model.split('[').next().unwrap_or(model);
        let pick = |m: &Map<String, Value>| {
            m.get(base).cloned().or_else(|| {
                let mut keys: Vec<&String> = m.keys().filter(|k| base.starts_with(k.as_str())).collect();
                keys.sort_by_key(|k| std::cmp::Reverse(k.len()));
                keys.first().and_then(|k| m.get(*k)).cloned()
            })
        };
        pick(overrides).or_else(|| pick(&self.rates))
    }

    /** Dollars for one row: [model, day, size_class, input, output, write_5m, write_1h, read]. */
    fn row_cost(&self, overrides: &Map<String, Value>, row: &[Value]) -> Option<f64> {
        let model = row.first()?.as_str()?;
        let mut c = self.rate(overrides, model)?;
        let size = row.get(2)?.as_u64().unwrap_or(0);
        // Long prompts can cost more (`tiers`: a context size and its rates).
        if let Some(tiers) = c.get("tiers").and_then(Value::as_array).cloned() {
            for t in tiers {
                let limit = t.pointer("/tier/size").and_then(Value::as_u64).unwrap_or(u64::MAX);
                let over = match size {
                    0 => false,
                    1 => limit <= 100_000,
                    _ => limit <= 200_000,
                };
                if over {
                    c = t;
                }
            }
        }
        let r = |k: &str| c.get(k).and_then(Value::as_f64);
        let input = r("input")?;
        let n = |i: usize| row.get(i).and_then(Value::as_u64).unwrap_or(0) as f64;
        let cost = n(3) * input + n(4) * r("output").unwrap_or(input * 5.) + n(5) * r("cache_write").unwrap_or(input * 1.25)
            + n(6) * r("cache_write_1h").unwrap_or(input * 2.) + n(7) * r("cache_read").unwrap_or(input * 0.1);
        Some(cost / 1e6)
    }

    /** Re-prices everything after new usage or rates. */
    pub fn recompute(&mut self, overrides: &Map<String, Value>) {
        let today = chrono_today();
        let mut windows = [0.; 4];
        let mut sessions = HashMap::new();
        let (mut unpriced, mut unpriced_models): (u64, Vec<String>) = (0, vec![]);
        for (host, v) in &self.raw {
            for s in v["sessions"].as_array().into_iter().flatten() {
                let id = s["id"].as_str().unwrap_or("").to_string();
                let dir = s["dir"].as_str().map(|d| if host == "mac" { real(d) } else { d.to_string() }).unwrap_or_default();
                let mut cost = 0.;
                for row in s["tokens"].as_array().into_iter().flatten().filter_map(Value::as_array) {
                    match self.row_cost(overrides, row) {
                        Some(c) => {
                            cost += c;
                            let age = row.get(1).and_then(Value::as_str).and_then(days_since(&today)).unwrap_or(i64::MAX);
                            for (k, days) in [7, 30, 90].iter().enumerate() {
                                if age < *days {
                                    windows[k] += c;
                                }
                            }
                            windows[3] += c;
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
                let last = s["last"].as_str().unwrap_or("").to_string();
                let e = sessions.entry(id).or_insert((host.clone(), dir, 0., last));
                e.2 += cost;
            }
        }
        self.windows = windows;
        self.sessions = sessions;
        self.unpriced = (unpriced, unpriced_models);
    }

    /** `.usage` in the context: totals for the chat, its worktree and the worktree's tabs, then every
    conversation by window, and the plan's limits. */
    pub fn context(&self, chat: Option<&str>, worktree: Option<(&str, &str)>, tabs: &[(String, String)], meter: &MeterOptions) -> Value {
        let money = |x: f64| format!("${}", thousands(x));
        let chat_cost = chat.and_then(|c| self.sessions.get(c)).map(|s| s.2).unwrap_or(0.);
        let wt_cost: f64 = worktree
            .map(|(host, path)| {
                let p = if host == "mac" { real(path) } else { path.to_string() };
                self.sessions.values().filter(|s| s.0 == host && s.1 == p).map(|s| s.2).sum()
            })
            .unwrap_or(0.);
        json!({
            "loading": self.loading,
            "chat": chat_cost, "chat_text": money(chat_cost),
            "worktree": wt_cost, "worktree_text": money(wt_cost),
            "tabs": tabs.iter().map(|(id, name)| {
                let c = self.sessions.get(id).map(|s| s.2).unwrap_or(0.);
                json!({"id": id, "name": name, "cost": c, "cost_text": money(c)})
            }).collect::<Vec<_>>(),
            "last_7": self.windows[0], "last_7_text": money(self.windows[0]),
            "last_30": self.windows[1], "last_30_text": money(self.windows[1]),
            "last_90": self.windows[2], "last_90_text": money(self.windows[2]),
            "lifetime": self.windows[3], "lifetime_text": money(self.windows[3]),
            "unpriced_tokens": self.unpriced.0, "unpriced_models": self.unpriced.1,
            "rates_updated": self.rates_updated,
            "plan": self.plan_with_meters(meter),
        })
    }

    /** `.plan` plus the gauge: `meter` for the chosen mode, `meters` for every mode (the picker's previews). */
    fn plan_with_meters(&self, o: &MeterOptions) -> Value {
        let mut plan = self.plan.clone();
        let limits: Vec<Value> = plan["limits"].as_array().into_iter().flatten().map(|l| o.decorate(l.clone())).collect();
        plan["limits"] = json!(limits);
        let shown = |mode: &str| meter(&limits, mode).unwrap_or(Value::Null);
        if let Some(p) = plan.as_object_mut() {
            p.insert("meter_mode".into(), json!(o.mode));
            p.insert("meter".into(), shown(&o.mode));
            p.insert("meters".into(), json!({ "smart": shown("smart"), "session": shown("session"), "weekly": shown("weekly") }));
        }
        plan
    }

    /** The plan's limits from Claude Code's OAuth usage cache: [{name, used, left, resets_at, resets_in, severity}]. */
    pub fn load_plan(&mut self) {
        let path = crate::script::expand("~/.claude/oauth-usage-cache.json");
        let Ok(text) = std::fs::read_to_string(&path) else { return };
        let Ok(v) = serde_json::from_str::<Value>(&text) else { return };
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.);
        let limits: Vec<Value> = v["limits"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|l| {
                let used = l["percent"].as_f64().unwrap_or(0.);
                let name = match l["kind"].as_str().unwrap_or("") {
                    "session" => "Session".to_string(),
                    "weekly_all" => "Weekly".to_string(),
                    "weekly_scoped" => format!("Weekly · {}", l.pointer("/scope/model/display_name").and_then(Value::as_str).unwrap_or("model")),
                    other => other.replace('_', " "),
                };
                let resets = l["resets_at"].as_str().and_then(parse_rfc3339);
                json!({
                    "name": name, "kind": l["kind"], "used": used, "left": (100. - used).max(0.), "severity": l["severity"],
                    "resets_at": l["resets_at"], "resets_in": resets.map(|t| duration_text(t - now)),
                })
            })
            .collect();
        let mtime = std::fs::metadata(&path).ok().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs_f64());
        self.plan_updated = mtime;
        self.plan = json!({"limits": limits, "updated": mtime, "updated_text": mtime.map(|t| format!("{} ago", duration_text(now - t)))});
    }
}

/** Settings `usage.meter` (or state `usage_meter`) and `usage.meter_levels`. */
pub struct MeterOptions {
    pub mode: String,
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
window, weekly the longest (the all-models week before a per-model one). */
pub fn meter(limits: &[Value], mode: &str) -> Option<Value> {
    let left = |l: &&Value| l["left"].as_f64().unwrap_or(100.);
    let least = |it: Vec<&Value>| it.into_iter().min_by(|a, b| left(a).total_cmp(&left(b))).cloned();
    let kind = |k: &str| limits.iter().find(|l| l["kind"] == k).cloned();
    let weekly = || kind("weekly_all").or_else(|| least(limits.iter().filter(|l| l["kind"].as_str().is_some_and(|k| k.starts_with("weekly"))).collect()));
    match mode {
        "session" => kind("session").or_else(weekly),
        "weekly" => weekly().or_else(|| kind("session")),
        _ => least(limits.iter().collect()),
    }
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
            json!({"kind": "session", "left": 89.}),
            json!({"kind": "weekly_all", "left": 12.}),
            json!({"kind": "weekly_scoped", "left": 100.}),
        ];
        let kind = |mode: &str| super::meter(&limits, mode).unwrap()["kind"].clone();
        assert_eq!(kind("smart"), "weekly_all");
        assert_eq!(kind("session"), "session");
        assert_eq!(kind("weekly"), "weekly_all");
        assert_eq!(super::meter(&limits[..1], "weekly").unwrap()["kind"], "session");
        assert!(super::meter(&[], "smart").is_none());
        let o = super::MeterOptions { mode: "smart".into(), warning_below: 50., danger_below: 20. };
        let m = o.decorate(limits[1].clone());
        assert_eq!((m["level"].as_str(), m["icon"].as_str()), (Some("danger"), Some("abx-gauge-10")));
        assert!(super::gauge_svg("abx-gauge-10.svg").unwrap().contains("<svg"));
        assert!(super::gauge_svg("clock.svg").is_none());
    }

    #[test]
    fn prices_rows_and_formats_money() {
        let mut u = super::Usage::default();
        u.rates.insert("claude-opus-5-5".into(), serde_json::json!({"input": 4, "output": 20, "cache_read": 0.2, "cache_write": 5}));
        let row: Vec<serde_json::Value> = serde_json::json!(["claude-opus-5-5[1m]", "2026-10-16", 0, 1_000_000, 1_000_000, 0, 1_000_000, 1_000_000]).as_array().unwrap().clone();
        assert_eq!(u.row_cost(&Default::default(), &row), Some(4. + 20. + 8. + 0.2));
        assert_eq!(super::thousands(4454.333), "4,454.33");
        assert_eq!(super::thousands(1.6), "1.60");
        assert_eq!(super::parse_rfc3339("1970-01-02T00:00:01.5+00:00"), Some(86401.));
    }
}
