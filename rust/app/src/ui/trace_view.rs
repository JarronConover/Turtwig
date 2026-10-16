/*! The transcript in the style of shadcn's Trace: user turns as right-aligned bubbles, replies as text,
and each run of thinking and tool calls as quiet one-line steps that expand; long runs fold. */

use super::style;
use crate::transcript::Item;
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
    if working && !(last_is_steps && tail_running) {
        blocks.push(Block::Pending(if last_is_steps { "Working" } else { "Thinking" }));
    }
    blocks
}

/** (running, done, failed) titles and an icon for a tool call. */
fn describe(name: &str, input: &Value, summary: &str) -> (String, String, String, &'static str) {
    let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let file = |k: &str| {
        let p = s(k);
        let name = p.rsplit('/').next().unwrap_or(&p).to_string();
        name.split_once('-').filter(|(stamp, _)| stamp.len() >= 10 && stamp.chars().all(|c| c.is_ascii_digit())).map(|(_, n)| n.to_string()).unwrap_or(name)
    };
    let three = |a: &str, b: &str, c: &str, rest: String, icon| (format!("{a} {rest}"), format!("{b} {rest}"), format!("{c} {rest}"), icon);
    match name {
        "Read" => three("Reading", "Read", "Couldn't read", file("file_path"), "file-text"),
        "Edit" | "MultiEdit" => three("Editing", "Edited", "Couldn't edit", file("file_path"), "pencil"),
        "Write" => three("Writing", "Wrote", "Couldn't write", file("file_path"), "file-plus"),
        "NotebookEdit" => three("Editing", "Edited", "Couldn't edit", file("notebook_path"), "notebook-pen"),
        "Bash" => {
            let what = if s("description").is_empty() { s("command").lines().next().unwrap_or("").to_string() } else { s("description") };
            let what = upper_first(&what);
            (what.clone(), what.clone(), format!("Failed: {what}"), "square-terminal")
        }
        "Grep" => three("Searching for", "Searched for", "Couldn't search for", format!("“{}”", s("pattern")), "search"),
        "Glob" => three("Finding", "Found", "Couldn't find", s("pattern"), "folder-search"),
        "WebFetch" => three("Fetching", "Fetched", "Couldn't fetch", s("url").trim_start_matches("https://").to_string(), "globe"),
        "WebSearch" => three("Searching the web for", "Searched the web for", "Couldn't search the web for", format!("“{}”", s("query")), "search"),
        "Agent" | "Task" => three("Delegating:", "Delegated:", "Delegation failed:", s("description"), "bot"),
        "TodoWrite" => ("Updating the plan".into(), "Updated the plan".into(), "Couldn't update the plan".into(), "list-todo"),
        "Skill" => three("Loading", "Loaded", "Couldn't load", format!("the {} skill", s("skill")), "sparkles"),
        "Compaction" => {
            let what = if input.get("trigger").and_then(Value::as_str) == Some("manual") { "Compacted the conversation" } else { "Compacted the conversation automatically" };
            (what.to_string(), what.to_string(), what.to_string(), "fold-vertical")
        }
        "AgentMessage" => {
            let what = format!("Message from {}{}", s("from"), if summary.is_empty() { String::new() } else { format!(": {summary}") });
            (what.clone(), what.clone(), what, "message-square")
        }
        _ => {
            let label = name.trim_start_matches("mcp__").replace("__", " · ");
            let rest = if summary.is_empty() { label.clone() } else { format!("{label}: {summary}") };
            (rest.clone(), rest.clone(), format!("Failed: {rest}"), "wrench")
        }
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
                let line = self.trace_line(&key, "brain", title, true, false, false, false, vec![], &st, &theme, cx);
                Some(style::apply(v_flex(), &st("trace"), &theme).child(line).into_any_element())
            }
            Block::Steps(steps) => {
                let active = working && bi + 1 == n;
                let mut trace = style::apply(v_flex(), &st("trace"), &theme);
                let key = format!("{chat_id}:run:{}", steps[0]);
                let folded = steps.len() > fold_after;
                let plural = |n: usize| if n == 1 { "" } else { "s" };
                if folded && !active {
                    let open = self.expanded_tools.contains(&key);
                    let tools = steps.iter().filter(|i| matches!(items[**i], Item::Tool { .. })).count();
                    let errors = steps.iter().filter(|i| matches!(items[**i], Item::Tool { is_error: true, .. })).count();
                    let failed = if errors > 0 { format!(", {errors} failed") } else { String::new() };
                    let title = format!("{tools} step{}{failed}", plural(tools));
                    let lines = if open { steps.iter().map(|&i| self.render_step(chat_id, i, &items[i], false, &st, &theme, cx)).collect() } else { vec![] };
                    trace = trace.child(self.trace_line(&key, "list-checks", &title, false, false, true, open, lines, &st, &theme, cx));
                } else {
                    let hidden = if active && folded { steps.len() - fold_after } else { 0 };
                    if hidden > 0 {
                        let open = self.expanded_tools.contains(&key);
                        let lines = if open { steps[..hidden].iter().map(|&i| self.render_step(chat_id, i, &items[i], false, &st, &theme, cx)).collect() } else { vec![] };
                        trace = trace.child(self.trace_line(&key, "list-checks", &format!("{hidden} earlier step{}", plural(hidden)), false, false, true, open, lines, &st, &theme, cx));
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
                let thumbs: Vec<AnyElement> = uploads
                    .into_iter()
                    .map(|p| {
                        if crate::ui::attach_view::is_image(&p) {
                            let (w, h) = crate::workspace::fit(&p, mw, mh);
                            style::apply(img(p).w(px(w)).h(px(h)).object_fit(ObjectFit::Contain), &image_style, theme).into_any_element()
                        } else {
                            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                            let name = name.split_once('-').filter(|(stamp, _)| stamp.chars().all(|c| c.is_ascii_digit())).map(|(_, n)| n.to_string()).unwrap_or(name);
                            style::apply(h_flex().gap(px(6.)), &st("file"), theme)
                                .child(Icon::empty().path("icons/file.svg").size(px(14.)))
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
        let view = TextView::markdown(SharedString::from(key.to_string()), show_angle_brackets(text)).selectable(selectable);
        match self.markdown_style(theme) {
            Some(st) => view.style(st),
            None => view,
        }
    }

    /** settings `chat.markdown`: paragraph_gap (rem), heading_sizes (h1..h6 points), and style objects
    for code_block, table, table_head, table_cell. Unset: the built-in look. */
    pub(crate) fn markdown_style(&self, theme: &Map<String, Value>) -> Option<gpui_kit::component::text::TextViewStyle> {
        let md = self.settings.chat.get("markdown").and_then(Value::as_object)?;
        let refine = |k: &str| {
            let mut r = StyleRefinement::default();
            if let Some(o) = md.get(k).and_then(Value::as_object) {
                style::refine(&mut r, o, theme);
            }
            r
        };
        let mut st = gpui_kit::component::text::TextViewStyle {
            code_block: refine("code_block"),
            table: refine("table"),
            table_head: refine("table_head"),
            table_cell: refine("table_cell"),
            is_dark: true,
            highlight_theme: gpui_kit::component::highlighter::HighlightTheme::default_dark(),
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

    /** A message with Copy and Fork under it while hovered (`chat.message_actions`: false hides them). */
    fn with_message_actions(&self, chat_id: &str, key: &str, text: &str, mine: bool, body: AnyElement, theme: &Map<String, Value>) -> AnyElement {
        if self.settings.chat.get("message_actions").and_then(Value::as_bool) == Some(false) || chat_id == crate::designer::CHAT_ID {
            return body;
        }
        let muted = style::color(&Value::from("@text_faint"), theme).unwrap_or(gpui_kit::black());
        let hover = style::color(&Value::from("@hover"), theme).unwrap_or(gpui_kit::black());
        let group = SharedString::from(format!("msg:{key}"));
        let view = self.view_handle.clone();
        let button = |id: String, icon: &str, tip: &'static str, action: Value| {
            let view = view.clone();
            div()
                .id(SharedString::from(id))
                .p(px(4.))
                .rounded(px(5.))
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip).build(w, cx))
                .child(Icon::empty().path(format!("icons/{icon}.svg")).size(px(13.)).text_color(muted))
                .on_click(move |_, window, cx| {
                    if let Some(v) = view.as_ref().and_then(|v| v.upgrade()) {
                        let action = action.clone();
                        v.update(cx, |ws, cx| ws.run_action(&action, window, cx));
                    }
                })
        };
        let bar = h_flex()
            .gap(px(2.))
            .h(px(22.))
            .when(mine, |d| d.justify_end())
            .invisible()
            .group_hover(group.clone(), |s| s.visible())
            .child(button(format!("{key}:copy"), "copy", "Copy", serde_json::json!({"do": "clipboard.copy", "text": text})))
            .child(button(format!("{key}:fork"), "git-fork", "Fork this chat (a new tab that starts from this conversation)", serde_json::json!({"do": "chat.fork", "chat": chat_id})));
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
                self.trace_line(&key, "brain", if running { "Thinking" } else { "Thought" }, running, false, has_text, open, body.into_iter().collect(), st, theme, cx)
            }
            Item::Tool { id, name, summary, input, result, is_error } => {
                let (doing, done, failed, icon) = describe(name, input, summary);
                let live = running && result.is_none();
                let title = if *is_error { failed } else if live { doing } else { done };
                let open = self.expanded_tools.contains(id);
                let body = if open { self.step_detail(id, name, input, result.as_deref(), *is_error, st, theme) } else { vec![] };
                self.trace_line(id, icon, &title, live, *is_error, true, open, body, st, theme, cx)
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
        let c = |k: &str| style::color(&Value::from(format!("@{k}")), theme);
        let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let mut lines: Vec<(Option<Hsla>, Option<Hsla>, String)> = vec![];
        match name {
            "Edit" => {
                lines.extend(s("old_string").lines().map(|l| (c("diff_del_bg"), c("diff_del"), format!("- {l}"))));
                lines.extend(s("new_string").lines().map(|l| (c("diff_add_bg"), c("diff_add"), format!("+ {l}"))));
            }
            "Write" => lines.extend(s("content").lines().map(|l| (c("diff_add_bg"), c("diff_add"), format!("+ {l}")))),
            "Bash" => {
                lines.push((None, None, format!("$ {}", s("command"))));
                lines.extend(result.unwrap_or("").lines().map(|l| (None, None, l.to_string())));
            }
            _ => {
                lines.extend(result.unwrap_or("").lines().map(|l| (None, if is_error { c("danger") } else { None }, l.to_string())));
                if lines.is_empty() {
                    lines.push((None, None, serde_json::to_string_pretty(input).unwrap_or_default()));
                }
            }
        }
        let total = lines.len();
        let mut code = v_flex().flex_none().min_w_full();
        for (bg, fg, text) in lines.into_iter().take(max_lines) {
            let text = if text.is_empty() { " ".to_string() } else { text.replace('\t', "    ") };
            code = code.child(div().px(px(8.)).whitespace_nowrap().when_some(bg, |d, b| d.bg(b)).when_some(fg, |d, f| d.text_color(f)).child(text));
        }
        if total > max_lines {
            code = code.child(div().px(px(8.)).italic().child(format!("… {} more lines", total - max_lines)));
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
        let lane = h_flex().id(SharedString::from(format!("{id}:code"))).items_start().overflow_x_scroll().restrict_scroll_to_axis().track_scroll(&handle).pb(px(12.)).child(code);
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
        let icon_el = Icon::empty().path(format!("icons/{icon}.svg")).size(px(14.)).when_some(fg, |i, c| i.text_color(c));
        let title_el = div().min_w_0().truncate().child(title.to_string());
        let title_el: AnyElement = if running && self.motion.borrow().enabled {
            let t = self.anim_phase(1400., cx);
            title_el.opacity(pulsating_between(0.35, 1.)(t)).into_any_element()
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
                .child(Icon::empty().path(if open { "icons/chevron-down.svg" } else { "icons/chevron-right.svg" }).size(px(12.)).when_some(fg, |i, c| i.text_color(c)))
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
