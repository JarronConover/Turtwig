/*! A worktree's changes as unified diff text: against HEAD (uncommitted) or against where the branch
left the default branch (everything on the branch). Untracked files are included as additions. */

use crate::util;
use serde_json::{json, Value};
use std::time::Duration;

const MAX_BYTES: usize = 3_000_000;
const MAX_UNTRACKED: usize = 200;

fn git(path: &str, args: &[&str]) -> util::Output {
    let mut all = vec!["-C", path];
    all.extend_from_slice(args);
    util::run("git", &all, &[("GIT_PAGER", "cat")], None, Duration::from_secs(30))
}

fn default_branch(path: &str) -> String {
    let head = git(path, &["rev-parse", "--abbrev-ref", "origin/HEAD"]);
    let name = head.stdout.trim();
    if head.ok && !name.is_empty() && name != "origin/HEAD" {
        return name.to_string();
    }
    "origin/main".into()
}

pub fn worktree_diff(path: &str, base: Option<&str>) -> Result<Value, String> {
    let (from, label) = match base.unwrap_or("uncommitted") {
        "branch" => {
            let target = default_branch(path);
            let mb = git(path, &["merge-base", "HEAD", &target]);
            if !mb.ok {
                return Err(format!("no merge base with {target}"));
            }
            (mb.stdout.trim().to_string(), format!("since {}", target.trim_start_matches("origin/")))
        }
        _ => ("HEAD".to_string(), "uncommitted".to_string()),
    };
    let d = git(path, &["diff", &from, "--no-color", "--no-ext-diff", "-M"]);
    if !d.ok && d.stdout.is_empty() {
        return Err(d.stderr.lines().last().unwrap_or("git diff failed").to_string());
    }
    let mut text = d.stdout;
    let untracked = git(path, &["ls-files", "--others", "--exclude-standard", "-z"]);
    let files: Vec<&str> = untracked.stdout.split('\0').filter(|f| !f.is_empty()).collect();
    for f in files.iter().take(MAX_UNTRACKED) {
        if text.len() > MAX_BYTES {
            break;
        }
        text.push_str(&git(path, &["diff", "--no-index", "--no-color", "--", "/dev/null", f]).stdout);
    }
    let truncated = text.len() > MAX_BYTES || files.len() > MAX_UNTRACKED;
    if text.len() > MAX_BYTES {
        let mut cut = MAX_BYTES;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
    }
    Ok(json!({"diff": text, "base": label, "from": from, "truncated": truncated}))
}
