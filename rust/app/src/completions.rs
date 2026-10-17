/*! What the composer offers after a trigger character: slash commands (built in, from settings), the
skills and commands in Claude Code's folders, and the worktree's files, per `chat.completion_sources`. */

use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

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

fn scan(dir: &Path, kind: &str, source: &str, skip: &[String], out: &mut Vec<Entry>) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    for e in read.flatten() {
        let p = e.path();
        let file_name = e.file_name().to_string_lossy().into_owned();
        if skip.iter().any(|x| file_name.starts_with(x.as_str())) {
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

/** A `chat.skill_dirs` entry as (path, kind, source): a string guesses kind and source from the path;
an object is `{path, kind?, source?}`. */
fn dir_spec(v: &Value) -> Option<(String, String, String)> {
    let path = v.as_str().or_else(|| v.get("path").and_then(Value::as_str))?.to_string();
    let kind = v.get("kind").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| if path.contains("commands") { "command" } else { "skill" }.into());
    let source = v.get("source").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| if path.contains("{worktree}") { "project" } else { "personal" }.into());
    Some((path, kind, source))
}

/** What `chat.completion_sources` names for a trigger: commands, skills, files. Unlisted triggers get
commands and skills. */
fn sources(setting: Option<&Value>, trigger: &str) -> Vec<String> {
    setting
        .and_then(|s| s.get(trigger))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_else(|| vec!["commands".into(), "skills".into()])
}

/** The worktree's files by relative path, skipping hidden and build folders, up to `max`. */
fn files(root: &Path, skip: &[String], max: usize, out: &mut Vec<Entry>) {
    let mut stack = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else { continue };
        let mut kids: Vec<_> = read.flatten().collect();
        kids.sort_by_key(|e| e.file_name());
        for e in kids {
            let name = e.file_name().to_string_lossy().into_owned();
            if skip.iter().any(|x| name.starts_with(x.as_str())) || matches!(name.as_str(), "node_modules" | "target" | ".git") {
                continue;
            }
            let p = e.path();
            if p.is_dir() {
                if depth < 8 {
                    stack.push((p, depth + 1));
                }
            } else if out.len() < max {
                let rel = p.strip_prefix(root).unwrap_or(&p).to_string_lossy().into_owned();
                out.push(Entry { name: rel, description: String::new(), kind: "file".into(), source: "project".into() });
            } else {
                return;
            }
        }
    }
}

/** Everything a trigger offers in a worktree, by its `chat.completion_sources`: commands (settings
`chat.slash_commands`), skills (the folders in `chat.skill_dirs`; `{worktree}` is the worktree, and a box
worktree reads this Mac's checkout of the repo) and files. */
pub fn entries(settings: &crate::settings::Settings, trigger: &str, worktree: Option<&str>, local_repo: Option<&str>) -> Vec<Entry> {
    let key = format!("{trigger}|{}|{}", worktree.unwrap_or(""), local_repo.unwrap_or(""));
    if let Some(hit) = CACHE.with(|c| c.borrow().get(&key).filter(|(t, _)| t.elapsed().as_secs_f64() < settings.chat.get("completion_cache_secs").and_then(Value::as_f64).unwrap_or(20.)).map(|(_, e)| e.clone())) {
        return hit;
    }
    let want = sources(settings.chat.get("completion_sources"), trigger);
    let skip: Vec<String> = settings
        .chat
        .get("completion_skip_prefixes")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_else(|| vec!["_".into(), ".".into()]);
    let mut out: Vec<Entry> = vec![];
    let commands = if want.iter().any(|w| w == "commands") { settings.chat.get("slash_commands").and_then(Value::as_array) } else { None };
    for c in commands.into_iter().flatten() {
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
    let dirs: Vec<(String, String, String)> = if want.iter().any(|w| w == "skills") {
        settings.chat.get("skill_dirs").and_then(Value::as_array).map(|a| a.iter().filter_map(dir_spec).collect()).unwrap_or_default()
    } else {
        vec![]
    };
    for (d, kind, source) in dirs {
        if d.contains("{worktree}") && base.is_empty() {
            continue;
        }
        let path = PathBuf::from(crate::script::expand(&d.replace("{worktree}", base)));
        scan(&path, &kind, &source, &skip, &mut out);
    }
    if want.iter().any(|w| w == "files") && !base.is_empty() {
        let max = settings.chat.get("completion_files_max").and_then(Value::as_u64).unwrap_or(5000) as usize;
        files(Path::new(&crate::script::expand(base)), &skip, max, &mut out);
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

/** How well `q`'s letters appear in order in `name` (lower is better); None when they don't. */
fn fuzzy(name: &str, q: &str) -> Option<u32> {
    let mut gaps = 0u32;
    let mut chars = name.chars().enumerate();
    let mut last: Option<usize> = None;
    for qc in q.chars() {
        let (i, _) = chars.find(|(_, c)| *c == qc)?;
        gaps += last.map(|l| (i - l - 1) as u32).unwrap_or(i as u32);
        last = Some(i);
    }
    Some(gaps)
}

/** Entries matching `query`, by `chat.completion_rank`: "fuzzy" (letters in order, tightest first), else
name prefix first, then name contains, then description contains. */
pub fn filter(all: &[Entry], query: &str, limit: usize, rank: &str) -> Vec<Entry> {
    let q = query.to_lowercase();
    if rank == "fuzzy" {
        let mut scored: Vec<(u32, &Entry)> = all.iter().filter_map(|e| fuzzy(&e.name.to_lowercase(), &q).map(|s| (s, e))).collect();
        scored.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.name.len().cmp(&b.1.name.len())).then(a.1.name.cmp(&b.1.name)));
        return scored.into_iter().take(limit).map(|(_, e)| e.clone()).collect();
    }
    let mut scored: Vec<(u32, &Entry)> = all
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

    fn entry(name: &str, description: &str) -> super::Entry {
        super::Entry { name: name.into(), description: description.into(), kind: "skill".into(), source: "personal".into() }
    }

    #[test]
    fn ranks_by_setting() {
        let all = vec![entry("review", ""), entry("prereview", ""), entry("loop", "review in a loop"), entry("rv-tools", "")];
        let names = |v: Vec<super::Entry>| v.into_iter().map(|e| e.name).collect::<Vec<_>>();
        assert_eq!(names(super::filter(&all, "review", 10, "")), ["review", "prereview", "loop"]);
        assert_eq!(names(super::filter(&all, "rv", 10, "fuzzy")), ["rv-tools", "review", "prereview"]);
    }

    #[test]
    fn skill_dirs_and_sources() {
        use serde_json::json;
        assert_eq!(super::dir_spec(&json!("{worktree}/.claude/commands")), Some(("{worktree}/.claude/commands".into(), "command".into(), "project".into())));
        assert_eq!(super::dir_spec(&json!({"path": "~/x", "kind": "command"})), Some(("~/x".into(), "command".into(), "personal".into())));
        assert_eq!(super::sources(Some(&json!({"@": ["files"]})), "@"), ["files"]);
        assert_eq!(super::sources(Some(&json!({"@": ["files"]})), "/"), ["commands", "skills"]);
    }

    #[test]
    fn reads_folded_descriptions() {
        let (n, d) = super::front_matter("---\nname: x\ndescription:\n  \"one\n  two\"\nother: 1\n---\nbody");
        assert_eq!(n.as_deref(), Some("x"));
        assert_eq!(d.as_deref(), Some("one two"));
    }
}
