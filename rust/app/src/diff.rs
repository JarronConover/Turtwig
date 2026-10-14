/*! Unified diff text parsed into files and flattened into fixed-height rows for a virtual list. */

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Add,
    Del,
    Ctx,
    Meta,
}

#[derive(Clone)]
pub struct Line {
    pub kind: Kind,
    pub old: Option<u32>,
    pub new: Option<u32>,
    pub text: String,
}

#[derive(Clone)]
pub struct Hunk {
    pub header: String,
    pub lines: Vec<Line>,
}

#[derive(Clone)]
pub struct File {
    pub path: String,
    pub old_path: Option<String>,
    pub status: char,
    pub added: usize,
    pub removed: usize,
    pub binary: bool,
    pub hunks: Vec<Hunk>,
}

pub fn parse(text: &str) -> Vec<File> {
    let mut files: Vec<File> = vec![];
    let (mut old, mut new) = (0u32, 0u32);
    for raw in text.lines() {
        if let Some(rest) = raw.strip_prefix("diff --git ") {
            let path = rest.rsplit_once(" b/").map(|(_, b)| b.to_string()).unwrap_or_else(|| rest.to_string());
            files.push(File { path, old_path: None, status: 'M', added: 0, removed: 0, binary: false, hunks: vec![] });
            continue;
        }
        let Some(f) = files.last_mut() else { continue };
        if f.hunks.is_empty() {
            if raw.starts_with("new file mode") {
                f.status = 'A';
            } else if raw.starts_with("deleted file mode") {
                f.status = 'D';
            } else if let Some(from) = raw.strip_prefix("rename from ") {
                f.status = 'R';
                f.old_path = Some(from.to_string());
            } else if raw.starts_with("Binary files") {
                f.binary = true;
            }
            if !raw.starts_with("@@") {
                continue;
            }
        }
        if let Some(rest) = raw.strip_prefix("@@ ") {
            let mut nums = rest.split(' ');
            let start = |s: Option<&str>| s.and_then(|x| x[1..].split(',').next()).and_then(|n| n.parse::<u32>().ok()).unwrap_or(1);
            old = start(nums.next());
            new = start(nums.next());
            f.hunks.push(Hunk { header: raw.to_string(), lines: vec![] });
            continue;
        }
        let Some(h) = f.hunks.last_mut() else { continue };
        let (kind, text) = match raw.chars().next() {
            Some('+') => (Kind::Add, &raw[1..]),
            Some('-') => (Kind::Del, &raw[1..]),
            Some('\\') => (Kind::Meta, raw),
            Some(' ') => (Kind::Ctx, &raw[1..]),
            _ => (Kind::Ctx, raw),
        };
        let line = match kind {
            Kind::Add => {
                f.added += 1;
                new += 1;
                Line { kind, old: None, new: Some(new - 1), text: text.into() }
            }
            Kind::Del => {
                f.removed += 1;
                old += 1;
                Line { kind, old: Some(old - 1), new: None, text: text.into() }
            }
            Kind::Ctx => {
                old += 1;
                new += 1;
                Line { kind, old: Some(old - 1), new: Some(new - 1), text: text.into() }
            }
            Kind::Meta => Line { kind, old: None, new: None, text: text.into() },
        };
        h.lines.push(line);
    }
    files
}

pub enum Row {
    File(usize),
    Hunk(String),
    Line { file: usize, line: Line },
    Note(String),
}

pub const MAX_LINES_PER_FILE: usize = 4000;

/** Rows for the list plus the row where each file starts; collapsed files show only their header. */
pub fn rows(files: &[File], collapsed: &std::collections::HashSet<String>) -> (Vec<Row>, Vec<usize>, usize) {
    let (mut rows, mut starts, mut widest, mut widest_len) = (vec![], vec![], 0, 0);
    for (i, f) in files.iter().enumerate() {
        starts.push(rows.len());
        rows.push(Row::File(i));
        if collapsed.contains(&f.path) {
            continue;
        }
        if f.binary {
            rows.push(Row::Note("Binary file".into()));
            continue;
        }
        let mut shown = 0;
        for h in &f.hunks {
            rows.push(Row::Hunk(h.header.clone()));
            for l in &h.lines {
                if shown == MAX_LINES_PER_FILE {
                    break;
                }
                shown += 1;
                if l.text.len() > widest_len {
                    widest_len = l.text.len();
                    widest = rows.len();
                }
                rows.push(Row::Line { file: i, line: l.clone() });
            }
        }
        let total: usize = f.hunks.iter().map(|h| h.lines.len()).sum();
        if total > shown {
            rows.push(Row::Note(format!("{} more lines; open the file to see them", total - shown)));
        }
    }
    (rows, starts, widest)
}
