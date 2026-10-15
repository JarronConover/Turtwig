/*! What the composer offers after a trigger character: slash commands (built in, from settings) and the
skills and commands in Claude Code's folders, for the chat's worktree. */

use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub description: String,
    pub kind: String,
    /** Where it comes from: "built-in", "project" (the repo's own), or "personal" (your ~ folders). */
    pub source: String,
}

thread_local! {
    static CACHE: std::cell::RefCell<HashMap<String, (Instant, Vec<Entry>)>> = std::cell::RefCell::new(HashMap::new());
}

/** `name:` and `description:` from a Markdown file's YAML front matter (folded lines joined). */
fn front_matter(text: &str) -> (Option<String>, Option<String>) {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return (None, text.lines().find(|l| !l.trim().is_empty()).map(|l| l.trim_start_matches('#').trim().to_string()));
    }
    let (mut name, mut desc, mut in_desc) = (None, String::new(), false);
    for l in lines {
        if l.trim() == "---" {
            break;
        }
        if let Some(v) = l.strip_prefix("name:") {
            name = Some(v.trim().trim_matches('"').to_string());
            in_desc = false;
        } else if let Some(v) = l.strip_prefix("description:") {
            desc = v.trim().to_string();
            in_desc = true;
        } else if in_desc && l.starts_with(' ') {
            desc.push(' ');
            desc.push_str(l.trim());
        } else {
            in_desc = false;
        }
    }
    let desc = desc.trim().trim_matches(|c| c == '"' || c == '>' || c == '|').trim().to_string();
    (name, (!desc.is_empty()).then_some(desc))
}

fn scan(dir: &Path, kind: &str, source: &str, out: &mut Vec<Entry>) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    for e in read.flatten() {
        let p = e.path();
        let file_name = e.file_name().to_string_lossy().into_owned();
        if file_name.starts_with('_') || file_name.starts_with('.') {
            continue;
        }
        let (md, fallback) = if p.is_dir() {
            (p.join("SKILL.md"), file_name.clone())
        } else if p.extension().is_some_and(|x| x == "md") {
            (p.clone(), file_name.trim_end_matches(".md").to_string())
        } else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(&md) else { continue };
        let (name, description) = front_matter(&text);
        let name = name.filter(|n| !n.is_empty()).unwrap_or(fallback);
        if !out.iter().any(|x| x.name == name) {
            out.push(Entry { name, description: description.unwrap_or_default(), kind: kind.to_string(), source: source.to_string() });
        }
    }
}

/** Everything `/` offers in a worktree: settings `chat.slash_commands`, then the folders in
`chat.skill_dirs` (`{worktree}` is the worktree; a box worktree reads this Mac's checkout of the repo). */
pub fn entries(settings: &crate::settings::Settings, worktree: Option<&str>, local_repo: Option<&str>) -> Vec<Entry> {
    let key = format!("{}|{}", worktree.unwrap_or(""), local_repo.unwrap_or(""));
    if let Some(hit) = CACHE.with(|c| c.borrow().get(&key).filter(|(t, _)| t.elapsed() < Duration::from_secs(20)).map(|(_, e)| e.clone())) {
        return hit;
    }
    let mut out: Vec<Entry> = vec![];
    for c in settings.chat.get("slash_commands").and_then(Value::as_array).into_iter().flatten() {
        if let Some(name) = c.get("name").and_then(Value::as_str) {
            out.push(Entry {
                name: name.trim_start_matches('/').to_string(),
                description: c.get("description").and_then(Value::as_str).unwrap_or("").to_string(),
                kind: "command".into(),
                source: "built-in".into(),
            });
        }
    }
    let base = local_repo.or(worktree).unwrap_or("");
    let dirs: Vec<String> = settings
        .chat
        .get("skill_dirs")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default();
    for d in dirs {
        if d.contains("{worktree}") && base.is_empty() {
            continue;
        }
        let path = PathBuf::from(crate::script::expand(&d.replace("{worktree}", base)));
        let kind = if d.contains("commands") { "command" } else { "skill" };
        scan(&path, kind, if d.contains("{worktree}") { "project" } else { "personal" }, &mut out);
    }
    CACHE.with(|c| c.borrow_mut().insert(key, (Instant::now(), out.clone())));
    out
}

/** The trigger word being typed at the end of `text`: ("/", "ship") for "please /ship". */
pub fn token(text: &str, triggers: &[String]) -> Option<(String, String)> {
    let last = text.rsplit(|c: char| c.is_whitespace()).next().unwrap_or("");
    if text.ends_with(char::is_whitespace) {
        return None;
    }
    triggers.iter().find(|t| last.starts_with(t.as_str())).map(|t| (t.clone(), last[t.len()..].to_string()))
}

/** Entries matching `query`: name prefix first, then name contains, then description contains. */
pub fn filter(all: &[Entry], query: &str, limit: usize) -> Vec<Entry> {
    let q = query.to_lowercase();
    let mut scored: Vec<(u8, &Entry)> = all
        .iter()
        .filter_map(|e| {
            let n = e.name.to_lowercase();
            if n.starts_with(&q) {
                Some((0, e))
            } else if n.contains(&q) {
                Some((1, e))
            } else if !q.is_empty() && e.description.to_lowercase().contains(&q) {
                Some((2, e))
            } else {
                None
            }
        })
        .collect();
    scored.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.name.cmp(&b.1.name)));
    scored.into_iter().take(limit).map(|(_, e)| e.clone()).collect()
}

pub fn to_json(items: &[Entry]) -> Value {
    Value::Array(items.iter().map(|e| json!({"name": e.name, "description": e.description, "kind": e.kind, "source": e.source})).collect())
}

#[cfg(test)]
mod tests {
    #[test]
    fn finds_the_trigger_word() {
        let t = vec!["/".to_string()];
        assert_eq!(super::token("/shi", &t), Some(("/".into(), "shi".into())));
        assert_eq!(super::token("run /loop", &t), Some(("/".into(), "loop".into())));
        assert_eq!(super::token("/loop ", &t), None);
        assert_eq!(super::token("path/to", &t), None);
    }

    #[test]
    fn reads_folded_descriptions() {
        let (n, d) = super::front_matter("---\nname: x\ndescription:\n  \"one\n  two\"\nother: 1\n---\nbody");
        assert_eq!(n.as_deref(), Some("x"));
        assert_eq!(d.as_deref(), Some("one two"));
    }
}
