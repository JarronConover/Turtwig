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

const ROW_HEIGHT: f32 = 20.;

impl Workspace {
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
        let d = self.diffs.entry(key.clone()).or_default();
        let due = d.fetched.is_none() || (d.stale && d.fetched.is_some_and(|t| t.elapsed() > Duration::from_secs(2)));
        if d.loading || !self.connected {
            return;
        }
        if !due {
            if d.stale {
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(Duration::from_secs(2)).await;
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
            return style::apply(div().size_full().flex().items_center().justify_center(), &st("empty"), &theme).child("Select a worktree").into_any_element();
        };
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
            let msg = if loaded { "No changes" } else { "Loading changes…" };
            return style::apply(div().size_full().flex().items_center().justify_center(), &st("empty"), &theme).child(msg).into_any_element();
        }
        let (rows, starts, widest) = diff::rows(&files, &self.diff_collapsed);
        let rows = Rc::new(rows);
        let handle = self.diff_scroll.clone();
        let view = cx.entity().downgrade();
        let host = self.settings.connection.ssh_host.clone();
        let editor = ds.get("open_url").and_then(Value::as_str).unwrap_or("cursor://vscode-remote/ssh-remote+{host}{path}:{line}").to_string();
        let c = |k: &str, fallback: &str| style::color(&Value::from(format!("@{k}")), &theme).unwrap_or_else(|| crate::settings::parse_color(fallback));
        let (add_bg, del_bg, add_fg, del_fg, muted, hunk_bg, file_bg) = (
            c("diff_add_bg", "#1d3324"),
            c("diff_del_bg", "#3a1e23"),
            c("diff_add", "#3fb950"),
            c("diff_del", "#e5534b"),
            c("text_faint", "#6c6c70"),
            c("diff_hunk_bg", "#22262e"),
            c("diff_file_bg", "#262628"),
        );
        let mono = theme.get("font_mono").and_then(Value::as_str).unwrap_or("Menlo").to_string();
        let font_size = ds.get("font_size").and_then(Value::as_f64).unwrap_or(12.) as f32;
        let row_files = files.clone();
        let collapsed = self.diff_collapsed.clone();
        let list = uniform_list("diff-rows", rows.len(), move |range, _, _| {
            range
                .map(|ix| {
                    let base = div().h(px(ROW_HEIGHT)).min_w_full().flex().flex_row().items_center().whitespace_nowrap();
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
                                .font_family(".SystemUIFont")
                                .text_size(px(12.5))
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
                        Row::Note(n) => base.px(px(10.)).text_color(muted).italic().text_size(px(12.)).child(n.clone()).into_any_element(),
                        Row::Line { file, line } => {
                            let (bg, sign, fg) = match line.kind {
                                Kind::Add => (Some(add_bg), "+", Some(add_fg)),
                                Kind::Del => (Some(del_bg), "−", Some(del_fg)),
                                Kind::Meta => (None, " ", Some(muted)),
                                Kind::Ctx => (None, " ", None),
                            };
                            let num = |n: Option<u32>| div().w(px(44.)).flex_shrink_0().text_right().pr(px(8.)).text_color(muted).child(n.map(|n| n.to_string()).unwrap_or_default());
                            let target = line.new.or(line.old).unwrap_or(1);
                            let url = editor.replace("{host}", &host).replace("{path}", &format!("{path}/{}", row_files[*file].path)).replace("{line}", &target.to_string());
                            let gutter = div()
                                .id(("gutter", ix))
                                .flex()
                                .flex_row()
                                .cursor_pointer()
                                .hover(|s| s.opacity(0.7))
                                .child(num(line.old))
                                .child(num(line.new))
                                .on_click(move |_, _, cx| cx.open_url(&url));
                            let mut el = base.font_family(mono.clone()).text_size(px(font_size)).child(gutter).child(div().w(px(14.)).flex_shrink_0().text_color(fg.unwrap_or(muted)).child(sign));
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
                    .child(div().w(px(14.)).flex_shrink_0().text_color(status_color(f.status, add_fg, del_fg, muted)).child(f.status.to_string()))
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

fn status_color(s: char, add: Hsla, del: Hsla, other: Hsla) -> Hsla {
    match s {
        'A' => add,
        'D' => del,
        _ => other,
    }
}
