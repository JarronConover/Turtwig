/*! Token usage from every Claude Code session on this machine (~/.claude/projects): per session, by model,
day and prompt size. Files are read once, then only new lines; each message id counts once. */

use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/** Prompt sizes that some models price higher: up to 100k, up to 200k, more. */
fn size_class(prompt: u64) -> u8 {
    match prompt {
        0..=100_000 => 0,
        100_001..=200_000 => 1,
        _ => 2,
    }
}

/** Tokens: input, output, cache writes (5 minute), cache writes (1 hour), cache reads. */
type Tokens = [u64; 5];

#[derive(Default)]
struct FileUsage {
    offset: u64,
    seen: HashSet<String>,
    /** (model, day, size class) -> tokens */
    totals: HashMap<(String, String, u8), Tokens>,
    dir: Option<String>,
    last: Option<String>,
}

static FILES: Mutex<Option<HashMap<PathBuf, FileUsage>>> = Mutex::new(None);

fn read_new(path: &Path, f: &mut FileUsage) {
    let Ok(meta) = std::fs::metadata(path) else { return };
    if meta.len() < f.offset {
        *f = FileUsage::default();
    }
    if meta.len() == f.offset {
        return;
    }
    let Ok(mut file) = std::fs::File::open(path) else { return };
    let mut buf = Vec::new();
    if file.seek(SeekFrom::Start(f.offset)).is_err() || file.read_to_end(&mut buf).is_err() {
        return;
    }
    let Some(end) = buf.iter().rposition(|b| *b == b'\n') else { return };
    f.offset += end as u64 + 1;
    for line in buf[..end].split(|b| *b == b'\n') {
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
        if !id.is_empty() && !f.seen.insert(id) {
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
        let day = ts.get(..10).unwrap_or("").to_string();
        if !ts.is_empty() {
            f.last = Some(ts.to_string());
        }
        let t = f.totals.entry((model, day, size_class(input + write + read))).or_default();
        t[0] += input;
        t[1] += output;
        t[2] += write - hour;
        t[3] += hour;
        t[4] += read;
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/** Every session's usage (subagent files count toward their session): `{host, sessions: [{id, dir, last,
tokens: [[model, day, size_class, input, output, cache_write_5m, cache_write_1h, cache_read], ...]}]}`. */
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
    let mut sessions: HashMap<String, (Option<String>, Option<String>, Vec<Value>)> = HashMap::new();
    for (id, p) in paths {
        let f = files.entry(p.clone()).or_default();
        read_new(&p, f);
        let s = sessions.entry(id).or_default();
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
            .map(|(id, (dir, last, tokens))| json!({"id": id, "dir": dir, "last": last, "tokens": tokens}))
            .collect::<Vec<_>>(),
    })
}
