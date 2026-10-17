/*! A worktree's changes as unified diff text: against HEAD (uncommitted) or against where the branch
left the default branch (everything on the branch). Untracked files are included as additions. */

use crate::{config, util};
use serde_json::{json, Value};

fn git(path: &str, args: &[&str]) -> util::Output {
    let mut all = vec!["-C", path];
    all.extend_from_slice(args);
    util::run("git", &all, &[("GIT_PAGER", "cat")], None, config::secs("git_timeout_secs", 30))
}

fn default_branch(path: &str) -> String {
    let head = git(path, &["rev-parse", "--abbrev-ref", "origin/HEAD"]);
    let name = head.stdout.trim();
    if head.ok && !name.is_empty() && name != "origin/HEAD" {
        return name.to_string();
    }
    config::get("diff_default_base")
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("origin/{}", config::text("default_base", "main")))
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
    // `diff_max_bytes` of diff text, `diff_max_untracked` new files, and untracked files only with
    // `diff_include_untracked`.
    let (max_bytes, max_untracked) = (config::count("diff_max_bytes", 3_000_000), config::count("diff_max_untracked", 200));
    let mut text = d.stdout;
    let untracked = if config::flag("diff_include_untracked", true) {
        git(path, &["ls-files", "--others", "--exclude-standard", "-z"]).stdout
    } else {
        String::new()
    };
    let files: Vec<&str> = untracked.split('\0').filter(|f| !f.is_empty()).collect();
    for f in files.iter().take(max_untracked) {
        if text.len() > max_bytes {
            break;
        }
        text.push_str(&git(path, &["diff", "--no-index", "--no-color", "--", "/dev/null", f]).stdout);
    }
    let truncated = text.len() > max_bytes || files.len() > max_untracked;
    if text.len() > max_bytes {
        let mut cut = max_bytes;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
    }
    Ok(json!({"diff": text, "base": label, "from": from, "truncated": truncated}))
}
