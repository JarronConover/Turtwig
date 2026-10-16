/*! The `diff` node: a worktree's changes as a file list beside a virtualized, horizontally scrolling
diff. Clicking a file jumps to it, a file header collapses it, a line number opens that line in Cursor. */

use super::style;
use crate::diff::{self, Kind, Row};
use crate::workspace::Workspace;
use agent_box_protocol::Call;
use gpui_kit::component::Icon;
use gpui_kit::*;
use serde_json::{json, Map, Value};
use std::rc::Rc;
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct DiffData {
    pub files: Rc<Vec<diff::File>>,
    pub base_label: String,
    pub truncated: bool,
    pub error: Option<String>,
    pub loading: bool,
    pub stale: bool,
    pub fetched: Option<Instant>,
}

impl Workspace {
    /** How long a changed diff waits before fetching again (`diff.refresh_ms`). */
    fn diff_refresh(&self) -> Duration {
        Duration::from_millis(self.settings.num("diff.refresh_ms", 2000.).max(100.) as u64)
    }

    pub(crate) fn diff_base(&self) -> String {
        self.state.get("diff_base").and_then(Value::as_str).unwrap_or("uncommitted").to_string()
    }

    /** Summary for expressions: `.selected.diff`. */
    pub(crate) fn diff_summary(&self, path: &str) -> Value {
        let key = format!("{path}|{}", self.diff_base());
        match self.diffs.get(&key) {
            None => json!({"loaded": false, "base": self.diff_base()}),
            Some(d) => json!({
                "loaded": d.fetched.is_some(), "loading": d.loading, "error": d.error, "base": self.diff_base(),
                "base_label": d.base_label, "truncated": d.truncated, "files": d.files.len(),
                "added": d.files.iter().map(|f| f.added).sum::<usize>(),
                "removed": d.files.iter().map(|f| f.removed).sum::<usize>(),
            }),
        }
    }

    pub(crate) fn mark_diffs_stale(&mut self) {
        for d in self.diffs.values_mut() {
            d.stale = true;
        }
    }

    pub(crate) fn ensure_diff(&mut self, path: &str, base: &str, cx: &mut Context<Self>) {
        let key = format!("{path}|{base}");
        let refresh = self.diff_refresh();
        let d = self.diffs.entry(key.clone()).or_default();
        let due = d.fetched.is_none() || (d.stale && d.fetched.is_some_and(|t| t.elapsed() > refresh));
        if d.loading || !self.connected {
            return;
        }
        if !due {
            if d.stale {
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(refresh).await;
                    this.update(cx, |_, cx| cx.notify()).ok();
                })
                .detach();
            }
            return;
        }
        d.loading = true;
        d.stale = false;
        let call = Call::WorktreeDiff { path: path.to_string(), base: Some(base.to_string()) };
        let remote = self.route(&call);
        cx.spawn(async move |this, cx| {
            let res = remote.call(call).await;
            let parsed = cx
                .background_spawn(async move {
                    res.map(|v| {
                        let files = diff::parse(v["diff"].as_str().unwrap_or(""));
                        (files, v["base"].as_str().unwrap_or("").to_string(), v["truncated"].as_bool().unwrap_or(false))
                    })
                })
                .await;
            this.update(cx, |ws, cx| {
                let d = ws.diffs.entry(key).or_default();
                d.loading = false;
                d.fetched = Some(Instant::now());
                match parsed {
                    Ok((files, label, truncated)) => {
                        d.files = Rc::new(files);
                        d.base_label = label;
                        d.truncated = truncated;
                        d.error = None;
                    }
                    Err(e) => d.error = Some(e.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn diff_view(&mut self, props: &Map<String, Value>, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.settings.theme.clone();
        let ds = self.settings.diff.clone();
        let st = |k: &str| ds.get(k).and_then(Value::as_object).cloned().unwrap_or_default();
        let path = props.get("path").and_then(Value::as_str).map(str::to_string).or_else(|| self.selected.clone());
        let Some(path) = path else {
            return style::apply(div().size_full().flex().items_center().justify_center(), &st("empty"), &theme).child(self.settings.text("diff.select_worktree", "Select a worktree")).into_any_element();
        };
        let (no_changes, loading) = (self.settings.text("diff.no_changes", "No changes"), self.settings.text("diff.loading", "Loading changes…"));
        let base = props.get("base").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| self.diff_base());
        self.ensure_diff(&path, &base, cx);
        let key = format!("{path}|{base}");
        let data = self.diffs.get(&key);
        let (files, error, loaded) = match data {
            Some(d) => (d.files.clone(), d.error.clone(), d.fetched.is_some()),
            None => (Rc::new(vec![]), None, false),
        };
        if let Some(e) = error.filter(|_| files.is_empty()) {
            return style::apply(div().size_full().flex().items_center().justify_center(), &st("empty"), &theme).child(e).into_any_element();
        }
        if files.is_empty() {
            let msg = if loaded { no_changes } else { loading };
            return style::apply(div().size_full().flex().items_center().justify_center(), &st("empty"), &theme).child(msg).into_any_element();
        }
        let limits = self.diff_limits();
        let (rows, starts, widest) = diff::rows(&files, &self.diff_collapsed, &limits);
        let rows = Rc::new(rows);
        let handle = self.diff_scroll.clone();
        let view = cx.entity().downgrade();
        let editor = self.diff_link_template(&path);
        let host = self.diff_link_host(&path);
        let machine = self.host_of_path(&path);
        let colors = ds.get("colors").and_then(Value::as_object).cloned().unwrap_or_default();
        // `diff.colors.NAME` (a color or @token) overrides the theme token each part uses by default.
        let c = |k: &str, fallback: &str| {
            let v = colors.get(k).cloned().unwrap_or_else(|| Value::from(format!("@{}", default_token(k))));
            style::color(&v, &theme).unwrap_or_else(|| crate::settings::parse_color(fallback))
        };
        let (add_bg, del_bg, add_fg, del_fg, muted, hunk_bg, file_bg) = (
            c("add_bg", "#1d3324"),
            c("del_bg", "#3a1e23"),
            c("add", "#3fb950"),
            c("del", "#e5534b"),
            c("muted", "#6c6c70"),
            c("hunk_bg", "#22262e"),
            c("file_bg", "#262628"),
        );
        let mono = theme.get("font_mono").and_then(Value::as_str).unwrap_or("Menlo").to_string();
        let font_size = ds.get("font_size").and_then(Value::as_f64).unwrap_or(12.) as f32;
        let num = |k: &str, d: f64| ds.get(k).and_then(Value::as_f64).unwrap_or(d) as f32;
        let (row_height, gutter_w, sign_w) = (num("row_height", 20.), num("gutter_width", 44.), num("sign_width", 14.));
        let header = ds.get("header").and_then(Value::as_object).cloned().unwrap_or_default();
        let header_font = header.get("font_family").and_then(Value::as_str).unwrap_or(".SystemUIFont").to_string();
        let header_size = header.get("font_size").and_then(Value::as_f64).unwrap_or(12.5) as f32;
        let note_size = num("note_font_size", 12.);
        let row_files = files.clone();
        let collapsed = self.diff_collapsed.clone();
        let geom = self.diff_geom.clone();
        *geom.borrow_mut() = json!({
            "row_height": row_height,
            "file_rows": files.iter().zip(starts.iter()).map(|(f, r)| json!({"path": f.path, "row": r})).collect::<Vec<_>>(),
            "rows": rows.len(),
        });
        let list = uniform_list("diff-rows", rows.len(), move |range, _, _| {
            if let Value::Object(o) = &mut *geom.borrow_mut() {
                o.insert("visible".into(), json!([range.start, range.end]));
            }
            range
                .map(|ix| {
                    let base = div().h(px(row_height)).min_w_full().flex().flex_row().items_center().whitespace_nowrap();
                    match &rows[ix] {
                        Row::File(i) => {
                            let f = &row_files[*i];
                            let p = f.path.clone();
                            let v = view.clone();
                            let shut = collapsed.contains(&f.path);
                            base.id(("file", ix))
                                .bg(file_bg)
                                .px(px(10.))
                                .gap(px(8.))
                                .cursor_pointer()
                                .font_family(header_font.clone())
                                .text_size(px(header_size))
                                .child(Icon::empty().path(if shut { "icons/chevron-right.svg" } else { "icons/chevron-down.svg" }).size(px(12.)).text_color(muted))
                                .child(div().text_color(status_color(f.status, add_fg, del_fg, muted)).child(f.status.to_string()))
                                .child(f.old_path.as_ref().map(|o| format!("{o} → {}", f.path)).unwrap_or_else(|| f.path.clone()))
                                .child(div().text_color(add_fg).child(format!("+{}", f.added)))
                                .child(div().text_color(del_fg).child(format!("−{}", f.removed)))
                                .on_click(move |_, _, cx| {
                                    let p = p.clone();
                                    let _ = v.update(cx, |ws, cx| {
                                        if !ws.diff_collapsed.remove(&p) {
                                            ws.diff_collapsed.insert(p);
                                        }
                                        cx.notify();
                                    });
                                })
                                .into_any_element()
                        }
                        Row::Hunk(h) => base.bg(hunk_bg).px(px(10.)).text_color(muted).font_family(mono.clone()).text_size(px(font_size)).child(h.clone()).into_any_element(),
                        Row::Note(n) => base.px(px(10.)).text_color(muted).italic().text_size(px(note_size)).child(n.clone()).into_any_element(),
                        Row::Line { file, line } => {
                            let (bg, sign, fg) = match line.kind {
                                Kind::Add => (Some(add_bg), "+", Some(add_fg)),
                                Kind::Del => (Some(del_bg), "−", Some(del_fg)),
                                Kind::Meta => (None, " ", Some(muted)),
                                Kind::Ctx => (None, " ", None),
                            };
                            let num = |n: Option<u32>| div().w(px(gutter_w)).flex_shrink_0().text_right().pr(px(8.)).text_color(muted).child(n.map(|n| n.to_string()).unwrap_or_default());
                            let target = line.new.or(line.old).unwrap_or(1);
                            let url = link(&editor, &host, &machine, &format!("{path}/{}", row_files[*file].path), target);
                            let gutter = div()
                                .id(("gutter", ix))
                                .flex()
                                .flex_row()
                                .cursor_pointer()
                                .hover(|s| s.opacity(0.7))
                                .child(num(line.old))
                                .child(num(line.new))
                                .on_click(move |_, _, cx| cx.open_url(&url));
                            let mut el = base.font_family(mono.clone()).text_size(px(font_size)).child(gutter).child(div().w(px(sign_w)).flex_shrink_0().text_color(fg.unwrap_or(muted)).child(sign));
                            if let Some(bg) = bg {
                                el = el.bg(bg);
                            }
                            el.child(div().pr(px(24.)).child(if line.text.is_empty() { " ".to_string() } else { line.text.replace('\t', "    ") })).into_any_element()
                        }
                    }
                })
                .collect::<Vec<_>>()
        })
        .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
        .with_width_from_item(Some(widest))
        .track_scroll(&handle)
        .size_full();

        let list_style = st("file_list");
        let mut file_list = style::apply(div().id("diff-files").flex().flex_col().overflow_y_scroll(), &list_style, &theme);
        for (i, f) in files.iter().enumerate() {
            let (dir, name) = f.path.rsplit_once('/').map(|(d, n)| (format!("{d}/"), n.to_string())).unwrap_or((String::new(), f.path.clone()));
            let start = starts[i];
            let h = handle.clone();
            file_list = file_list.child(
                style::apply(div().id(("diff-file", i)).flex().flex_row().items_center().cursor_pointer(), &st("file_row"), &theme)
                    .hover(|s| s.bg(c("hover", "#28282b")))
                    .child(div().w(px(sign_w)).flex_shrink_0().text_color(status_color(f.status, add_fg, del_fg, muted)).child(f.status.to_string()))
                    .child(div().flex_1().min_w_0().truncate().child(name).child(div().text_color(muted).text_size(px(11.)).truncate().child(dir)))
                    .child(div().text_size(px(11.)).text_color(add_fg).child(format!("+{}", f.added)))
                    .child(div().text_size(px(11.)).text_color(del_fg).pl(px(4.)).child(format!("−{}", f.removed)))
                    .on_click(move |_, _, _| h.scroll_to_item(start, ScrollStrategy::Top)),
            );
        }
        div()
            .size_full()
            .flex()
            .flex_row()
            .child(file_list)
            .child(div().flex_1().min_w_0().h_full().child(list))
            .into_any_element()
    }
}

impl Workspace {
    fn diff_limits(&self) -> diff::RowLimits {
        let d = diff::RowLimits::default();
        diff::RowLimits {
            max_lines: self.settings.num("diff.max_lines_per_file", d.max_lines as f64).max(1.) as usize,
            more: self.settings.text("diff.more_lines", &d.more),
            binary: self.settings.text("diff.binary", &d.binary),
        }
    }

    /** `diff.open_url` for worktrees on the box, `diff.open_url_local` for ones on this Mac. */
    fn diff_link_template(&self, path: &str) -> String {
        let local = self.host_of_path(path) == "mac";
        let (key, default) = if local { ("open_url_local", "cursor://file{path}:{line}") } else { ("open_url", "cursor://vscode-remote/ssh-remote+{host}{path}:{line}") };
        self.settings.diff.get(key).and_then(Value::as_str).unwrap_or(default).to_string()
    }

    /** `{host}` in a line link: the ssh host for box worktrees, "localhost" for the Mac's own. */
    fn diff_link_host(&self, path: &str) -> String {
        if self.host_of_path(path) == "mac" { "localhost".into() } else { self.settings.connection.ssh_host.clone() }
    }

    /** The diff showing for the selected worktree (its key in `diffs`). */
    fn shown_diff(&self) -> Option<(String, &DiffData)> {
        let path = self.selected.clone()?;
        let key = format!("{path}|{}", self.diff_base());
        self.diffs.get(&key).map(|d| (path, d))
    }

    /** Opens a changed file at `line` (default: its first change) the way the gutter link does. */
    fn open_diff_file(&self, file: &str, line: Option<u32>, cx: &mut Context<Self>) -> bool {
        let Some((root, d)) = self.shown_diff() else { return false };
        let Some(f) = d.files.iter().find(|f| f.path == file) else { return false };
        let first = f.hunks.iter().flat_map(|h| h.lines.iter()).find(|l| !matches!(l.kind, Kind::Ctx | Kind::Meta)).and_then(|l| l.new.or(l.old));
        let url = link(&self.diff_link_template(&root), &self.diff_link_host(&root), &self.host_of_path(&root), &format!("{root}/{}", f.path), line.or(first).unwrap_or(1));
        cx.open_url(&url);
        true
    }
}

fn link(template: &str, host: &str, machine: &str, path: &str, line: u32) -> String {
    template.replace("{host}", host).replace("{machine}", machine).replace("{path}", path).replace("{line}", &line.to_string())
}

/** The theme token a diff part uses unless `diff.colors` names another. */
fn default_token(part: &str) -> &str {
    match part {
        "add_bg" => "diff_add_bg",
        "del_bg" => "diff_del_bg",
        "add" => "diff_add",
        "del" => "diff_del",
        "muted" => "text_faint",
        "hunk_bg" => "diff_hunk_bg",
        "file_bg" => "diff_file_bg",
        other => other,
    }
}

fn status_color(s: char, add: Hsla, del: Hsla, other: Hsla) -> Hsla {
    match s {
        'A' => add,
        'D' => del,
        _ => other,
    }
}

/** Actions this module handles (named in SETTINGS.md with the rest). */
pub(crate) const ACTIONS: &[(&str, &str)] = &[
    ("diff.toggle_file", "{path}: collapse or expand one file in the selected worktree's diff (path relative to the worktree)"),
    ("diff.jump", "{path}: scroll the diff to that file"),
    ("diff.open", "{path, line?}: open the file at that line (default: its first change) via `diff.open_url` / `diff.open_url_local`"),
];

/** Runs `name` if it's one of this module's actions; false otherwise. */
pub(crate) fn dispatch(ws: &mut Workspace, name: &str, a: &Map<String, Value>, _window: &mut Window, cx: &mut Context<Workspace>) -> bool {
    let path = a.get("path").and_then(Value::as_str).map(str::to_string);
    match name {
        "diff.toggle_file" => {
            if let Some(p) = path {
                if !ws.diff_collapsed.remove(&p) {
                    ws.diff_collapsed.insert(p);
                }
                cx.notify();
            }
        }
        "diff.jump" => {
            let Some(p) = path else { return true };
            let limits = ws.diff_limits();
            if let Some((_, d)) = ws.shown_diff() {
                if let Some(i) = d.files.iter().position(|f| f.path == p) {
                    let (_, starts, _) = diff::rows(&d.files, &ws.diff_collapsed, &limits);
                    ws.diff_scroll.scroll_to_item(starts[i], ScrollStrategy::Top);
                    cx.notify();
                }
            }
        }
        "diff.open" => {
            if let Some(p) = path {
                let line = a.get("line").and_then(Value::as_u64).map(|l| l as u32);
                ws.open_diff_file(&p, line, cx);
            }
        }
        _ => return false,
    }
    true
}

/** What inspect.json shows about this module's views: the selected worktree's diff, file by file. */
pub(crate) fn inspect(ws: &Workspace) -> Value {
    let Some(path) = ws.selected.clone() else { return Value::Null };
    let base = ws.diff_base();
    let Some(d) = ws.diffs.get(&format!("{path}|{base}")) else { return json!({"worktree": path, "base": base, "loaded": false}) };
    json!({
        "worktree": path, "base": base, "base_label": d.base_label, "loading": d.loading, "truncated": d.truncated, "error": d.error,
        "files": d.files.iter().map(|f| json!({
            "path": f.path, "old_path": f.old_path, "status": f.status.to_string(), "adds": f.added, "dels": f.removed,
            "binary": f.binary, "collapsed": ws.diff_collapsed.contains(&f.path),
        })).collect::<Vec<_>>(),
        // Rows: `visible` [first, end) are drawn, each `row_height` tall, from the top of the diff node.
        "rows": ws.diff_geom.borrow().clone(),
    })
}
