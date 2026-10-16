/*! The transcript in the style of shadcn's Trace: user turns as right-aligned bubbles, replies as text,
and each run of thinking and tool calls as quiet one-line steps that expand; long runs fold. */

use super::style;
use crate::transcript::{label, tool_look, Item};
use crate::workspace::Workspace;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{h_flex, v_flex, Icon};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::{Map, Value};

/** One row of the transcript list: a message, a run of steps, or the "Thinking"/"Working" line. */
pub(crate) enum Block {
    One(usize),
    Steps(Vec<usize>),
    Pending(&'static str),
}

/** Groups items into rows: each run of thinking and tool calls is one row. */
pub(crate) fn blocks_of(items: &[Item], working: bool) -> Vec<Block> {
    let mut blocks: Vec<Block> = vec![];
    for (i, item) in items.iter().enumerate() {
        let step = matches!(item, Item::Tool { .. } | Item::Thinking { .. });
        match (step, blocks.last_mut()) {
            (true, Some(Block::Steps(v))) => v.push(i),
            (true, _) => blocks.push(Block::Steps(vec![i])),
            (false, _) => blocks.push(Block::One(i)),
        }
    }
    let last_is_steps = matches!(blocks.last(), Some(Block::Steps(_)));
    let tail_running = items.last().is_some_and(|i| matches!(i, Item::Tool { result: None, .. }));
    if working && crate::transcript::options().pending && !(last_is_steps && tail_running) {
        blocks.push(Block::Pending(if last_is_steps { "Working" } else { "Thinking" }));
    }
    blocks
}

/** (running, done, failed) titles and an icon for a tool call: the subject (a file, a pattern, a
command) after the tool's prefixes from `crate::transcript::tool_look`. */
fn describe(name: &str, input: &Value, summary: &str, strip_stamps: bool) -> (String, String, String, String) {
    let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let file = |k: &str| {
        let p = s(k);
        let name = p.rsplit('/').next().unwrap_or(&p).to_string();
        if strip_stamps { strip_stamp(&name, 10) } else { name }
    };
    let look = tool_look(name);
    let subject = match name {
        "Read" | "Edit" | "MultiEdit" | "Write" => file("file_path"),
        "NotebookEdit" => file("notebook_path"),
        "Bash" => upper_first(&if s("description").is_empty() { s("command").lines().next().unwrap_or("").to_string() } else { s("description") }),
        "Grep" => format!("“{}”", s("pattern")),
        "Glob" => s("pattern"),
        "WebFetch" => s("url").trim_start_matches("https://").to_string(),
        "WebSearch" => format!("“{}”", s("query")),
        "Agent" | "Task" => s("description"),
        "TodoWrite" => String::new(),
        "Skill" => label("skill_subject", "the {skill} skill", &[("skill", &s("skill"))]),
        "Compaction" => {
            let what = if input.get("trigger").and_then(Value::as_str) == Some("manual") {
                label("compacted", "Compacted the conversation", &[])
            } else {
                label("compacted_auto", "Compacted the conversation automatically", &[])
            };
            return (what.clone(), what.clone(), what, look.icon);
        }
        "AgentMessage" => {
            let what = if summary.is_empty() {
                label("message_from", "Message from {from}", &[("from", &s("from"))])
            } else {
                label("message_from_summary", "Message from {from}: {summary}", &[("from", &s("from")), ("summary", summary)])
            };
            return (what.clone(), what.clone(), what, look.icon);
        }
        _ => {
            let shown = crate::transcript::tool_display_name(name);
            if summary.is_empty() { shown } else { format!("{shown}: {summary}") }
        }
    };
    let join = |prefix: &str| match (prefix.is_empty(), subject.is_empty()) {
        (true, _) => subject.clone(),
        (false, true) => prefix.to_string(),
        (false, false) => format!("{prefix} {subject}"),
    };
    (join(&look.doing), join(&look.done), join(&look.failed), look.icon)
}

/** "1792164281008-Screenshot.png" → "Screenshot.png": a leading run of at least `min` digits before a dash. */
pub(crate) fn strip_stamp(name: &str, min: usize) -> String {
    name.split_once('-').filter(|(stamp, _)| stamp.len() >= min && stamp.chars().all(|c| c.is_ascii_digit())).map(|(_, n)| n.to_string()).unwrap_or_else(|| name.to_string())
}

/** The trace line's look: `chat.step_line` (icon_size, chevron_size, pulse_ms, pulse_min, pulse_max, icons). */
struct LineLook {
    icon_size: f32,
    chevron_size: f32,
    pulse_ms: f32,
    pulse_min: f32,
    pulse_max: f32,
    icons: Value,
}

impl LineLook {
    fn of(chat: &Value) -> Self {
        let c = &chat["step_line"];
        let f = |k: &str, d: f32| c.get(k).and_then(Value::as_f64).map(|x| x as f32).unwrap_or(d);
        LineLook {
            icon_size: f("icon_size", 14.),
            chevron_size: f("chevron_size", 12.),
            pulse_ms: f("pulse_ms", 1400.).max(1.),
            pulse_min: f("pulse_min", 0.35),
            pulse_max: f("pulse_max", 1.),
            icons: c.get("icons").cloned().unwrap_or(Value::Null),
        }
    }

    fn icon(&self, key: &str, default: &str) -> String {
        self.icons.get(key).and_then(Value::as_str).unwrap_or(default).to_string()
    }
}

fn upper_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

type Styles<'a> = &'a dyn Fn(&str) -> Map<String, Value>;

impl Workspace {
    /** Row `bi` of a chat's transcript. */
    pub(crate) fn render_block(&mut self, chat_id: &str, items: &[Item], blocks: &[Block], bi: usize, working: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = self.settings.theme.clone();
        let cs = self.settings.chat.clone();
        let st = |k: &str| match cs.get(k) {
            Some(Value::Object(o)) => o.clone(),
            _ => Map::new(),
        };
        let fold_after = cs.get("fold_steps_after").and_then(Value::as_u64).unwrap_or(5).max(1) as usize;
        let n = blocks.len() - usize::from(matches!(blocks.last(), Some(Block::Pending(_))));
        match blocks.get(bi)? {
            Block::One(i) => self.render_message(chat_id, *i, &items[*i], &st, &theme),
            Block::Pending(title) => {
                let key = format!("{chat_id}:pending");
                let look = LineLook::of(&cs);
                let text = if *title == "Working" { label("pending_working", "Working", &[]) } else { label("pending_thinking", "Thinking", &[]) };
                let line = self.trace_line(&key, &look.icon("pending", "brain"), &text, true, false, false, false, vec![], &st, &theme, cx);
                Some(style::apply(v_flex(), &st("trace"), &theme).child(line).into_any_element())
            }
            Block::Steps(steps) => {
                let active = working && bi + 1 == n;
                let mut trace = style::apply(v_flex(), &st("trace"), &theme);
                let key = format!("{chat_id}:run:{}", steps[0]);
                let folded = steps.len() > fold_after;
                let plural = |n: usize| if n == 1 { "" } else { "s" };
                let fold_icon = LineLook::of(&cs).icon("fold", "list-checks");
                if folded && !active {
                    let open = self.expanded_tools.contains(&key);
                    let tools = steps.iter().filter(|i| matches!(items[**i], Item::Tool { .. })).count();
                    let errors = steps.iter().filter(|i| matches!(items[**i], Item::Tool { is_error: true, .. })).count();
                    let failed = if errors > 0 { label("steps_failed", ", {n} failed", &[("n", &errors.to_string())]) } else { String::new() };
                    let title = label("steps", "{n} step{s}{failed}", &[("n", &tools.to_string()), ("s", plural(tools)), ("failed", &failed)]);
                    let lines = if open { steps.iter().map(|&i| self.render_step(chat_id, i, &items[i], false, &st, &theme, cx)).collect() } else { vec![] };
                    trace = trace.child(self.trace_line(&key, &fold_icon, &title, false, false, true, open, lines, &st, &theme, cx));
                } else {
                    let hidden = if active && folded { steps.len() - fold_after } else { 0 };
                    if hidden > 0 {
                        let open = self.expanded_tools.contains(&key);
                        let lines = if open { steps[..hidden].iter().map(|&i| self.render_step(chat_id, i, &items[i], false, &st, &theme, cx)).collect() } else { vec![] };
                        let title = label("earlier_steps", "{n} earlier step{s}", &[("n", &hidden.to_string()), ("s", plural(hidden))]);
                        trace = trace.child(self.trace_line(&key, &fold_icon, &title, false, false, true, open, lines, &st, &theme, cx));
                    }
                    let last = *steps.last().unwrap_or(&0);
                    for &i in &steps[hidden..] {
                        trace = trace.child(self.render_step(chat_id, i, &items[i], active && i == last, &st, &theme, cx));
                    }
                }
                Some(trace.into_any_element())
            }
        }
    }

    fn render_message(&mut self, chat_id: &str, i: usize, item: &Item, st: Styles, theme: &Map<String, Value>) -> Option<AnyElement> {
        let key = format!("{chat_id}:{i}");
        match item {
            Item::User { text } => {
                let image_style = st("image");
                let max = |k: &str, d: f32| image_style.get(k).and_then(Value::as_f64).map(|x| x as f32).unwrap_or(d);
                let (mw, mh) = (max("max_width", 360.), max("max_height", 240.));
                let (uploads, rest) = crate::workspace::split_uploads(text);
                let file_icon = self.attachment_setting("file_icon").and_then(|v| v.as_str().map(str::to_string)).unwrap_or_else(|| "file".into());
                let strip = self.strip_stamps();
                let thumbs: Vec<AnyElement> = uploads
                    .into_iter()
                    .map(|p| {
                        if self.is_image_path(&p) {
                            let (w, h) = crate::workspace::fit(&p, mw, mh);
                            style::apply(img(p).w(px(w)).h(px(h)).object_fit(ObjectFit::Contain), &image_style, theme).into_any_element()
                        } else {
                            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                            let name = if strip { strip_stamp(&name, 1) } else { name };
                            style::apply(h_flex().gap(px(6.)), &st("file"), theme)
                                .child(Icon::empty().path(format!("icons/{file_icon}.svg")).size(px(14.)))
                                .child(name)
                                .into_any_element()
                        }
                    })
                    .collect();
                let body = (!rest.is_empty()).then(|| self.markdown(&format!("{key}:u"), &rest, theme));
                let bubble = style::apply(v_flex(), &st("user"), theme).children(body).children(thumbs);
                let row = h_flex().w_full().justify_end().child(bubble).into_any_element();
                Some(self.with_message_actions(chat_id, &key, text, true, row, theme))
            }
            Item::Image { media_type, data } => {
                let image = self.images.entry(key.clone()).or_insert_with(|| crate::workspace::decode_image(media_type, data)).clone();
                image.map(|i| h_flex().w_full().justify_end().child(style::apply(img(i).object_fit(ObjectFit::Contain), &st("image"), theme)).into_any_element())
            }
            Item::Assistant { text } => {
                let body = style::apply(div(), &st("assistant"), theme).child(self.markdown(&key, text, theme));
                Some(self.with_message_actions(chat_id, &key, text, false, body.into_any_element(), theme))
            }
            Item::Notice { text } => Some(style::apply(div(), &st("notice"), theme).child(text.clone()).into_any_element()),
            _ => None,
        }
    }

    fn markdown(&self, key: &str, text: &str, theme: &Map<String, Value>) -> TextView {
        let selectable = self.settings.chat.get("selectable").and_then(Value::as_bool) != Some(false);
        let angles = self.settings.chat.get("markdown").and_then(|m| m.get("show_angle_brackets")).and_then(Value::as_bool) != Some(false);
        let text = if angles { show_angle_brackets(text) } else { text.to_string() };
        let view = TextView::markdown(SharedString::from(key.to_string()), text).selectable(selectable);
        match self.markdown_style(theme) {
            Some(st) => view.style(st),
            None => view,
        }
    }

    /** settings `chat.markdown`: paragraph_gap (rem), heading_sizes (h1..h6 points), style objects for
    code_block, table, table_head, table_cell, `theme` ("dark", "light", or "auto": `window.appearance`), and
    `highlight` ("dark", "light", or a Zed-format theme JSON file) for code colors. Unset: the built-in look. */
    pub(crate) fn markdown_style(&self, theme: &Map<String, Value>) -> Option<gpui_kit::component::text::TextViewStyle> {
        let md = self.settings.chat.get("markdown").and_then(Value::as_object)?;
        let refine = |k: &str| {
            let mut r = StyleRefinement::default();
            if let Some(o) = md.get(k).and_then(Value::as_object) {
                style::refine(&mut r, o, theme);
            }
            r
        };
        use gpui_kit::component::highlighter::HighlightTheme;
        let theme_name = md.get("theme").and_then(Value::as_str).unwrap_or("dark");
        let dark = match theme_name {
            "light" => false,
            "auto" => self.settings.get("window.appearance").and_then(Value::as_str) != Some("light"),
            _ => true,
        };
        let highlight = match md.get("highlight").and_then(Value::as_str) {
            Some("light") => HighlightTheme::default_light(),
            Some("dark") => HighlightTheme::default_dark(),
            Some(path) => std::fs::read_to_string(crate::script::expand(path))
                .ok()
                .and_then(|t| serde_json::from_str::<HighlightTheme>(&t).ok())
                .map(std::sync::Arc::new)
                .unwrap_or_else(|| if dark { HighlightTheme::default_dark() } else { HighlightTheme::default_light() }),
            None if dark => HighlightTheme::default_dark(),
            None => HighlightTheme::default_light(),
        };
        let mut st = gpui_kit::component::text::TextViewStyle {
            code_block: refine("code_block"),
            table: refine("table"),
            table_head: refine("table_head"),
            table_cell: refine("table_cell"),
            is_dark: dark,
            highlight_theme: highlight,
            ..Default::default()
        };
        if let Some(g) = md.get("paragraph_gap").and_then(Value::as_f64) {
            st.paragraph_gap = rems(g as f32);
        }
        if let Some(sizes) = md.get("heading_sizes").and_then(Value::as_array) {
            let sizes: Vec<f32> = sizes.iter().filter_map(Value::as_f64).map(|x| x as f32).collect();
            st = st.heading_font_size(move |level, base| sizes.get(level.saturating_sub(1) as usize).map(|s| px(*s)).unwrap_or(base));
        }
        Some(st)
    }

    /** A message with Copy and Fork under it while hovered. `chat.message_actions`: false hides them; an
    object sets `buttons` (["copy", "fork"]), `show` ("hover" or "always"), `icons`, `tooltips`, `icon_size`,
    `padding`, `radius`, `height`, `gap`, `color`, and `hover` (colors or @tokens). */
    fn with_message_actions(&self, chat_id: &str, key: &str, text: &str, mine: bool, body: AnyElement, theme: &Map<String, Value>) -> AnyElement {
        let cfg = self.settings.chat.get("message_actions").cloned().unwrap_or(Value::Bool(true));
        if cfg == Value::Bool(false) || chat_id == crate::designer::CHAT_ID {
            return body;
        }
        let f = |k: &str, d: f32| cfg.get(k).and_then(Value::as_f64).map(|x| x as f32).unwrap_or(d);
        let text_of = |group: &str, k: &str, d: &str| cfg.get(group).and_then(|g| g.get(k)).and_then(Value::as_str).unwrap_or(d).to_string();
        let tok = |k: &str, d: &str| cfg.get(k).cloned().unwrap_or_else(|| Value::from(d));
        let muted = style::color(&tok("color", "@text_faint"), theme).unwrap_or(gpui_kit::black());
        let hover = style::color(&tok("hover", "@hover"), theme).unwrap_or(gpui_kit::black());
        let (icon_size, pad, radius) = (f("icon_size", 13.), f("padding", 4.), f("radius", 5.));
        let always = cfg.get("show").and_then(Value::as_str) == Some("always");
        let buttons: Vec<String> = cfg
            .get("buttons")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
            .unwrap_or_else(|| vec!["copy".into(), "fork".into()]);
        let group = SharedString::from(format!("msg:{key}"));
        let view = self.view_handle.clone();
        let button = |id: String, icon: String, tip: String, action: Value| {
            let view = view.clone();
            let tip = SharedString::from(tip);
            div()
                .id(SharedString::from(id))
                .p(px(pad))
                .rounded(px(radius))
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx))
                .child(Icon::empty().path(format!("icons/{icon}.svg")).size(px(icon_size)).text_color(muted))
                .on_click(move |_, window, cx| {
                    if let Some(v) = view.as_ref().and_then(|v| v.upgrade()) {
                        let action = action.clone();
                        v.update(cx, |ws, cx| ws.run_action(&action, window, cx));
                    }
                })
        };
        let mut bar = h_flex()
            .gap(px(f("gap", 2.)))
            .h(px(f("height", 22.)))
            .when(mine, |d| d.justify_end())
            .when(!always, |d| d.invisible().group_hover(group.clone(), |s| s.visible()));
        for b in &buttons {
            bar = match b.as_str() {
                "copy" => bar.child(button(
                    format!("{key}:copy"),
                    text_of("icons", "copy", "copy"),
                    text_of("tooltips", "copy", "Copy"),
                    serde_json::json!({"do": "clipboard.copy", "text": text}),
                )),
                "fork" => bar.child(button(
                    format!("{key}:fork"),
                    text_of("icons", "fork", "git-fork"),
                    text_of("tooltips", "fork", "Fork this chat (a new tab that starts from this conversation)"),
                    serde_json::json!({"do": "chat.fork", "chat": chat_id}),
                )),
                _ => bar,
            };
        }
        v_flex().w_full().group(group).child(body).child(bar).into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_step(&mut self, chat_id: &str, i: usize, item: &Item, running: bool, st: Styles, theme: &Map<String, Value>, cx: &mut Context<Self>) -> AnyElement {
        match item {
            Item::Thinking { text } => {
                let key = format!("{chat_id}:think:{i}");
                let open = self.expanded_tools.contains(&key);
                let has_text = !text.trim().is_empty();
                let body = (has_text && open).then(|| div().whitespace_normal().child(text.trim().to_string()).into_any_element());
                let look = LineLook::of(&self.settings.chat);
                let title = if running { label("thinking", "Thinking", &[]) } else { label("thought", "Thought", &[]) };
                self.trace_line(&key, &look.icon("thinking", "brain"), &title, running, false, has_text, open, body.into_iter().collect(), st, theme, cx)
            }
            Item::Tool { id, name, summary, input, result, is_error } => {
                let (doing, done, failed, icon) = describe(name, input, summary, self.strip_stamps());
                let live = running && result.is_none();
                let title = if *is_error { failed } else if live { doing } else { done };
                let open = self.expanded_tools.contains(id);
                let body = if open { self.step_detail(id, name, input, result.as_deref(), *is_error, st, theme) } else { vec![] };
                self.trace_line(id, &icon, &title, live, *is_error, true, open, body, st, theme, cx)
            }
            _ => div().into_any_element(),
        }
    }

    /** What a step did: an edit as -/+ lines, a command with its output, or the tool's result. */
    fn step_detail(&mut self, id: &str, name: &str, input: &Value, result: Option<&str>, is_error: bool, st: Styles, theme: &Map<String, Value>) -> Vec<AnyElement> {
        if name == "AgentMessage" || name == "Compaction" {
            let body = self.markdown(&format!("{id}:msg"), result.unwrap_or(""), theme);
            return vec![style::apply(div(), &st("agent_message"), theme).child(body).into_any_element()];
        }
        let max_lines = self.settings.chat.get("step_detail_lines").and_then(Value::as_u64).unwrap_or(40) as usize;
        // `chat.step_detail`: colors (theme tokens or values) for removed/added/error lines, the line
        // prefixes, the command prompt, the JSON fallback, the tab width, and the paddings.
        let cfg = self.settings.chat.get("step_detail").cloned().unwrap_or(Value::Null);
        let c = |k: &str, d: &str| style::color(&cfg.get(k).cloned().unwrap_or_else(|| Value::from(d)), theme);
        let txt = |k: &str, d: &str| cfg.get(k).and_then(Value::as_str).unwrap_or(d).to_string();
        let num = |k: &str, d: f32| cfg.get(k).and_then(Value::as_f64).map(|x| x as f32).unwrap_or(d);
        let (del, add, cmd) = (txt("removed_prefix", "- "), txt("added_prefix", "+ "), txt("command_prefix", "$ "));
        let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let mut lines: Vec<(Option<Hsla>, Option<Hsla>, String)> = vec![];
        match name {
            "Edit" => {
                lines.extend(s("old_string").lines().map(|l| (c("removed_background", "@diff_del_bg"), c("removed", "@diff_del"), format!("{del}{l}"))));
                lines.extend(s("new_string").lines().map(|l| (c("added_background", "@diff_add_bg"), c("added", "@diff_add"), format!("{add}{l}"))));
            }
            "Write" => lines.extend(s("content").lines().map(|l| (c("added_background", "@diff_add_bg"), c("added", "@diff_add"), format!("{add}{l}")))),
            "Bash" => {
                lines.push((None, None, format!("{cmd}{}", s("command"))));
                lines.extend(result.unwrap_or("").lines().map(|l| (None, None, l.to_string())));
            }
            _ => {
                lines.extend(result.unwrap_or("").lines().map(|l| (None, if is_error { c("error", "@danger") } else { None }, l.to_string())));
                if lines.is_empty() && cfg.get("json_fallback").and_then(Value::as_bool) != Some(false) {
                    lines.push((None, None, serde_json::to_string_pretty(input).unwrap_or_default()));
                }
            }
        }
        let total = lines.len();
        let (pad_x, tab) = (num("padding_x", 8.), " ".repeat(num("tab_width", 4.) as usize));
        let mut code = v_flex().flex_none().min_w_full();
        for (bg, fg, text) in lines.into_iter().take(max_lines) {
            let text = if text.is_empty() { " ".to_string() } else { text.replace('\t', &tab) };
            code = code.child(div().px(px(pad_x)).whitespace_nowrap().when_some(bg, |d, b| d.bg(b)).when_some(fg, |d, f| d.text_color(f)).child(text));
        }
        if total > max_lines {
            let more = label("more_lines", "… {n} more lines", &[("n", &(total - max_lines).to_string())]);
            code = code.child(div().px(px(pad_x)).italic().child(more));
        }
        // Long lines scroll sideways with a horizontal swipe (never with the transcript's vertical scroll),
        // or with shift held, which holds the transcript still; `chat.code_scroll` tunes it.
        let cfg = self.settings.chat.get("code_scroll").cloned().unwrap_or(Value::Null);
        let shift = cfg.get("shift").and_then(Value::as_bool) != Some(false);
        let speed = cfg.get("speed").and_then(Value::as_f64).unwrap_or(1.) as f32;
        let frame_id = format!("{id}:frame");
        // "hover" shows the bar while the pointer is anywhere over the code (and while scrolling).
        let mode = match cfg.get("scrollbar").and_then(Value::as_str) {
            Some("always") => Some(gpui_kit::base::ScrollbarMode::Always),
            Some("scrolling") => Some(gpui_kit::base::ScrollbarMode::Scrolling),
            Some("never") => None,
            _ if HOVERED_CODE.with(|h| h.borrow().as_deref() == Some(frame_id.as_str())) => Some(gpui_kit::base::ScrollbarMode::Always),
            _ => Some(gpui_kit::base::ScrollbarMode::Scrolling),
        };
        let handle = self.scroll_handles.entry(format!("code:{id}")).or_default().clone();
        let lane = h_flex().id(SharedString::from(format!("{id}:code"))).items_start().overflow_x_scroll().restrict_scroll_to_axis().track_scroll(&handle).pb(px(num("padding_bottom", 12.))).child(code);
        let fid = frame_id.clone();
        let mut frame = style::apply(div().relative(), &st("step_code"), theme).id(SharedString::from(frame_id)).child(lane).on_hover(move |hovered, window, _| {
            HOVERED_CODE.with(|h| {
                let mut h = h.borrow_mut();
                if *hovered {
                    *h = Some(fid.clone());
                } else if h.as_deref() == Some(fid.as_str()) {
                    *h = None;
                }
            });
            window.refresh();
        });
        if shift {
            frame = frame.child(shift_scroll_layer(Some(handle.clone()), speed));
        }
        if let Some(mode) = mode {
            frame = frame.child(gpui_kit::component::scroll::Scrollbar::horizontal(&handle).mode(mode));
        }
        vec![frame.into_any_element()]
    }

    /** One Trace line: icon, title (pulsing while running), a chevron when it expands, and its content
    indented under a rule. Clicking toggles `key` in the expanded set. */
    #[allow(clippy::too_many_arguments)]
    fn trace_line(
        &mut self,
        key: &str,
        icon: &str,
        title: &str,
        running: bool,
        error: bool,
        expandable: bool,
        open: bool,
        body: Vec<AnyElement>,
        st: Styles,
        theme: &Map<String, Value>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let line_style = st("trace_line");
        let hover = line_style.get("hover").and_then(Value::as_object).cloned().unwrap_or_default();
        let fg = if error { style::color(&Value::from("@danger"), theme) } else { line_style.get("color").and_then(|v| style::color(v, theme)) };
        let look = LineLook::of(&self.settings.chat);
        let icon_el = Icon::empty().path(format!("icons/{icon}.svg")).size(px(look.icon_size)).when_some(fg, |i, c| i.text_color(c));
        let title_el = div().min_w_0().truncate().child(title.to_string());
        let title_el: AnyElement = if running && self.motion.borrow().enabled {
            let t = self.anim_phase(look.pulse_ms, cx);
            title_el.opacity(pulsating_between(look.pulse_min, look.pulse_max)(t)).into_any_element()
        } else {
            title_el.into_any_element()
        };
        let mut line = style::apply(h_flex().id(SharedString::from(format!("trace:{key}"))).max_w_full().min_w_0(), &line_style, theme)
            .when_some(fg, |d, c| d.text_color(c))
            .child(icon_el)
            .child(title_el);
        if expandable {
            let (hover, theme2, k) = (hover.clone(), theme.clone(), key.to_string());
            line = line
                .cursor_pointer()
                .hover(move |mut s| {
                    style::refine(&mut s, &hover, &theme2);
                    s
                })
                .child(
                    Icon::empty()
                        .path(format!("icons/{}.svg", if open { look.icon("open", "chevron-down") } else { look.icon("closed", "chevron-right") }))
                        .size(px(look.chevron_size))
                        .when_some(fg, |i, c| i.text_color(c)),
                )
                .on_click(cx.listener(move |ws, _, _, cx| {
                    if !ws.expanded_tools.remove(&k) {
                        ws.expanded_tools.insert(k.clone());
                    }
                    ws.pending_motion = true;
                    cx.notify();
                }));
        }
        let mut col = v_flex().min_w_0().child(line);
        if open && !body.is_empty() {
            col = col.child(style::apply(v_flex(), &st("trace_content"), theme).children(body));
        }
        col.into_any_element()
    }
}

thread_local! {
    static HOVERED_CODE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/** Over its parent (which must be `relative`), shift + scroll catches the wheel before anything under it:
with a handle it scrolls that handle sideways; without one it just holds still what would have scrolled. */
pub fn shift_scroll_layer(handle: Option<ScrollHandle>, speed: f32) -> impl IntoElement {
    canvas(
        |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |_, hitbox, window, _| {
            window.on_mouse_event(move |e: &ScrollWheelEvent, phase, window, cx| {
                if phase != DispatchPhase::Capture || !e.modifiers.shift || !hitbox.is_hovered(window) {
                    return;
                }
                if let Some(h) = &handle {
                    let d = e.delta.pixel_delta(window.line_height());
                    let step = if d.x.is_zero() { d.y } else { d.x };
                    let mut o = h.offset();
                    o.x = (o.x + step * speed).clamp(-h.max_offset().x, px(0.));
                    h.set_offset(o);
                    window.refresh();
                }
                cx.stop_propagation();
            });
        },
    )
    .absolute()
    .size_full()
    .top_0()
    .left_0()
}

#[cfg(test)]
mod tests {
    use super::{blocks_of, Block};
    use crate::transcript::Item;

    fn tool(done: bool) -> Item {
        Item::Tool { id: "t".into(), name: "Bash".into(), summary: String::new(), input: serde_json::Value::Null, result: done.then(String::new), is_error: false }
    }

    #[test]
    fn groups_steps_and_adds_the_working_line() {
        let items = vec![Item::User { text: "hi".into() }, tool(true), tool(true), Item::Assistant { text: "ok".into() }, tool(true)];
        let b = blocks_of(&items, true);
        assert!(matches!(b.as_slice(), [Block::One(0), Block::Steps(s), Block::One(3), Block::Steps(_), Block::Pending("Working")] if s.len() == 2));
        assert!(matches!(blocks_of(&items[..4], true).last(), Some(Block::Pending("Thinking"))));
        let running = vec![Item::User { text: "hi".into() }, tool(false)];
        assert!(matches!(blocks_of(&running, true).last(), Some(Block::Steps(_))), "a running tool shows its own progress");
        assert!(!blocks_of(&items, false).iter().any(|b| matches!(b, Block::Pending(_))));
    }
}

/** Markdown hides anything shaped like an HTML tag, so "Move to <name>" loses "<name>". Outside code,
`<` becomes `&lt;` (autolinks like <https://…> stay links). */
pub(crate) fn show_angle_brackets(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut fence: Option<&str> = None;
    for line in text.split_inclusive('\n') {
        let t = line.trim_start();
        if let Some(f) = fence {
            if t.starts_with(f) {
                fence = None;
            }
            out.push_str(line);
            continue;
        }
        if let Some(f) = ["```", "~~~"].into_iter().find(|f| t.starts_with(f)) {
            fence = Some(f);
            out.push_str(line);
            continue;
        }
        let mut ticks = 0;
        let mut chars = line.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            if c == '`' {
                let mut run = 1;
                while chars.peek().is_some_and(|(_, c)| *c == '`') {
                    chars.next();
                    run += 1;
                }
                ticks = if ticks == 0 { run } else if ticks == run { 0 } else { ticks };
                out.push_str(&"`".repeat(run));
                continue;
            }
            let rest = &line[i + c.len_utf8()..];
            if c == '<' && ticks == 0 && !["http://", "https://", "mailto:"].iter().any(|p| rest.starts_with(p)) {
                out.push_str("&lt;");
            } else {
                out.push(c);
            }
        }
    }
    out
}

#[cfg(test)]
mod angle_tests {
    #[test]
    fn keeps_placeholders_visible_outside_code() {
        let f = super::show_angle_brackets;
        assert_eq!(f("Move to <section name>"), "Move to &lt;section name>");
        assert_eq!(f("`a <b>` and <https://x.io>"), "`a <b>` and <https://x.io>");
        assert_eq!(f("```\n<div>\n```\n<x>"), "```\n<div>\n```\n&lt;x>");
        assert_eq!(f("café → <x> ✓"), "café → &lt;x> ✓");
    }
}

/** Actions this module handles (named in SETTINGS.md with the rest). */
pub(crate) const ACTIONS: &[(&str, &str)] = &[
    ("step.toggle / step.expand / step.collapse", "{chat?, id | index}: open or close one step line in a transcript (`id` as inspect.json views.transcripts lists it, or the item `index`)"),
    ("chat.detach", "{chat?, index | name}: take an attachment off a chat's composer (inspect.json views.transcripts[chat].attachments)"),
];

/** The expanded-set key for item `index` of a chat's transcript: a tool's id, or `CHAT:think:INDEX`. */
fn step_key(ws: &Workspace, chat: &str, index: usize) -> Option<String> {
    match ws.transcripts.get(chat)?.items.get(index)? {
        Item::Tool { id, .. } => Some(id.clone()),
        Item::Thinking { .. } => Some(format!("{chat}:think:{index}")),
        _ => None,
    }
}

/** Runs `name` if it's one of this module's actions; false otherwise. */
pub(crate) fn dispatch(ws: &mut crate::workspace::Workspace, name: &str, a: &serde_json::Map<String, serde_json::Value>, _window: &mut gpui_kit::Window, cx: &mut gpui_kit::Context<crate::workspace::Workspace>) -> bool {
    let s = |k: &str| a.get(k).and_then(Value::as_str).map(str::to_string).filter(|x| !x.is_empty());
    let chat = s("chat").or_else(|| ws.current_chat().map(|c| c.id.clone()));
    match name {
        "step.toggle" | "step.expand" | "step.collapse" => {
            let index = a.get("index").and_then(Value::as_u64).map(|i| i as usize);
            let key = s("id").or_else(|| chat.as_deref().zip(index).and_then(|(c, i)| step_key(ws, c, i)));
            if let Some(k) = key {
                let open = match name {
                    "step.expand" => true,
                    "step.collapse" => false,
                    _ => !ws.expanded_tools.contains(&k),
                };
                if open {
                    ws.expanded_tools.insert(k);
                } else {
                    ws.expanded_tools.remove(&k);
                }
                ws.pending_motion = true;
                cx.notify();
            }
            true
        }
        "chat.detach" => {
            if let Some(list) = chat.and_then(|c| ws.attachments.get_mut(&c)) {
                let index = a.get("index").and_then(Value::as_u64).map(|i| i as usize);
                match (index, s("name")) {
                    (Some(i), _) if i < list.len() => {
                        list.remove(i);
                    }
                    (_, Some(n)) => list.retain(|p| p.name != n),
                    _ => {}
                }
                cx.notify();
            }
            true
        }
        _ => false,
    }
}

/** What inspect.json shows about this module's views: per loaded transcript, its newest items
(`inspect.transcript_items`, default 20; text cut at `inspect.transcript_chars`, default 2000), which steps
are open, its running background work, and its composer's attachments. */
pub(crate) fn inspect(ws: &crate::workspace::Workspace) -> serde_json::Value {
    let n = ws.settings.num("inspect.transcript_items", 20.) as usize;
    let max = ws.settings.num("inspect.transcript_chars", 2000.) as usize;
    let cut = |t: &str| {
        if t.chars().count() > max {
            format!("{}…", t.chars().take(max).collect::<String>())
        } else {
            t.to_string()
        }
    };
    let mut out = Map::new();
    for (chat, t) in &ws.transcripts {
        let from = t.items.len().saturating_sub(n);
        let items: Vec<Value> = t.items[from..]
            .iter()
            .enumerate()
            .map(|(k, it)| {
                let index = from + k;
                let step = step_key(ws, chat, index);
                let open = step.as_ref().is_some_and(|s| ws.expanded_tools.contains(s));
                let mut v = match it {
                    Item::User { text } => serde_json::json!({"kind": "user", "text": cut(text)}),
                    Item::Assistant { text } => serde_json::json!({"kind": "assistant", "text": cut(text)}),
                    Item::Thinking { text } => serde_json::json!({"kind": "thinking", "text": cut(text)}),
                    Item::Notice { text } => serde_json::json!({"kind": "notice", "text": cut(text)}),
                    Item::Image { media_type, .. } => serde_json::json!({"kind": "image", "media_type": media_type}),
                    Item::Tool { name, summary, result, is_error, .. } => serde_json::json!({
                        "kind": "tool", "tool": name, "summary": summary, "is_error": is_error,
                        "has_result": result.is_some(), "text": result.as_deref().map(cut),
                    }),
                };
                if let Value::Object(o) = &mut v {
                    o.insert("index".into(), Value::from(index));
                    if let Some(s) = step {
                        o.insert("step".into(), Value::String(s));
                        o.insert("open".into(), Value::Bool(open));
                    }
                }
                v
            })
            .collect();
        let tool_ids: std::collections::HashSet<&str> = t.items.iter().filter_map(|i| if let Item::Tool { id, .. } = i { Some(id.as_str()) } else { None }).collect();
        let mut expanded: Vec<&String> = ws.expanded_tools.iter().filter(|k| k.starts_with(&format!("{chat}:")) || tool_ids.contains(k.as_str())).collect();
        expanded.sort();
        let background: Vec<Value> = t
            .background()
            .into_iter()
            .map(|b| serde_json::json!({"kind": b.kind, "label": b.label, "tool_id": b.tool_id, "task_id": b.task_id, "output": b.output}))
            .collect();
        out.insert(
            chat.clone(),
            serde_json::json!({
                "total": t.items.len(), "start": t.start, "lines": t.lines, "revision": t.revision,
                "waiting_results": t.orphan_count(),
                "rows": ws.lists.get(chat).map(|l| {
                    // Each row drawn now: its block (one:ITEM / steps:N / pending) and window bounds.
                    let kinds = l.kinds();
                    (0..l.state.item_count())
                        .filter_map(|ix| l.state.bounds_for_item(ix).map(|b| serde_json::json!({
                            "row": ix, "kind": kinds.get(ix), "x": f32::from(b.origin.x), "y": f32::from(b.origin.y),
                            "w": f32::from(b.size.width), "h": f32::from(b.size.height),
                        })))
                        .collect::<Vec<_>>()
                }),
                "items": items, "expanded": expanded, "background": background, "finished": t.finished_count(),
                "attachments": ws.attachments_json(chat),
            }),
        );
    }
    Value::Object(out)
}

#[cfg(test)]
mod look_tests {
    use super::describe;
    use serde_json::json;

    #[test]
    fn titles_read_as_before_and_follow_settings() {
        crate::transcript::set_options(&json!({}));
        let t = describe("Read", &json!({"file_path": "/a/1792164281008-shot.png"}), "", true);
        assert_eq!((t.0.as_str(), t.1.as_str(), t.2.as_str(), t.3.as_str()), ("Reading shot.png", "Read shot.png", "Couldn't read shot.png", "file-text"));
        let b = describe("Bash", &json!({"command": "ls -la"}), "", true);
        assert_eq!((b.1.as_str(), b.2.as_str()), ("Ls -la", "Failed: Ls -la"));
        assert_eq!(describe("TodoWrite", &json!({}), "", true).1, "Updated the plan");
        assert_eq!(describe("mcp__linear__get_issue", &json!({}), "X-1", true).1, "linear · get_issue: X-1");
        crate::transcript::set_options(&json!({"tools": {"Read": {"done": "Opened", "icon": "eye"}, "*": {"failed": "Broke:"}}, "tool_names": {"separator": "/"}}));
        let t = describe("Read", &json!({"file_path": "x.rs"}), "", true);
        assert_eq!((t.1.as_str(), t.3.as_str()), ("Opened x.rs", "eye"));
        assert_eq!(describe("mcp__a__b", &json!({}), "", true).2, "Broke: a/b");
        crate::transcript::set_options(&json!({}));
    }
}
