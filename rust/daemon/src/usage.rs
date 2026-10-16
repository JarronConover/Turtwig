/*! Token usage from every agent session on this machine: Claude Code (~/.claude/projects) and Codex
(~/.codex/sessions), per session, by model, day and prompt size. Files are read once, then only new lines;
each Claude message id and each Codex token count counts once. Codex rollouts also carry its plan limits. */

use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/** Context sizes past which some models price higher (Anthropic's 200k, OpenAI's 272k, ...). */
const SIZE_BOUNDS: [u64; 6] = [100_000, 128_000, 200_000, 272_000, 400_000, 1_000_000];

/** A prompt's size bucket, as the smallest size in it (0, or one past a bound), so a rate tier for
`size` applies when the bucket is over it. */
fn size_class(prompt: u64) -> u64 {
    SIZE_BOUNDS.iter().rev().find(|b| prompt > **b).map_or(0, |b| b + 1)
}

/** Tokens: input, output, cache writes (5 minute), cache writes (1 hour), cache reads. */
type Tokens = [u64; 5];

#[derive(Default)]
struct FileUsage {
    offset: u64,
    /** (model, day, size class) -> tokens */
    totals: HashMap<(String, String, u64), Tokens>,
    dir: Option<String>,
    last: Option<String>,
    /** Codex: the model in use, the running total last counted, and the latest plan limits. */
    model: Option<String>,
    total: u64,
    limits: Option<(String, Value)>,
    /** Codex: the root session a subagent's rollout belongs to. */
    session: Option<String>,
}

static FILES: Mutex<Option<HashMap<PathBuf, FileUsage>>> = Mutex::new(None);
/** Claude message ids already counted, across files: a forked session's transcript repeats its parent's. */
static SEEN: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/** The new complete lines of a file since the last read. */
fn new_lines(path: &Path, f: &mut FileUsage) -> Option<Vec<u8>> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() < f.offset {
        *f = FileUsage::default();
    }
    if meta.len() == f.offset {
        return None;
    }
    let mut file = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    file.seek(SeekFrom::Start(f.offset)).ok()?;
    file.read_to_end(&mut buf).ok()?;
    let end = buf.iter().rposition(|b| *b == b'\n')?;
    f.offset += end as u64 + 1;
    buf.truncate(end);
    Some(buf)
}

fn add(f: &mut FileUsage, model: String, ts: &str, prompt: u64, t: Tokens) {
    let day = ts.get(..10).unwrap_or("").to_string();
    if !ts.is_empty() {
        f.last = Some(ts.to_string());
    }
    let e = f.totals.entry((model, day, size_class(prompt))).or_default();
    for (a, b) in e.iter_mut().zip(t) {
        *a += b;
    }
}

/** A Codex rollout: `turn_context` / `thread_settings_applied` name the model, and each `token_count`
event whose running total moved adds its last call's tokens (input includes the cached part). A fork or
subagent rollout starts by replaying its parent's history, token counts included, before any model is
set: those only set the running total, so they aren't counted twice. */
fn read_codex(path: &Path, f: &mut FileUsage) {
    let Some(buf) = new_lines(path, f) else { return };
    for line in buf.split(|b| *b == b'\n') {
        let interesting = find(line, b"\"token_count\"").is_some() || find(line, b"\"model\"").is_some() || (f.dir.is_none() && find(line, b"\"session_meta\"").is_some());
        if !interesting {
            continue;
        }
        let Ok(e) = serde_json::from_slice::<Value>(line) else { continue };
        let p = &e["payload"];
        let ts = e.get("timestamp").and_then(Value::as_str).unwrap_or("");
        match (e["type"].as_str().unwrap_or(""), p["type"].as_str().unwrap_or("")) {
            ("session_meta", _) => {
                f.dir = p["cwd"].as_str().map(str::to_string);
                f.session = p["session_id"].as_str().map(str::to_string);
            }
            ("turn_context", _) => f.model = p["model"].as_str().map(str::to_string).or(f.model.take()),
            (_, "thread_settings_applied") => f.model = p.pointer("/thread_settings/model").and_then(Value::as_str).map(str::to_string).or(f.model.take()),
            (_, "token_count") => {
                if p["rate_limits"].is_object() {
                    f.limits = Some((ts.to_string(), p["rate_limits"].clone()));
                }
                let total = p.pointer("/info/total_token_usage/total_tokens").and_then(Value::as_u64).unwrap_or(0);
                if total == 0 || total == f.total {
                    continue;
                }
                f.total = total;
                if f.model.is_none() {
                    continue;
                }
                let u = &p["info"]["last_token_usage"];
                let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
                let (input, cached, write) = (n("input_tokens"), n("cached_input_tokens"), n("cache_write_input_tokens"));
                let fresh = input.saturating_sub(cached).saturating_sub(write);
                let model = f.model.clone().unwrap_or_else(|| "unknown".into());
                add(f, model, ts, input, [fresh, n("output_tokens"), write, 0, cached]);
            }
            _ => {}
        }
    }
}

fn read_new(path: &Path, f: &mut FileUsage, seen: &mut HashSet<String>) {
    let Some(buf) = new_lines(path, f) else { return };
    for line in buf.split(|b| *b == b'\n') {
        if f.dir.is_none() {
            if let Some(i) = find(line, b"\"cwd\":\"") {
                let rest = &line[i + 7..];
                if let Some(j) = rest.iter().position(|b| *b == b'"') {
                    f.dir = Some(String::from_utf8_lossy(&rest[..j]).into_owned());
                }
            }
        }
        if find(line, b"\"usage\"").is_none() || find(line, b"\"assistant\"").is_none() {
            continue;
        }
        let Ok(e) = serde_json::from_slice::<Value>(line) else { continue };
        if e.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let m = &e["message"];
        let id = m.get("id").and_then(Value::as_str).or(e.get("requestId").and_then(Value::as_str)).unwrap_or("").to_string();
        if !id.is_empty() && !seen.insert(id) {
            continue;
        }
        let u = &m["usage"];
        let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
        let (input, output, write, read) = (n("input_tokens"), n("output_tokens"), n("cache_creation_input_tokens"), n("cache_read_input_tokens"));
        let hour = u.pointer("/cache_creation/ephemeral_1h_input_tokens").and_then(Value::as_u64).unwrap_or(0).min(write);
        let model = m.get("model").and_then(Value::as_str).unwrap_or("unknown").to_string();
        if model == "<synthetic>" {
            continue;
        }
        let ts = e.get("timestamp").and_then(Value::as_str).unwrap_or("");
        add(f, model, ts, input + write + read, [input, output, write - hour, hour, read]);
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/** Codex rollouts, `rollout-<time>-<session id>.jsonl` by day, plus archived ones (one path per id). */
fn codex_paths() -> Vec<(String, PathBuf)> {
    let root = crate::util::home().join(".codex");
    let mut out: HashMap<String, PathBuf> = HashMap::new();
    let mut stack = vec![root.join("sessions"), root.join("archived_sessions")];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let Some(stem) = name.strip_prefix("rollout-").and_then(|n| n.strip_suffix(".jsonl")) else { continue };
            // The session id is the last five dash-separated groups (a UUID).
            let parts: Vec<&str> = stem.split('-').collect();
            let id = if parts.len() >= 5 { parts[parts.len() - 5..].join("-") } else { stem.to_string() };
            let bigger = |q: &PathBuf| std::fs::metadata(q).map(|m| m.len()).unwrap_or(0) < std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            if out.get(&id).is_none_or(bigger) {
                out.insert(id, p);
            }
        }
    }
    out.into_iter().collect()
}

/** Every session's usage (subagent files count toward their session): `{host, sessions: [{id, agent,
provider, dir, last, tokens: [[model, day, size, input, output, cache_write_5m, cache_write_1h, cache_read],
...]}], plans: [{provider, at, rate_limits}]}`. `size` is the smallest prompt size in the row's bucket. */
pub fn scan() -> Value {
    let root = crate::util::home().join(".claude").join("projects");
    let mut paths: Vec<(String, PathBuf)> = vec![];
    for project in std::fs::read_dir(&root).into_iter().flatten().flatten() {
        for e in std::fs::read_dir(project.path()).into_iter().flatten().flatten() {
            let p = e.path();
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if let Some(id) = name.strip_suffix(".jsonl") {
                paths.push((id.to_string(), p.clone()));
            } else if p.is_dir() {
                for s in std::fs::read_dir(p.join("subagents")).into_iter().flatten().flatten() {
                    let sp = s.path();
                    if sp.extension().is_some_and(|x| x == "jsonl") {
                        paths.push((name.clone(), sp));
                    }
                }
            }
        }
    }
    let mut all = FILES.lock().unwrap();
    let files = all.get_or_insert_with(HashMap::new);
    let mut sessions: HashMap<String, (Option<String>, Option<String>, Vec<Value>, &str)> = HashMap::new();
    let mut codex_limits: Option<(String, Value)> = None;
    for (id, p) in codex_paths() {
        let f = files.entry(p.clone()).or_default();
        read_codex(&p, f);
        if f.limits.as_ref().is_some_and(|(at, _)| codex_limits.as_ref().is_none_or(|(best, _)| at > best)) {
            codex_limits = f.limits.clone();
        }
        let own = f.session.as_ref().is_none_or(|root| *root == id);
        let s = sessions.entry(f.session.clone().unwrap_or(id)).or_default();
        s.3 = "codex";
        if own || s.0.is_none() {
            s.0 = f.dir.clone();
        }
        if f.last > s.1 {
            s.1 = f.last.clone();
        }
        for ((model, day, size), t) in &f.totals {
            s.2.push(json!([model, day, size, t[0], t[1], t[2], t[3], t[4]]));
        }
    }
    let mut seen_all = SEEN.lock().unwrap();
    let seen = seen_all.get_or_insert_with(HashSet::new);
    // Older files first, so a fork's copies are skipped rather than its parent's originals.
    paths.sort_by_key(|(_, p)| std::fs::metadata(p).and_then(|m| m.created().or_else(|_| m.modified())).ok());
    for (id, p) in paths {
        let f = files.entry(p.clone()).or_default();
        read_new(&p, f, seen);
        let s = sessions.entry(id).or_default();
        s.3 = "claude";
        if s.0.is_none() && !p.to_string_lossy().contains("/subagents/") {
            s.0 = f.dir.clone();
        }
        if f.last > s.1 {
            s.1 = f.last.clone();
        }
        for ((model, day, size), t) in &f.totals {
            s.2.push(json!([model, day, size, t[0], t[1], t[2], t[3], t[4]]));
        }
    }
    json!({
        "host": crate::mac::host(),
        "sessions": sessions
            .into_iter()
            .filter(|(_, s)| !s.2.is_empty())
            .map(|(id, (dir, last, tokens, agent))| {
                let provider = if agent == "codex" { "openai" } else { "anthropic" };
                json!({"id": id, "agent": agent, "provider": provider, "dir": dir, "last": last, "tokens": tokens})
            })
            .collect::<Vec<_>>(),
        "plans": codex_limits.map(|(at, l)| vec![json!({"provider": "openai", "at": at, "rate_limits": l})]).unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_counts_each_token_total_once() {
        let dir = std::env::temp_dir().join(format!("abx-usage-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("rollout-2026-10-09T10-03-02-01a12167-5455-7dd3-beb2-da880f158645.jsonl");
        let tc = |total: u64, ts: &str| format!(
            r#"{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"total_tokens":{total}}},"last_token_usage":{{"input_tokens":1000,"cached_input_tokens":400,"output_tokens":50}}}},"rate_limits":{{"primary":{{"used_percent":39.0,"window_minutes":300,"resets_at":1791578992}}}}}}}}"#
        );
        let lines = [
            r#"{"timestamp":"2026-10-09T16:03:00Z","type":"session_meta","payload":{"id":"x","cwd":"/w"}}"#.to_string(),
            tc(500, "2026-10-09T16:03:00Z"),
            r#"{"timestamp":"2026-10-09T16:03:01Z","type":"turn_context","payload":{"model":"gpt-6-sol"}}"#.to_string(),
            tc(1050, "2026-10-09T16:03:02Z"),
            tc(1050, "2026-10-09T16:03:03Z"),
            tc(2100, "2026-10-09T16:03:04Z"),
        ];
        std::fs::write(&p, lines.join("\n") + "\n").unwrap();
        let mut f = FileUsage::default();
        read_codex(&p, &mut f);
        assert_eq!(f.dir.as_deref(), Some("/w"));
        let t = f.totals.get(&("gpt-6-sol".to_string(), "2026-10-09".to_string(), 0)).copied().unwrap();
        assert_eq!(t, [1200, 100, 0, 0, 800]);
        assert_eq!(f.limits.as_ref().map(|l| l.0.as_str()), Some("2026-10-09T16:03:04Z"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn size_buckets_sit_past_each_bound() {
        assert_eq!(size_class(5_000), 0);
        assert_eq!(size_class(150_000), 128_001);
        assert_eq!(size_class(250_000), 200_001);
        assert_eq!(size_class(300_000), 272_001);
    }
}
