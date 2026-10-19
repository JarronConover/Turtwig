/*! A chat's own title as its agent keeps it (`agents.<agent>.title`): Claude Code's records in the transcript
(`custom-title` from /rename, then `ai-title`), Codex's thread name in its session index. Files are read
from where the last look stopped, so a long transcript is scanned once. */

use serde_json::Value;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/** Per file: how far it was read, and the latest value seen for each record kind (by key). */
#[derive(Default)]
struct Scan {
    offset: u64,
    latest: HashMap<String, String>,
}

static SCANS: Mutex<Option<HashMap<PathBuf, Scan>>> = Mutex::new(None);

/** Reads `path` past what was read before; `keyed` turns one line into (key, value). */
fn scan(path: &Path, keyed: &dyn Fn(&Value) -> Option<(String, String)>, needle: &[&str]) -> HashMap<String, String> {
    let mut guard = SCANS.lock().unwrap();
    let scans = guard.get_or_insert_with(HashMap::new);
    let s = scans.entry(path.to_path_buf()).or_default();
    let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if len < s.offset {
        *s = Scan::default();
    }
    if len > s.offset {
        if let Ok(mut f) = std::fs::File::open(path) {
            let mut buf = Vec::new();
            if f.seek(SeekFrom::Start(s.offset)).is_ok() && f.read_to_end(&mut buf).is_ok() {
                // Only whole lines; a line still being written is read next time.
                let end = buf.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
                for line in buf[..end].split(|b| *b == b'\n') {
                    if line.is_empty() || !needle.iter().any(|n| line.windows(n.len()).any(|w| w == n.as_bytes())) {
                        continue;
                    }
                    if let Some((k, v)) = serde_json::from_slice::<Value>(line).ok().as_ref().and_then(keyed) {
                        s.latest.insert(k, v);
                    }
                }
                s.offset += end as u64;
            }
        }
    }
    s.latest.clone()
}

/** The chat's title by its agent's `title` spec: `{records: [{type, field}]}` (the first record kind that
has one wins; each kind's latest) read from the transcript, or `{index, id, field}` (a JSON-lines file
naming sessions). */
pub fn title(chat: &Value, agent: &str, transcript: Option<&Path>) -> Option<String> {
    let spec = crate::agents::get(agent, "title")?;
    let clean = |t: &str| Some(t.trim().to_string()).filter(|t| !t.is_empty());
    if let Some(records) = spec.get("records").and_then(Value::as_array) {
        let kinds: Vec<(String, String)> = records
            .iter()
            .filter_map(|r| Some((r.get("type")?.as_str()?.to_string(), r.get("field")?.as_str()?.to_string())))
            .collect();
        let needles: Vec<String> = kinds.iter().map(|(t, _)| t.clone()).collect();
        let refs: Vec<&str> = needles.iter().map(String::as_str).collect();
        let kinds2 = kinds.clone();
        let latest = scan(transcript?, &move |v| {
            let t = v.get("type")?.as_str()?;
            let (_, field) = kinds2.iter().find(|(k, _)| k == t)?;
            Some((t.to_string(), v.get(field)?.as_str()?.to_string()))
        }, &refs);
        return kinds.iter().find_map(|(t, _)| latest.get(t).and_then(|v| clean(v)));
    }
    let index = crate::agents::expand(spec.get("index")?.as_str()?);
    let id_key = spec.get("id").and_then(Value::as_str).unwrap_or("id").to_string();
    let field = spec.get("field").and_then(Value::as_str).unwrap_or("thread_name").to_string();
    let session = crate::agents::session_key(chat)?;
    let latest = scan(&index, &move |v| Some((v.get(&id_key)?.as_str()?.to_string(), v.get(&field)?.as_str()?.to_string())), &["\""]);
    latest.get(&session).and_then(|v| clean(v))
}

/** Whether the chat's name may follow its agent's title (see bin/chat's auto_named). Orchestrators
(`auto_title_keep`: these kinds, or a name or folder matching) keep theirs: their title drifts to whichever
leg they're on. */
pub fn auto_named(chat: &Value) -> bool {
    let keep = crate::config::get("auto_title_keep").unwrap_or_else(|| serde_json::json!({"kinds": ["orchestrator"], "match": "(?i)ship.feature|orchestrat"}));
    let kind = chat.get("kind").and_then(Value::as_str).unwrap_or("");
    if keep["kinds"].as_array().is_some_and(|k| k.iter().any(|x| x.as_str() == Some(kind))) {
        return false;
    }
    let text = format!("{} {}", chat.get("name").and_then(Value::as_str).unwrap_or(""), chat.get("dir").and_then(Value::as_str).unwrap_or(""));
    if keep["match"].as_str().filter(|m| !m.is_empty()).and_then(|m| regex::Regex::new(m).ok()).is_some_and(|re| re.is_match(&text)) {
        return false;
    }
    match chat.get("name_source").and_then(Value::as_str) {
        Some(source) => source == "auto",
        None => {
            chat.get("imported_from").is_some_and(|v| !v.is_null())
                || chat.get("name").and_then(Value::as_str) == Some(crate::config::text("default_chat_name", "New chat").as_str())
        }
    }
}

static SAVED: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

/** Records a new title in the registry (`bin/chat rename --auto`, which re-checks the name may follow it),
once per title, off the model loop. */
pub fn persist(id: &str, title: &str) {
    {
        let mut guard = SAVED.lock().unwrap();
        let saved = guard.get_or_insert_with(HashMap::new);
        if saved.get(id).map(String::as_str) == Some(title) {
            return;
        }
        saved.insert(id.to_string(), title.to_string());
    }
    let (id, title) = (id.to_string(), title.to_string());
    std::thread::spawn(move || {
        let tool = crate::util::agent_box_dir().join("bin").join("chat");
        crate::util::run(&tool.to_string_lossy(), &["rename", "--auto", &id, &title], &[], None, std::time::Duration::from_secs(60));
    });
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn a_custom_title_beats_the_latest_ai_title_and_appended_lines_are_read() {
        let dir = std::env::temp_dir().join(format!("abx-titles-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.jsonl");
        std::fs::write(&p, "{\"type\":\"user\",\"message\":{}}\n{\"type\":\"ai-title\",\"aiTitle\":\"First idea\"}\n").unwrap();
        crate::config::set(json!({"agents": {"claude": {"title": {"records": [{"type": "custom-title", "field": "customTitle"}, {"type": "ai-title", "field": "aiTitle"}]}}}}));
        let chat = json!({"id": "x"});
        assert_eq!(super::title(&chat, "claude", Some(&p)).as_deref(), Some("First idea"));
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        use std::io::Write;
        writeln!(f, "{{\"type\":\"ai-title\",\"aiTitle\":\"Better idea\"}}").unwrap();
        assert_eq!(super::title(&chat, "claude", Some(&p)).as_deref(), Some("Better idea"));
        writeln!(f, "{{\"type\":\"custom-title\",\"customTitle\":\"Mine\"}}").unwrap();
        writeln!(f, "{{\"type\":\"ai-title\",\"aiTitle\":\"Later idea\"}}").unwrap();
        assert_eq!(super::title(&chat, "claude", Some(&p)).as_deref(), Some("Mine"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_unchosen_names_follow_titles() {
        assert!(super::auto_named(&json!({"name_source": "auto", "name": "x"})));
        assert!(!super::auto_named(&json!({"name_source": "user", "name": "New chat"})));
        assert!(!super::auto_named(&json!({"name_source": "creator", "name": "New chat"})));
        assert!(super::auto_named(&json!({"name": "New chat"})));
        assert!(super::auto_named(&json!({"name": "From SE", "imported_from": "super.engineering"})));
        assert!(!super::auto_named(&json!({"name": "legacy leg"})));
        assert!(!super::auto_named(&json!({"name": "New chat", "kind": "orchestrator", "name_source": "auto"})));
        assert!(!super::auto_named(&json!({"name": "Ship Feature AIOPSUP-1", "imported_from": "super.engineering", "kind": "feature"})));
    }
}
