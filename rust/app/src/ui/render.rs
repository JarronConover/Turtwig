/*! The interpreter: turns a settings node into GPUI elements. Every node can repeat (`for`), hide
(`if`), style itself, react to events, carry a context menu and tooltip, and animate. */

use super::expr::{to_json, to_val, with};
use super::style;
use crate::workspace::Workspace;
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{Icon, Sizable as _};
use gpui_kit::*;
use jaq_json::Val;
use serde_json::{Map, Value};
use std::time::{Duration, Instant};

/** A style value easing toward its latest target: numbers use one lane, colors four (RGBA). */
pub struct Transition {
    from: Vec<f32>,
    to: Vec<f32>,
    start: Instant,
    duration: Duration,
    pub seen: u64,
}

impl Transition {
    fn value(&self) -> (Vec<f32>, bool) {
        let t = (self.start.elapsed().as_secs_f32() / self.duration.as_secs_f32().max(0.001)).min(1.);
        let e = 1. - (1. - t).powi(3);
        (self.from.iter().zip(&self.to).map(|(a, b)| a + (b - a) * e).collect(), t < 1.)
    }
}

/** Style keys that ease by themselves when `motion.style` is on. */
const AUTO_KEYS: &[&str] = &["background", "color", "border_color", "opacity", "radius", "width", "height", "font_size", "top", "left", "right", "bottom", "gap"];

pub const NODE_TYPES: &[(&str, &str)] = &[
    ("box", "A container. `style.direction` sets row or column (default column)."),
    ("row", "Children side by side."),
    ("column", "Children stacked."),
    ("stack", "Children layered on top of each other (later children on top); position them with `style.position: absolute`."),
    ("scroll", "A scrolling container. `props.axis`: y (default) | x | both."),
    ("text", "Text. `props.text` (templates allowed); a plain string node is shorthand for this."),
    ("icon", "A Lucide icon. `props.name` (e.g. git-branch, panel-left, plus), `props.size` (points), `props.spin` (bool)."),
    ("spinner", "A loading spinner. `props.size`: xsmall | small | medium | large."),
    ("spacer", "Takes the remaining space (grow 1)."),
    ("divider", "A 1-point line. `props.axis`: x (default, horizontal) | y."),
    ("markdown", "Rendered Markdown. `props.text`, `props.id` for a stable identity."),
    ("transcript", "The built-in chat transcript. `props.chat` (chat id; default the selected chat). Look: settings `chat`."),
    ("composer", "The built-in message box for a chat. `props.chat`, `props.placeholder`."),
    ("waiting", "The built-in card for a chat waiting on a permission or question, with its answer buttons. `props.chat`."),
    ("terminal", "The built-in terminal for a worktree. `props.path` (default the selected worktree). Look: settings `terminal`."),
    ("attachments", "Preview chips for files attached to a chat but not sent yet (spinner while uploading, × to remove). `props.chat` (default the selected chat), `props.size`. Look: settings `chat.attachments`, `chat.attachment`."),
    ("diff", "The built-in diff of a worktree: a file list beside the changes. `props.path` (default the selected worktree), `props.base` (default `state.diff_base`: uncommitted | branch). Click a file to jump, a file header to collapse, a line number to open it in the editor. Look: settings `diff`."),
    ("resize_handle", "A drag handle that sets a numeric state value. `props.state` (key), `props.axis` x|y, `props.min`, `props.max`, `props.invert` (bool, for handles on the far edge)."),
    ("<Component>", "Any name under `components`; its `props` are available inside as `.props`."),
];

impl Workspace {
    pub(crate) fn render_view(&mut self, name: &str, ctx: &Val, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(node) = self.settings.views.get(name).cloned() else {
            return div().child(format!("No view named `{name}` in settings.views")).into_any_element();
        };
        let mut els = self.render(&node, ctx, name, window, cx);
        if els.len() == 1 {
            els.pop().unwrap()
        } else {
            div().size_full().children(els).into_any_element()
        }
    }

    pub(crate) fn render(&mut self, node: &Value, ctx: &Val, path: &str, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let obj = match node {
            Value::String(s) => {
                let text = self.exprs.resolve(&Value::String(s.clone()), ctx);
                return vec![div().child(value_text(&text)).into_any_element()];
            }
            Value::Array(items) => {
                return items.iter().enumerate().flat_map(|(i, n)| self.render(n, ctx, &format!("{path}/{i}"), window, cx)).collect();
            }
            Value::Object(o) => o,
            _ => return vec![],
        };
        if let Some(code) = obj.get("for").and_then(Value::as_str) {
            let outs = self.exprs.run(code, ctx);
            let items: Vec<Val> = match outs.as_slice() {
                [Val::Arr(a)] => a.iter().cloned().collect(),
                _ => outs,
            };
            let name = obj.get("as").and_then(Value::as_str).unwrap_or("item").to_string();
            let mut node = obj.clone();
            node.remove("for");
            let node = Value::Object(node);
            let mut out = vec![];
            for (i, item) in items.into_iter().enumerate() {
                let c = with(ctx, vec![(name.clone(), item), ("index".into(), to_val(&Value::from(i)))]);
                let key = obj.get("key").and_then(Value::as_str).map(|k| value_text(&to_json(&self.exprs.first(k, &c)))).unwrap_or(i.to_string());
                out.extend(self.render(&node, &c, &format!("{path}[{key}]"), window, cx));
            }
            return out;
        }
        if let Some(code) = obj.get("if").and_then(Value::as_str) {
            if !self.exprs.truthy(code, ctx) {
                return vec![];
            }
        }
        let kind = obj.get("type").and_then(Value::as_str).unwrap_or("box").to_string();
        if let Some(comp) = self.settings.components.get(&kind).cloned() {
            let props = self.exprs.resolve(obj.get("props").unwrap_or(&Value::Null), ctx);
            let c = with(ctx, vec![("props".into(), to_val(&props))]);
            let mut comp = comp;
            if let Value::Object(base) = &mut comp {
                if let Some(Value::Object(extra)) = obj.get("style") {
                    if let Value::Object(st) = base.entry("style").or_insert(Value::Object(Map::new())) {
                        for (k, v) in extra {
                            st.insert(k.clone(), v.clone());
                        }
                    }
                }
                for k in ["id", "animate", "motion", "transition", "tooltip", "on", "menu"] {
                    if let Some(v) = obj.get(k) {
                        base.insert(k.into(), v.clone());
                    }
                }
            }
            return self.render(&comp, &c, &format!("{path}:{kind}"), window, cx);
        }
        let props = match self.exprs.resolve(obj.get("props").unwrap_or(&Value::Null), ctx) {
            Value::Object(p) => p,
            _ => Map::new(),
        };
        let mut st = match self.exprs.resolve(obj.get("style").unwrap_or(&Value::Null), ctx) {
            Value::Object(s) => s,
            _ => Map::new(),
        };
        if st.get("hidden").and_then(Value::as_bool) == Some(true) {
            return vec![];
        }
        let theme = self.settings.theme.clone();
        let id = match obj.get("id").and_then(Value::as_str) {
            Some(own) => format!("{own}{}", loop_keys(path)),
            None => path.to_string(),
        };
        self.apply_transitions(obj, &mut st, &id, window);
        self.apply_enter(obj, &mut st, &id, window);

        let children = |ws: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>| -> Vec<AnyElement> {
            match obj.get("children") {
                Some(Value::Array(kids)) => kids.iter().enumerate().flat_map(|(i, k)| ws.render(k, ctx, &format!("{path}/{i}"), window, cx)).collect(),
                Some(single) => ws.render(single, ctx, &format!("{path}/0"), window, cx),
                None => vec![],
            }
        };

        let base: Div = match kind.as_str() {
            "row" => div().flex().flex_row(),
            "column" | "box" | "scroll" | "resize_handle" => div().flex().flex_col(),
            "stack" => div().relative().flex().flex_col(),
            "spacer" => div().flex_1(),
            "divider" => {
                let c = style::color(&Value::from("@line"), &theme).unwrap_or(gpui_kit::black());
                if props.get("axis").and_then(Value::as_str) == Some("y") { div().w(px(1.)).h_full().bg(c) } else { div().h(px(1.)).w_full().bg(c) }
            }
            _ => div(),
        };
        let el = style::apply(base, &st, &theme);

        let content: Vec<AnyElement> = match kind.as_str() {
            "text" => vec![value_text(props.get("text").unwrap_or(&Value::Null)).into_any_element()],
            "icon" => {
                let name = props.get("name").and_then(Value::as_str).unwrap_or("circle");
                let size = props.get("size").and_then(Value::as_f64).unwrap_or(14.) as f32;
                let mut icon = Icon::empty().path(format!("icons/{name}.svg")).size(px(size));
                if let Some(c) = props.get("color").and_then(|c| style::color(c, &theme)) {
                    icon = icon.text_color(c);
                } else if let Some(c) = st.get("color").and_then(|c| style::color(c, &theme)) {
                    icon = icon.text_color(c);
                }
                if props.get("spin").and_then(Value::as_bool) == Some(true) {
                    vec![icon
                        .with_animation(SharedString::from(format!("{id}:spin")), Animation::new(Duration::from_millis(900)).repeat(), |i, d| {
                            i.rotate(percentage(d))
                        })
                        .into_any_element()]
                } else {
                    vec![icon.into_any_element()]
                }
            }
            "spinner" => {
                let s = Spinner::new();
                let s = match props.get("size").and_then(Value::as_str) {
                    Some("xsmall") => s.xsmall(),
                    Some("small") => s.small(),
                    Some("large") => s.large(),
                    _ => s,
                };
                let s = match props.get("color").and_then(|c| style::color(c, &theme)) {
                    Some(c) => s.color(c),
                    None => s,
                };
                vec![s.into_any_element()]
            }
            "markdown" => {
                let text = value_text(props.get("text").unwrap_or(&Value::Null));
                let mid = props.get("id").map(value_text).unwrap_or(id.clone());
                vec![TextView::markdown(SharedString::from(mid), text).into_any_element()]
            }
            "transcript" => {
                let chat = props.get("chat").and_then(Value::as_str).map(str::to_string).or_else(|| self.current_chat().map(|c| c.id.clone()));
                vec![self.transcript_view(chat, &props, window, cx)]
            }
            "composer" => {
                let chat = props.get("chat").and_then(Value::as_str).map(str::to_string).or_else(|| self.current_chat().map(|c| c.id.clone()));
                self.composer_view(chat, &props, window, cx).into_iter().collect()
            }
            "waiting" => {
                let chat = props.get("chat").and_then(Value::as_str).map(str::to_string).or_else(|| self.current_chat().map(|c| c.id.clone()));
                chat.and_then(|c| self.chat(&c).cloned()).filter(|c| c.state == "waiting").map(|c| self.waiting_card(&c, cx)).into_iter().collect()
            }
            "terminal" => {
                let path = props.get("path").and_then(Value::as_str).map(str::to_string).or_else(|| self.selected.clone());
                path.and_then(|p| self.terminals.get(&p).cloned()).map(|t| div().size_full().child(t).into_any_element()).into_iter().collect()
            }
            "diff" => vec![self.diff_view(&props, window, cx)],
            "attachments" => {
                let chat = props.get("chat").and_then(Value::as_str).map(str::to_string).or_else(|| self.current_chat().map(|c| c.id.clone()));
                self.attachments_view(chat, &props, cx).into_iter().collect()
            }
            "resize_handle" => vec![],
            "scroll" => vec![],
            _ => vec![],
        };
        let kids = children(self, window, cx);

        let mut el = el.id(SharedString::from(id.clone()));
        if kind == "scroll" {
            el = match props.get("axis").and_then(Value::as_str) {
                Some("x") => el.overflow_x_scroll(),
                Some("both") => el.overflow_scroll(),
                _ => el.overflow_y_scroll(),
            };
            if let Some(handle) = props.get("handle").and_then(Value::as_str) {
                let h = self.scroll_handles.entry(handle.to_string()).or_default().clone();
                el = el.track_scroll(&h);
            }
        }
        if kind == "resize_handle" {
            let key = props.get("state").and_then(Value::as_str).unwrap_or("").to_string();
            let vertical = props.get("axis").and_then(Value::as_str) == Some("y");
            let invert = props.get("invert").and_then(Value::as_bool) == Some(true);
            let (min, max) = (
                props.get("min").and_then(Value::as_f64).unwrap_or(80.) as f32,
                props.get("max").and_then(Value::as_f64).unwrap_or(2000.) as f32,
            );
            el = el
                .cursor(if vertical { CursorStyle::ResizeUpDown } else { CursorStyle::ResizeLeftRight })
                .on_mouse_down(MouseButton::Left, cx.listener(move |ws, e: &MouseDownEvent, _, cx| {
                    let start_size = ws.state.get(&key).and_then(Value::as_f64).unwrap_or(300.) as f32;
                    let start = if vertical { f32::from(e.position.y) } else { f32::from(e.position.x) };
                    ws.drag = Some(crate::workspace::Drag { key: key.clone(), vertical, invert, start, start_size, min, max });
                    cx.stop_propagation();
                }));
        }
        if let Some(Value::Object(h)) = st.get("hover") {
            let (h, theme) = (h.clone(), theme.clone());
            el = el.hover(move |mut s| {
                style::refine(&mut s, &h, &theme);
                s
            });
        }
        if let Some(Value::Object(d)) = st.get("drag_over") {
            let (d, theme) = (d.clone(), theme.clone());
            el = el.drag_over::<ExternalPaths>(move |mut s, _, _, _| {
                style::refine(&mut s, &d, &theme);
                s
            });
        }
        if let Some(Value::Object(a)) = st.get("active") {
            let (a, theme) = (a.clone(), theme.clone());
            el = el.active(move |mut s| {
                style::refine(&mut s, &a, &theme);
                s
            });
        }
        if let Some(Value::Object(on)) = obj.get("on") {
            let get = |k: &str| on.get(k).map(|a| self.exprs.resolve_action(a, ctx));
            let (click, double, down, drop) = (get("click"), get("double_click"), get("mouse_down"), get("drop"));
            if let Some(drop) = drop {
                el = el.on_drop(cx.listener(move |ws, paths: &ExternalPaths, window, cx| {
                    let list: Vec<Value> = paths.paths().iter().map(|p| Value::String(p.to_string_lossy().into_owned())).collect();
                    let mut action = drop.clone();
                    if let Value::Object(o) = &mut action {
                        o.insert("paths".into(), Value::Array(list));
                    }
                    ws.run_action(&action, window, cx);
                }));
            }
            for event in on.keys() {
                if !matches!(event.as_str(), "click" | "double_click" | "mouse_down" | "drop") {
                    self.exprs.note(format!("unknown event `{event}` at {path}"));
                }
            }
            if let Some(down) = down {
                el = el.on_mouse_down(MouseButton::Left, cx.listener(move |ws, e: &MouseDownEvent, window, cx| {
                    match (e.click_count, &double) {
                        (2, Some(d)) => ws.run_action(d, window, cx),
                        (1, _) => ws.run_action(&down, window, cx),
                        _ => {}
                    }
                }));
            } else if let Some(d) = double.clone() {
                el = el.on_click(cx.listener(move |ws, e: &ClickEvent, window, cx| {
                    if e.click_count() == 2 {
                        ws.run_action(&d, window, cx);
                    }
                }));
            }
            if let Some(c) = click {
                el = el.on_click(cx.listener(move |ws, e: &ClickEvent, window, cx| {
                    if e.click_count() < 2 {
                        ws.run_action(&c, window, cx);
                    }
                }));
            }
        }
        if let Some(tip) = obj.get("tooltip") {
            let tip = value_text(&self.exprs.resolve(tip, ctx));
            if !tip.is_empty() {
                el = el.tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx));
            }
        }
        let el = el.children(content).children(kids);
        let el: AnyElement = if let Some(Value::Array(menu)) = obj.get("menu") {
            let entries = self.menu_entries(menu, ctx);
            let view = cx.entity();
            el.context_menu(move |menu, window, _| {
                let mut m = menu;
                for (label, action, sep) in entries.iter() {
                    if *sep {
                        m = m.separator();
                        continue;
                    }
                    let action = action.clone();
                    m = m.item(PopupMenuItem::new(label.clone()).on_click(window.listener_for(&view, move |ws: &mut Workspace, _, window, cx| {
                        ws.run_action(&action, window, cx)
                    })));
                }
                m
            })
            .into_any_element()
        } else {
            el.into_any_element()
        };
        let moves = self.motion.borrow().enabled && obj.get("motion").and_then(Value::as_bool) != Some(false);
        vec![if moves { crate::ui::motion::Motion::new(id, el, self.motion.clone()).into_any_element() } else { el }]
    }

    /** Menu entries with `if` applied and `for` expanded: (label, action, is_separator). */
    fn menu_entries(&self, menu: &[Value], ctx: &Val) -> Vec<(String, Value, bool)> {
        let mut out = vec![];
        for m in menu {
            if let Some(code) = m.get("for").and_then(Value::as_str) {
                let name = m.get("as").and_then(Value::as_str).unwrap_or("item").to_string();
                let mut one = m.clone();
                if let Value::Object(o) = &mut one {
                    o.remove("for");
                }
                let items: Vec<Val> = match self.exprs.run(code, ctx).as_slice() {
                    [Val::Arr(a)] => a.iter().cloned().collect(),
                    other => other.to_vec(),
                };
                for (i, item) in items.into_iter().enumerate() {
                    let c = with(ctx, vec![(name.clone(), item), ("index".into(), to_val(&Value::from(i)))]);
                    out.extend(self.menu_entries(std::slice::from_ref(&one), &c));
                }
                continue;
            }
            if let Some(code) = m.get("if").and_then(Value::as_str) {
                if !self.exprs.truthy(code, ctx) {
                    continue;
                }
            }
            out.push((
                value_text(&self.exprs.resolve(m.get("label").unwrap_or(&Value::Null), ctx)),
                self.exprs.resolve_action(m.get("do").unwrap_or(&Value::Null), ctx),
                m.get("separator").and_then(Value::as_bool) == Some(true),
            ));
        }
        out
    }

    /** Enter animation: `animate` plays whenever the node appears; `motion.enter` plays for nodes that
    appear because of a change (a setting saved, a toggle), never on plain re-renders. */
    fn apply_enter(&mut self, obj: &Map<String, Value>, st: &mut Map<String, Value>, id: &str, window: &mut Window) {
        if !self.motion.borrow().enabled || obj.get("motion").and_then(Value::as_bool) == Some(false) {
            return;
        }
        let generation = self.render_generation;
        let running = self.entered.get(id).filter(|(_, seen, _)| *seen + 1 >= generation).map(|(start, _, spec)| (*start, spec.clone()));
        let (start, spec) = match running {
            Some(r) => r,
            None => {
                let m = self.motion.borrow();
                let appears = !m.was_visible(id);
                let by_change = m.frame == m.trigger && m.has_history();
                drop(m);
                let spec = match obj.get("animate") {
                    Some(a @ Value::Object(_)) => Some(a.clone()),
                    _ if by_change => self.settings.motion.get("enter").filter(|e| e.is_object()).cloned(),
                    _ => None,
                };
                match spec {
                    Some(spec) if appears => (Instant::now(), spec),
                    _ => return,
                }
            }
        };
        self.entered.insert(id.to_string(), (start, generation, spec.clone()));
        let ms = spec.get("duration").and_then(Value::as_u64).unwrap_or(200).max(1) as f32;
        let t = (start.elapsed().as_secs_f32() * 1000. / ms).min(1.);
        if t >= 1. {
            return;
        }
        window.request_animation_frame();
        let e = 1. - (1. - t).powi(4);
        let from = spec.get("from").cloned().unwrap_or(Value::Null);
        let f = |k: &str| from.get(k).and_then(Value::as_f64).map(|x| x as f32);
        if let Some(o) = f("opacity") {
            let base = st.get("opacity").and_then(Value::as_f64).unwrap_or(1.) as f32;
            st.insert("opacity".into(), Value::from((o + (base - o) * e) as f64));
        }
        for (k, side) in [("x", "left"), ("y", "top")] {
            if let Some(d) = f(k) {
                let base = st.get(side).and_then(Value::as_f64).unwrap_or(0.) as f32;
                st.insert(side.into(), Value::from((base + d * (1. - e)) as f64));
            }
        }
    }

    /** `transition` (and `motion.style` for common keys): numbers and colors ease from their old value
    to the new one. Dragging and first appearances snap. */
    fn apply_transitions(&mut self, obj: &Map<String, Value>, st: &mut Map<String, Value>, id: &str, window: &mut Window) {
        let enabled = self.motion.borrow().enabled && obj.get("motion").and_then(Value::as_bool) != Some(false);
        let mut keys: Vec<(String, u64)> = match obj.get("transition") {
            Some(Value::Object(tr)) => tr.iter().map(|(k, ms)| (k.clone(), ms.as_u64().unwrap_or(200))).collect(),
            _ => vec![],
        };
        if let Some(ms) = self.settings.motion.get("style").and_then(|s| s.get("duration")).and_then(Value::as_u64) {
            for k in AUTO_KEYS {
                if st.contains_key(*k) && !keys.iter().any(|(x, _)| x == k) {
                    keys.push((k.to_string(), ms));
                }
            }
        }
        let generation = self.render_generation;
        let snap = !enabled || self.drag.is_some();
        for (prop, ms) in keys {
            let Some(v) = st.get(&prop) else { continue };
            let (target, color) = match v {
                Value::Number(n) => (vec![n.as_f64().unwrap_or(0.) as f32], false),
                Value::String(_) => match style::color(v, &self.settings.theme) {
                    Some(c) => {
                        let r = Rgba::from(c);
                        (vec![r.r, r.g, r.b, r.a], true)
                    }
                    None => continue,
                },
                _ => continue,
            };
            let key = format!("{id}#{prop}");
            let shown = match self.transitions.get_mut(&key) {
                Some(t) if t.seen + 1 >= generation && !snap => {
                    t.seen = generation;
                    if t.to.len() == target.len() && t.to.iter().zip(&target).any(|(a, b)| (a - b).abs() > 0.001) {
                        let (now, _) = t.value();
                        *t = Transition { from: now, to: target, start: Instant::now(), duration: Duration::from_millis(ms), seen: generation };
                    }
                    let (v, running) = t.value();
                    if running {
                        window.request_animation_frame();
                    }
                    v
                }
                _ => {
                    self.transitions.insert(key, Transition { from: target.clone(), to: target.clone(), start: Instant::now(), duration: Duration::ZERO, seen: generation });
                    target
                }
            };
            let value = if color {
                let b = |x: f32| (x.clamp(0., 1.) * 255.).round() as u8;
                Value::String(format!("#{:02x}{:02x}{:02x}{:02x}", b(shown[0]), b(shown[1]), b(shown[2]), b(shown[3])))
            } else {
                Value::from(shown[0] as f64)
            };
            st.insert(prop, value);
        }
    }
}

/** The `[key]` segments of a path, so an `id` inside a `for` stays unique per item. */
fn loop_keys(path: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for ch in path.chars() {
        if ch == '[' {
            depth += 1;
        }
        if depth > 0 {
            out.push(ch);
        }
        if ch == ']' {
            depth -= 1;
        }
    }
    out
}

pub fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}
