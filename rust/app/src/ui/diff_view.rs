/*! The `diff` node: a worktree's changes as a file list beside a virtualized, horizontally scrolling
diff. Clicking a file jumps to it, a file header collapses it, a line number opens that line in Cursor. */

use super::style;
use crate::diff::{self, Kind, Row};
use crate::workspace::Workspace;
use agent_box_protocol::Call;
use gpui_kit::component::Icon;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
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

/** One diff's scroll position: the uniform list's, or the variable-height list's when `diff.wrap` is on. */
pub struct DiffScroll {
    pub uniform: UniformListScrollHandle,
    pub list: ListState,
    sig: u64,
}

impl Default for DiffScroll {
    fn default() -> Self {
        DiffScroll { uniform: UniformListScrollHandle::new(), list: ListState::new(0, ListAlignment::Top, px(400.)), sig: u64::MAX }
    }
}

/** What a diff row needs to draw itself, worked out once per frame from settings `diff`. */
struct Look {
    key: String,
    worktree: String,
    base: String,
    theme: Map<String, Value>,
    colors: Map<String, Value>,
    add_bg: Hsla,
    del_bg: Hsla,
    add_fg: Hsla,
    del_fg: Hsla,
    muted: Hsla,
    hunk_bg: Hsla,
    file_bg: Hsla,
    mono: String,
    font_size: f32,
    row_height: f32,
    gutter_w: f32,
    sign_w: f32,
    header_font: String,
    header_size: f32,
    note_size: f32,
    file_header: Map<String, Value>,
    hunk: Map<String, Value>,
    line: Map<String, Value>,
    gutter: Map<String, Value>,
    tab: String,
    icons: (String, String),
    labels: Map<String, Value>,
    line_spec: Option<Value>,
    editor: String,
    host: String,
    machine: String,
    wrap: bool,
}

impl Look {
    fn status(&self, s: char) -> Hsla {
        let (key, part) = status_part(s);
        let fallback = match part {
            "add" => self.add_fg,
            "del" => self.del_fg,
            _ => self.muted,
        };
        self.colors.get(&key).and_then(|v| style::color(v, &self.theme)).unwrap_or(fallback)
    }

    fn label(&self, key: &str, default: &str, n: Option<usize>) -> String {
        label(&self.labels, key, default, n)
    }
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

    pub(crate) fn diff_view(&mut self, props: &Map<String, Value>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
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
        let collapsed = Rc::new(collapsed_in(&self.diff_collapsed, &path));
        let (rows, starts, widest) = diff::rows(&files, &collapsed, &limits);
        let rows = Rc::new(rows);
        let colors = ds.get("colors").and_then(Value::as_object).cloned().unwrap_or_default();
        // `diff.colors.NAME` (a color or @token) overrides the theme token each part uses by default.
        let c = |k: &str, fallback: &str| {
            let v = colors.get(k).cloned().unwrap_or_else(|| Value::from(format!("@{}", default_token(k))));
            style::color(&v, &theme).unwrap_or_else(|| crate::settings::parse_color(fallback))
        };
        let num = |k: &str, d: f64| ds.get(k).and_then(Value::as_f64).unwrap_or(d) as f32;
        let header = st("header");
        let font = |v: Option<&Value>, d: &str| {
            let f = v.and_then(Value::as_str).unwrap_or(d);
            f.strip_prefix('@').and_then(|t| theme.get(t)).and_then(Value::as_str).unwrap_or(f).to_string()
        };
        let icons = st("icons");
        let icon = |k: &str, d: &str| icons.get(k).and_then(Value::as_str).unwrap_or(d).to_string();
        let render = st("render");
        let look = Rc::new(Look {
            key: key.clone(),
            worktree: path.clone(),
            base: base.clone(),
            add_bg: c("add_bg", "#1d3324"),
            del_bg: c("del_bg", "#3a1e23"),
            add_fg: c("add", "#3fb950"),
            del_fg: c("del", "#e5534b"),
            muted: c("muted", "#6c6c70"),
            hunk_bg: c("hunk_bg", "#22262e"),
            file_bg: c("file_bg", "#262628"),
            mono: font(theme.get("font_mono"), "Menlo"),
            font_size: num("font_size", 12.),
            row_height: num("row_height", 20.),
            gutter_w: num("gutter_width", 44.),
            sign_w: num("sign_width", 14.),
            header_font: font(header.get("font_family"), "@font_ui"),
            header_size: header.get("font_size").and_then(Value::as_f64).unwrap_or(12.5) as f32,
            note_size: num("note_font_size", 12.),
            file_header: st("file_header"),
            hunk: st("hunk"),
            line: st("line"),
            gutter: st("gutter"),
            tab: " ".repeat(num("tab_width", 4.).max(0.) as usize),
            icons: (icon("open", "chevron-down"), icon("closed", "chevron-right")),
            labels: st("labels"),
            line_spec: render.get("line").filter(|v| !v.is_null()).cloned(),
            editor: self.diff_link_template(&path),
            host: self.diff_link_host(&path),
            machine: self.host_of_path(&path),
            wrap: ds.get("wrap").and_then(Value::as_bool) == Some(true),
            theme: theme.clone(),
            colors: colors.clone(),
        });
        let geom = self.diff_geom.clone();
        {
            let mut g = geom.borrow_mut();
            if !g.is_object() {
                *g = json!({});
            }
            if let Value::Object(all) = &mut *g {
                all.insert(key.clone(), json!({
                    "row_height": look.row_height,
                    "file_rows": files.iter().zip(starts.iter()).map(|(f, r)| json!({"path": f.path, "row": r})).collect::<Vec<_>>(),
                    "rows": rows.len(),
                    "wrap": look.wrap,
                    "generation": self.render_generation,
                }));
            }
        }
        let scroll = self.diff_scroll.entry(key.clone()).or_default();
        let (r, f, sh) = (rows.clone(), files.clone(), collapsed.clone());
        let list: AnyElement = if look.wrap {
            let sig = wrap_sig(rows.len(), Rc::as_ptr(&files) as usize, &collapsed);
            if scroll.sig != sig {
                scroll.list.reset(rows.len());
                scroll.sig = sig;
            }
            let l = look.clone();
            list(scroll.list.clone(), cx.processor(move |ws: &mut Workspace, ix: usize, window, cx| diff_row(ws, &l, &r, &f, &sh, ix, window, cx))).size_full().into_any_element()
        } else {
            let (l, k, g) = (look.clone(), key.clone(), geom.clone());
            uniform_list("diff-rows", rows.len(), cx.processor(move |ws: &mut Workspace, range: std::ops::Range<usize>, window, cx| {
                if let Some(Value::Object(o)) = g.borrow_mut().get_mut(&k) {
                    o.insert("visible".into(), json!([range.start, range.end]));
                }
                range.map(|ix| diff_row(ws, &l, &r, &f, &sh, ix, window, cx)).collect::<Vec<_>>()
            }))
            .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
            .with_width_from_item(Some(widest))
            .track_scroll(&scroll.uniform)
            .size_full()
            .into_any_element()
        };

        let mut list_style = st("file_list");
        let side = ds.get("file_list_side").and_then(Value::as_str).unwrap_or("left").to_string();
        if side == "right" && !list_style.contains_key("border_left") && list_style.contains_key("border_right") {
            // The default rule sits between the list and the diff, whichever side the list is on.
            let w = list_style.remove("border_right").unwrap_or(Value::Null);
            list_style.insert("border_left".into(), w);
        }
        let hover_bg = c("hover", "#28282b");
        let row_spec = render.get("file_row").filter(|v| !v.is_null()).cloned();
        let mut file_list = style::apply(div().id("diff-files").flex().flex_col().overflow_y_scroll(), &list_style, &theme);
        if side != "hidden" {
            for (i, f) in files.iter().enumerate() {
                let (dir, name) = f.path.rsplit_once('/').map(|(d, n)| (format!("{d}/"), n.to_string())).unwrap_or((String::new(), f.path.clone()));
                let action = json!({"do": "diff.jump", "path": f.path, "worktree": path, "base": base});
                self.clickables.push((format!("diff-file:{}", f.path), f.path.clone(), action.clone()));
                let jump = cx.listener(move |ws, _: &ClickEvent, window, cx| ws.run_action(&action, window, cx));
                let custom = row_spec.as_ref().and_then(|spec| {
                    let props = json!({
                        "path": f.path, "old_path": f.old_path, "status": f.status.to_string(), "adds": f.added, "dels": f.removed,
                        "dir": dir, "name": name, "binary": f.binary, "worktree": path, "base": base, "collapsed": collapsed.contains(&f.path), "index": i,
                    });
                    self.render_part(Some(spec), props, &format!("diff-file:{key}[{i}]"), window, cx)
                });
                let row = match custom {
                    Some(el) => div().id(("diff-file", i)).cursor_pointer().child(el).on_click(jump).into_any_element(),
                    None => style::apply(div().id(("diff-file", i)).flex().flex_row().items_center().cursor_pointer(), &st("file_row"), &theme)
                        .when(!st("file_row").contains_key("hover"), |d| d.hover(move |s| s.bg(hover_bg)))
                        .child(div().w(px(look.sign_w)).flex_shrink_0().text_color(look.status(f.status)).child(f.status.to_string()))
                        .child(div().flex_1().min_w_0().truncate().child(name).child(div().text_color(look.muted).text_size(px(11.)).truncate().child(dir)))
                        .child(div().text_size(px(11.)).text_color(look.add_fg).child(look.label("added", "+{n}", Some(f.added))))
                        .child(div().text_size(px(11.)).text_color(look.del_fg).pl(px(4.)).child(look.label("removed", "−{n}", Some(f.removed))))
                        .on_click(jump)
                        .into_any_element(),
                };
                file_list = file_list.child(row);
            }
        }
        let changes = div().flex_1().min_w_0().h_full().child(list);
        let el = div().size_full().flex().flex_row();
        match side.as_str() {
            "hidden" => el.child(changes),
            "right" => el.child(changes).child(file_list),
            _ => el.child(file_list).child(changes),
        }
        .into_any_element()
    }
}

/** Row `ix` of a diff: a file header, a hunk header, a note, or a line (or `diff.render.line` in its place). */
#[allow(clippy::too_many_arguments)]
fn diff_row(ws: &mut Workspace, l: &Look, rows: &[Row], files: &[diff::File], collapsed: &HashSet<String>, ix: usize, window: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let base = div().min_w_full().flex().flex_row().items_center();
    let base = if l.wrap { base.min_h(px(l.row_height)) } else { base.h(px(l.row_height)).whitespace_nowrap() };
    match &rows[ix] {
        Row::File(i) => {
            let f = &files[*i];
            let shut = collapsed.contains(&f.path);
            let action = json!({"do": "diff.toggle_file", "path": f.path, "worktree": l.worktree, "base": l.base});
            let verb = if shut { l.label("expand", "Expand", None) } else { l.label("collapse", "Collapse", None) };
            ws.clickables.push((format!("diff-header:{}", f.path), format!("{verb} {}", f.path), action.clone()));
            let title = match &f.old_path {
                Some(o) => format!("{o} {} {}", l.label("arrow", "→", None), f.path),
                None => f.path.clone(),
            };
            let icon = if shut { &l.icons.1 } else { &l.icons.0 };
            let el = base.id(("file", ix)).bg(l.file_bg).px(px(10.)).gap(px(8.)).cursor_pointer().font_family(l.header_font.clone()).text_size(px(l.header_size));
            style::apply(el, &l.file_header, &l.theme)
                .child(Icon::empty().path(format!("icons/{icon}.svg")).size(px(12.)).text_color(l.muted))
                .child(div().text_color(l.status(f.status)).child(f.status.to_string()))
                .child(title)
                .child(div().text_color(l.add_fg).child(l.label("added", "+{n}", Some(f.added))))
                .child(div().text_color(l.del_fg).child(l.label("removed", "−{n}", Some(f.removed))))
                .on_click(cx.listener(move |ws, _, window, cx| ws.run_action(&action, window, cx)))
                .into_any_element()
        }
        Row::Hunk(h) => style::apply(base.bg(l.hunk_bg).px(px(10.)).text_color(l.muted).font_family(l.mono.clone()).text_size(px(l.font_size)), &l.hunk, &l.theme).child(h.clone()).into_any_element(),
        Row::Note(n) => base.px(px(10.)).text_color(l.muted).italic().text_size(px(l.note_size)).child(n.clone()).into_any_element(),
        Row::Line { file, line } => {
            let (bg, sign, fg, kind) = match line.kind {
                Kind::Add => (Some(l.add_bg), l.label("add_sign", "+", None), Some(l.add_fg), "add"),
                Kind::Del => (Some(l.del_bg), l.label("del_sign", "−", None), Some(l.del_fg), "del"),
                Kind::Meta => (None, l.label("context_sign", " ", None), Some(l.muted), "meta"),
                Kind::Ctx => (None, l.label("context_sign", " ", None), None, "context"),
            };
            let fpath = &files[*file].path;
            if let Some(spec) = &l.line_spec {
                let props = json!({"kind": kind, "old": line.old, "new": line.new, "text": line.text, "file": fpath, "sign": sign, "worktree": l.worktree, "base": l.base, "row": ix});
                if let Some(el) = ws.render_part(Some(spec), props, &format!("diff-line:{}[{ix}]", l.key), window, cx) {
                    return base.child(el).into_any_element();
                }
            }
            let target = line.new.or(line.old).unwrap_or(1);
            if line.new.or(line.old).is_some() {
                let open = json!({"do": "diff.open", "path": fpath, "line": target, "worktree": l.worktree, "base": l.base});
                ws.clickables.push((format!("diff-line:{fpath}:{target}"), format!("{fpath}:{target}"), open));
            }
            let url = link(&l.editor, &l.host, &l.machine, &format!("{}/{fpath}", l.worktree), target);
            let num = |n: Option<u32>| div().w(px(l.gutter_w)).flex_shrink_0().text_right().pr(px(8.)).text_color(l.muted).child(n.map(|n| n.to_string()).unwrap_or_default());
            let hover = l.gutter.get("hover").and_then(Value::as_object).cloned().unwrap_or_else(|| json!({"opacity": 0.7}).as_object().cloned().unwrap_or_default());
            let theme = l.theme.clone();
            let gutter = style::apply(div().id(("gutter", ix)).flex().flex_row().flex_shrink_0().cursor_pointer(), &l.gutter, &l.theme)
                .hover(move |mut s| {
                    style::refine(&mut s, &hover, &theme);
                    s
                })
                .child(num(line.old))
                .child(num(line.new))
                .on_click(move |_, _, cx| cx.open_url(&url));
            let mut el = base.font_family(l.mono.clone()).text_size(px(l.font_size)).child(gutter).child(div().w(px(l.sign_w)).flex_shrink_0().text_color(fg.unwrap_or(l.muted)).child(sign));
            if let Some(bg) = bg {
                el = el.bg(bg);
            }
            let text = if line.text.is_empty() { " ".to_string() } else { line.text.replace('\t', &l.tab) };
            let text = div().pr(px(24.)).child(text);
            let text = if l.wrap { text.flex_1().min_w_0().whitespace_normal() } else { text };
            style::apply(el, &l.line, &l.theme).child(text).into_any_element()
        }
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

    /** The worktree and base an action names (`worktree`, `base`), else the selected one and `state.diff_base`. */
    fn diff_target(&self, a: &Map<String, Value>) -> Option<(String, String)> {
        let worktree = a.get("worktree").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string).or_else(|| self.selected.clone())?;
        let base = a.get("base").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string).unwrap_or_else(|| self.diff_base());
        Some((worktree, base))
    }

    /** Opens a changed file at `line` (default: its first change) the way the gutter link does. */
    fn open_diff_file(&self, root: &str, base: &str, file: &str, line: Option<u32>, cx: &mut Context<Self>) -> bool {
        let Some(d) = self.diffs.get(&format!("{root}|{base}")) else { return false };
        let Some(f) = d.files.iter().find(|f| f.path == file) else { return false };
        let first = f.hunks.iter().flat_map(|h| h.lines.iter()).find(|l| !matches!(l.kind, Kind::Ctx | Kind::Meta)).and_then(|l| l.new.or(l.old));
        let url = link(&self.diff_link_template(root), &self.diff_link_host(root), &self.host_of_path(root), &format!("{root}/{}", f.path), line.or(first).unwrap_or(1));
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

/** The `diff.colors` key for a file status letter and the part it falls back to (add, del, or muted). */
fn status_part(s: char) -> (String, &'static str) {
    let fallback = match s {
        'A' => "add",
        'D' => "del",
        _ => "muted",
    };
    (format!("status_{s}"), fallback)
}

/** A `diff.labels` entry (else `default`), with `{n}` filled in. */
fn label(labels: &Map<String, Value>, key: &str, default: &str, n: Option<usize>) -> String {
    let t = labels.get(key).and_then(Value::as_str).unwrap_or(default);
    match n {
        Some(n) => t.replace("{n}", &n.to_string()),
        None => t.to_string(),
    }
}

/** Collapsed files are kept per worktree, as `WORKTREE|PATH`. */
fn collapse_key(worktree: &str, path: &str) -> String {
    format!("{worktree}|{path}")
}

/** The paths collapsed in one worktree's diff. */
fn collapsed_in(all: &HashSet<String>, worktree: &str) -> HashSet<String> {
    let prefix = format!("{worktree}|");
    all.iter().filter_map(|k| k.strip_prefix(&prefix)).map(str::to_string).collect()
}

/** Changes when a wrapped diff's rows do, so its list re-measures them. */
fn wrap_sig(rows: usize, files: usize, collapsed: &HashSet<String>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut c: Vec<&String> = collapsed.iter().collect();
    c.sort();
    (rows, files, c).hash(&mut h);
    h.finish()
}

/** Actions this module handles (named in SETTINGS.md with the rest). */
pub(crate) const ACTIONS: &[(&str, &str)] = &[
    ("diff.toggle_file", "{path, worktree?, base?}: collapse or expand one file in a worktree's diff (path relative to the worktree; worktree defaults to the selected one, base to `state.diff_base`)"),
    ("diff.jump", "{path, worktree?, base?}: scroll the diff to that file"),
    ("diff.open", "{path, line?, worktree?, base?}: open the file at that line (default: its first change) via `diff.open_url` / `diff.open_url_local`"),
    ("diff.collapse_all / diff.expand_all", "{worktree?}: collapse or expand every file in the diff (one worktree's, or every loaded diff's)"),
];

/** Runs `name` if it's one of this module's actions; false otherwise. */
pub(crate) fn dispatch(ws: &mut Workspace, name: &str, a: &Map<String, Value>, _window: &mut Window, cx: &mut Context<Workspace>) -> bool {
    let path = a.get("path").and_then(Value::as_str).map(str::to_string);
    let named = a.get("worktree").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
    match name {
        "diff.toggle_file" => {
            if let (Some(p), Some((root, _))) = (path, ws.diff_target(a)) {
                let k = collapse_key(&root, &p);
                if !ws.diff_collapsed.remove(&k) {
                    ws.diff_collapsed.insert(k);
                }
                cx.notify();
            }
        }
        "diff.jump" => {
            let (Some(p), Some((root, base))) = (path, ws.diff_target(a)) else { return true };
            let key = format!("{root}|{base}");
            let limits = ws.diff_limits();
            let collapsed = collapsed_in(&ws.diff_collapsed, &root);
            let start = ws.diffs.get(&key).and_then(|d| {
                let i = d.files.iter().position(|f| f.path == p)?;
                let (_, starts, _) = diff::rows(&d.files, &collapsed, &limits);
                starts.get(i).copied()
            });
            if let Some(start) = start {
                let s = ws.diff_scroll.entry(key).or_default();
                if ws.settings.diff.get("wrap").and_then(Value::as_bool) == Some(true) {
                    s.list.scroll_to(ListOffset { item_ix: start, offset_in_item: px(0.) });
                } else {
                    s.uniform.scroll_to_item(start, ScrollStrategy::Top);
                }
                cx.notify();
            }
        }
        "diff.open" => {
            if let (Some(p), Some((root, base))) = (path, ws.diff_target(a)) {
                let line = a.get("line").and_then(Value::as_u64).map(|l| l as u32);
                ws.open_diff_file(&root, &base, &p, line, cx);
            }
        }
        "diff.collapse_all" => {
            let keys: Vec<String> = ws
                .diffs
                .iter()
                .filter_map(|(k, d)| k.rsplit_once('|').map(|(root, _)| (root, d)))
                .filter(|(root, _)| named.as_deref().is_none_or(|n| n == *root))
                .flat_map(|(root, d)| d.files.iter().map(move |f| collapse_key(root, &f.path)))
                .collect();
            ws.diff_collapsed.extend(keys);
        }
        "diff.expand_all" => match named {
            Some(root) => {
                let prefix = format!("{root}|");
                ws.diff_collapsed.retain(|k| !k.starts_with(&prefix));
            }
            None => ws.diff_collapsed.clear(),
        },
        _ => return false,
    }
    true
}

fn describe(ws: &Workspace, root: &str, base: &str, d: &DiffData) -> Value {
    let collapsed = collapsed_in(&ws.diff_collapsed, root);
    json!({
        "worktree": root, "base": base, "base_label": d.base_label, "loading": d.loading, "truncated": d.truncated, "error": d.error,
        "files": d.files.iter().map(|f| json!({
            "path": f.path, "old_path": f.old_path, "status": f.status.to_string(), "adds": f.added, "dels": f.removed,
            "binary": f.binary, "collapsed": collapsed.contains(&f.path),
        })).collect::<Vec<_>>(),
        // Rows: `visible` [first, end) are drawn, each `row_height` tall, from the top of the diff node.
        "rows": ws.diff_geom.borrow().get(format!("{root}|{base}")).cloned().unwrap_or(Value::Null),
    })
}

/** What inspect.json shows about this module's views: the selected worktree's diff, file by file, and
under `all` every diff drawn in the last frame, by `PATH|BASE`. */
pub(crate) fn inspect(ws: &Workspace) -> Value {
    let geom = ws.diff_geom.borrow().clone();
    let drawn: Map<String, Value> = geom
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, g)| g.get("generation").and_then(Value::as_u64).is_some_and(|n| n + 1 >= ws.render_generation))
        .filter_map(|(k, _)| {
            let (root, base) = k.rsplit_once('|')?;
            ws.diffs.get(k).map(|d| (k.clone(), describe(ws, root, base, d)))
        })
        .collect();
    let Some(path) = ws.selected.clone() else { return json!({"all": drawn}) };
    let base = ws.diff_base();
    let mut out = match ws.diffs.get(&format!("{path}|{base}")) {
        Some(d) => describe(ws, &path, &base, d),
        None => json!({"worktree": path, "base": base, "loaded": false}),
    };
    if let Value::Object(o) = &mut out {
        o.insert("all".into(), Value::Object(drawn));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{collapse_key, collapsed_in, label, status_part};
    use serde_json::json;
    use std::collections::HashSet;

    #[test]
    fn labels_fill_counts_and_fall_back() {
        let labels = json!({"added": "{n} new"}).as_object().cloned().unwrap_or_default();
        assert_eq!(label(&labels, "added", "+{n}", Some(3)), "3 new");
        assert_eq!(label(&labels, "removed", "−{n}", Some(2)), "−2");
        assert_eq!(label(&labels, "arrow", "→", None), "→");
    }

    #[test]
    fn status_colors_follow_add_and_del() {
        assert_eq!(status_part('A'), ("status_A".to_string(), "add"));
        assert_eq!(status_part('D'), ("status_D".to_string(), "del"));
        assert_eq!(status_part('R'), ("status_R".to_string(), "muted"));
    }

    #[test]
    fn collapsed_files_stay_with_their_worktree() {
        let all: HashSet<String> = [collapse_key("/w/a", "src/x.rs"), collapse_key("/w/b", "src/y.rs")].into_iter().collect();
        assert_eq!(collapsed_in(&all, "/w/a"), ["src/x.rs".to_string()].into_iter().collect());
        assert!(collapsed_in(&all, "/w").is_empty());
    }
}
