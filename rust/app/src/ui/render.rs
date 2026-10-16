/*! The interpreter: turns a settings node into GPUI elements. Every node can repeat (`for`), hide
(`if`), style itself, react to events, carry a context menu and tooltip, and animate. */

use super::expr::{to_json, to_val, with};
use super::style;
use crate::workspace::Workspace;
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_kit::component::text::TextView;
use gpui_kit::component::{h_flex, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
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
    ("scroll", "A scrolling container. `props.axis`: y (default) | x | both. `props.virtual`: true builds only the children on screen (each `for` item is one row), for long lists."),
    ("text", "Text. `props.text` (templates allowed); a plain string node is shorthand for this."),
    ("icon", "A Lucide icon. `props.name` (e.g. git-branch, panel-left, plus), `props.size` (points), `props.spin` (bool)."),
    ("logo", "The app's logo (settings `logo`) at `props.size` (24). Any `logo` key in props overrides it for this node, e.g. `{\"tile\": null}` for bare art."),
    ("input", "A one-line text field bound to a state key: `props.state` (key), `props.placeholder`, `props.autofocus`, `props.appearance` (false: no border). Read the text as `.state.<key>`. Or `props.value` (follows the context while the field isn't focused) with `on.submit` (an action run on Enter, with the text as `.input`)."),
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
    /** Where a looping animation of `period_ms` is now (0..1). Looping animations redraw at
    `motion.animation_fps` (0: they hold still) instead of every display frame, since a frame redraws the whole window. */
    pub(crate) fn anim_phase(&mut self, period_ms: f32, cx: &mut Context<Self>) -> f32 {
        let fps = self.settings.motion.get("animation_fps").and_then(Value::as_f64).unwrap_or(30.);
        if fps <= 0. {
            return 0.;
        }
        if self.anim_tick.is_none() {
            let wait = Duration::from_secs_f64(1. / fps.min(240.));
            self.anim_tick = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(wait).await;
                let _ = this.update(cx, |ws, cx| {
                    ws.anim_tick = None;
                    cx.notify();
                });
            }));
        }
        (self.anim_epoch.elapsed().as_secs_f32() * 1000. % period_ms) / period_ms
    }

    pub(crate) fn spinner(&mut self, size: Option<&str>, color: Option<Hsla>, cx: &mut Context<Self>) -> AnyElement {
        let t = self.anim_phase(800., cx);
        let icon = Icon::new(gpui_kit::component::IconName::Loader);
        let icon = match size {
            Some("xsmall") => icon.xsmall(),
            Some("small") => icon.small(),
            Some("large") => icon.large(),
            _ => icon,
        };
        let icon = match color {
            Some(c) => icon.text_color(c),
            None => icon,
        };
        div().child(icon.rotate(percentage(ease_in_out(t)))).into_any_element()
    }

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
            let props = self.exprs.resolve_val(obj.get("props").unwrap_or(&Value::Null), ctx);
            let c = with(ctx, vec![("props".into(), props)]);
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
                    let t = self.anim_phase(900., cx);
                    vec![icon.rotate(percentage(t)).into_any_element()]
                } else {
                    vec![icon.into_any_element()]
                }
            }
            "logo" => {
                let size = props.get("size").and_then(Value::as_f64).unwrap_or(24.) as f32;
                let spec = crate::logo::spec(&self.settings.logo, &props);
                let pixels = (size * window.scale_factor()).ceil() as u32;
                crate::logo::image(&spec, &theme, pixels.max(1))
                    .map(|i| img(i).size(px(size)).flex_none().into_any_element())
                    .into_iter()
                    .collect()
            }
            "input" => {
                use gpui_kit::component::input::{Input, InputEvent, InputState};
                let key = props.get("state").and_then(Value::as_str).unwrap_or("").to_string();
                let entity = match self.inputs.get(&id) {
                    Some(e) => e.clone(),
                    None => {
                        let placeholder = props.get("placeholder").map(value_text).unwrap_or_default();
                        let start = self.state.get(&key).map(value_text).unwrap_or_default();
                        let e = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).default_value(start));
                        let (k, input_id) = (key.clone(), id.clone());
                        self._subscriptions.push(cx.subscribe_in(&e, window, move |ws: &mut Workspace, e, ev: &InputEvent, window, cx| match ev {
                            InputEvent::Change if !k.is_empty() => {
                                let v = e.read(cx).value().to_string();
                                ws.set_state(k.clone(), Value::String(v));
                                cx.notify();
                            }
                            InputEvent::PressEnter { .. } => {
                                if let Some((action, ctx)) = ws.input_submits.get(&input_id).cloned() {
                                    let text = e.read(cx).value().to_string();
                                    let c = with(&ctx, vec![("input".into(), to_val(&Value::String(text)))]);
                                    let resolved = ws.exprs.resolve_action(&action, &c);
                                    ws.pending_actions.push(resolved);
                                    // Leave the field so it shows the saved value again.
                                    window.blur(cx);
                                    cx.notify();
                                }
                            }
                            _ => {}
                        }));
                        if props.get("autofocus").and_then(Value::as_bool) == Some(true) {
                            e.update(cx, |s, cx| s.focus(window, cx));
                        }
                        self.inputs.insert(id.clone(), e.clone());
                        e
                    }
                };
                // `value` follows the context while the field isn't being edited; `on.submit` runs on Enter with `.input`.
                if let Some(v) = props.get("value").map(value_text) {
                    let editing = entity.read(cx).focus_handle(cx).is_focused(window);
                    if !editing && entity.read(cx).value().as_ref() != v.as_str() {
                        entity.update(cx, |s, cx| s.set_value(v, window, cx));
                    }
                }
                match obj.get("on").and_then(|o| o.get("submit")) {
                    Some(a) => self.input_submits.insert(id.clone(), (a.clone(), ctx.clone())),
                    None => self.input_submits.remove(&id),
                };
                vec![Input::new(&entity).appearance(props.get("appearance").and_then(Value::as_bool).unwrap_or(true)).into_any_element()]
            }
            "spinner" => {
                let color = props.get("color").and_then(|c| style::color(c, &theme));
                vec![self.spinner(props.get("size").and_then(Value::as_str), color, cx)]
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
        let virtual_list = kind == "scroll" && props.get("virtual").and_then(Value::as_bool) == Some(true);
        let kids = if virtual_list { vec![self.virtual_list(obj, ctx, path, &id, cx)] } else { children(self, window, cx) };

        let mut el = el.id(SharedString::from(id.clone()));
        if kind == "input" {
            // Clicks in a field are for the field, not a drag or click handler around it.
            el = el.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        }
        if virtual_list {
            el = el.overflow_hidden();
        } else if kind == "scroll" {
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
        if let Some(Value::Object(spec)) = obj.get("drag") {
            let spec = self.exprs.resolve(&Value::Object(spec.clone()), ctx);
            let item = DragItem {
                kind: spec.get("kind").and_then(Value::as_str).unwrap_or("item").to_string(),
                value: spec.get("value").cloned().unwrap_or(Value::Null),
                label: spec.get("label").map(value_text).unwrap_or_default(),
            };
            let preview_style = self.settings.theme.get("drag_preview").and_then(Value::as_object).cloned().unwrap_or_default();
            let theme2 = theme.clone();
            let (dragged, note) = (self.drag_item.clone(), self.drag_note.clone());
            el = el.on_drag(item, move |item, _, _, cx| {
                *dragged.borrow_mut() = Some((item.kind.clone(), item.value.clone()));
                note.borrow_mut().clear();
                cx.new(|_| DragPreview { label: item.label.clone(), note: note.clone(), style: preview_style.clone(), theme: theme2.clone() })
            });
        }
        // A press that drags, resizes, or clicks something isn't the start of a text selection; otherwise the
        // chat keeps selecting as the pointer moves after a drop (the drop eats the mouse-up).
        let presses = obj.contains_key("drag") || kind == "resize_handle" || obj.get("on").and_then(Value::as_object).is_some_and(|o| o.contains_key("click") || o.contains_key("double_click") || o.contains_key("mouse_down"));
        if presses {
            el = el.on_mouse_down(MouseButton::Left, |_, _, cx| gpui_kit::base::GlobalState::suppress_text_selection(cx));
        }
        if let Some(Value::Object(d)) = st.get("drag_over_item") {
            let accept = accepted(obj.get("on").and_then(|o| o.get("drop_item")));
            let (d, theme) = (d.clone(), theme.clone());
            el = el.drag_over::<DragItem>(move |mut s, item, _, _| {
                if accept.is_empty() || accept.contains(&item.kind) {
                    style::refine(&mut s, &d, &theme);
                }
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
            if let Some(spec) = on.get("drop_item") {
                let drop_key = obj.get("drop_key").map(|k| value_text(&self.exprs.resolve(k, ctx))).unwrap_or_else(|| id.clone());
                let zones = self.exprs.resolve(obj.get("drop_zones").unwrap_or(&Value::Null), ctx);
                let specs: std::rc::Rc<Vec<(Vec<String>, Value, Option<String>)>> = std::rc::Rc::new(
                    match spec {
                        Value::Array(list) => list.clone(),
                        one => vec![one.clone()],
                    }
                    .iter()
                    .map(|s| {
                        let action = self.exprs.resolve_action(s.get("do").unwrap_or(&Value::Null), ctx);
                        (accepted(Some(s)), action, s.get("valid").and_then(Value::as_str).map(str::to_string))
                    })
                    .collect(),
                );
                let (k2, z2, specs2, ctx2) = (drop_key.clone(), zones.clone(), specs.clone(), ctx.clone());
                el = el.on_drag_move::<DragItem>(cx.listener(move |ws, e: &DragMoveEvent<DragItem>, _, cx| {
                    let item = e.drag(cx).clone();
                    let mine = ws.drop_hint.as_ref().is_some_and(|h| h.key == k2);
                    let clear = |ws: &mut Workspace, cx: &mut Context<Workspace>| {
                        if mine {
                            ws.drop_hint = None;
                            cx.notify();
                        }
                    };
                    let spec = specs2.iter().find(|(accept, _, _)| accept.is_empty() || accept.contains(&item.kind));
                    let Some((_, _, valid)) = spec.filter(|_| e.bounds.contains(&e.event.position)) else { return clear(ws, cx) };
                    let mode = match &z2 {
                        Value::String(m) => m.clone(),
                        Value::Object(by_kind) => by_kind.get(&item.kind).map(value_text).unwrap_or_else(|| "inside".into()),
                        _ => "inside".into(),
                    };
                    let rel = (f32::from(e.event.position.y - e.bounds.origin.y) / f32::from(e.bounds.size.height).max(1.)).clamp(0., 1.);
                    let rel_x = (f32::from(e.event.position.x - e.bounds.origin.x) / f32::from(e.bounds.size.width).max(1.)).clamp(0., 1.);
                    let zone = match mode.as_str() {
                        "edges" => if rel < 0.5 { "before" } else { "after" },
                        "edges_x" => if rel_x < 0.5 { "before" } else { "after" },
                        "edges+inside" => if rel < 0.25 { "before" } else if rel > 0.75 { "after" } else { "inside" },
                        _ => "inside",
                    };
                    if let Some(code) = valid {
                        let c = with(&ctx2, vec![("dragged".into(), to_val(&item.value)), ("zone".into(), to_val(&Value::from(zone)))]);
                        if !ws.exprs.truthy(code, &c) {
                            return clear(ws, cx);
                        }
                    }
                    let hint = DropHint { key: k2.clone(), zone: zone.into(), kind: item.kind.clone(), value: item.value.clone() };
                    if ws.drop_hint.as_ref() != Some(&hint) {
                        ws.drop_hint = Some(hint);
                        cx.notify();
                    }
                }));
                el = el.on_drop(cx.listener(move |ws, item: &DragItem, window, cx| {
                    let Some((_, action, _)) = specs.iter().find(|(accept, _, _)| accept.is_empty() || accept.contains(&item.kind)) else { return };
                    let Some(hint) = ws.drop_hint.take().filter(|h| h.key == drop_key) else { return };
                    let mut action = action.clone();
                    let fill = |o: &mut Map<String, Value>| {
                        o.insert("dragged".into(), item.value.clone());
                        o.insert("zone".into(), Value::String(hint.zone.clone()));
                    };
                    match &mut action {
                        Value::Object(o) => fill(o),
                        Value::Array(list) => list.iter_mut().filter_map(Value::as_object_mut).for_each(fill),
                        _ => {}
                    }
                    ws.run_action(&action, window, cx);
                    cx.stop_propagation();
                }));
            }
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
                if !matches!(event.as_str(), "click" | "double_click" | "mouse_down" | "drop" | "drop_item") && !(event == "submit" && kind == "input") {
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
            if let Some(c) = click.clone() {
                let label = obj.get("tooltip").map(|t| value_text(&self.exprs.resolve(t, ctx))).filter(|t| !t.is_empty()).unwrap_or_default();
                self.clickables.push((id.clone(), label, c));
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
        let mut el = el.children(content).children(kids);
        if obj.get("layer").and_then(Value::as_str) == Some("top") || obj.get("occlude").and_then(Value::as_bool) == Some(true) {
            el = el.occlude();
        }
        let el: AnyElement = if let Some(Value::Array(menu)) = obj.get("menu") {
            // Entries are worked out when the menu opens, not on every frame.
            let (spec, menu_ctx) = (menu.clone(), ctx.clone());
            let view = cx.entity();
            el.context_menu(move |menu, window, cx| {
                let entries = view.read(cx).menu_entries(&spec, &menu_ctx);
                let theme = view.read(cx).settings.theme.clone();
                build_menu(menu, entries, &view, &theme, window, cx)
            })
            .into_any_element()
        } else {
            el.into_any_element()
        };
        let moves = self.motion.borrow().enabled && obj.get("motion").and_then(Value::as_bool) != Some(false);
        let el = if moves { crate::ui::motion::Motion::new(id, el, self.motion.clone()).into_any_element() } else { el };
        // "layer": "top" paints the node above everything else (popovers, dialogs) and keeps clicks off what's under it.
        if obj.get("layer").and_then(Value::as_str) == Some("top") {
            return vec![deferred(el).with_priority(1).into_any_element()];
        }
        vec![el]
    }

    /** A `scroll` with `props.virtual`: its children (each `for` item counts as one) become rows of a list
    that builds and lays out only the rows on screen. */
    fn virtual_list(&mut self, obj: &Map<String, Value>, ctx: &Val, path: &str, id: &str, cx: &mut Context<Self>) -> AnyElement {
        let kids: Vec<Value> = match obj.get("children") {
            Some(Value::Array(k)) => k.clone(),
            Some(one) => vec![one.clone()],
            None => vec![],
        };
        let mut rows: Vec<(Value, Val, String)> = vec![];
        for (i, kid) in kids.iter().enumerate() {
            let at = format!("{path}/{i}");
            let Some(code) = kid.get("for").and_then(Value::as_str) else {
                rows.push((kid.clone(), ctx.clone(), at));
                continue;
            };
            let items: Vec<Val> = match self.exprs.run(code, ctx).as_slice() {
                [Val::Arr(a)] => a.iter().cloned().collect(),
                other => other.to_vec(),
            };
            let name = kid.get("as").and_then(Value::as_str).unwrap_or("item").to_string();
            let mut one = kid.clone();
            if let Value::Object(o) = &mut one {
                o.remove("for");
            }
            for (j, item) in items.into_iter().enumerate() {
                let c = with(ctx, vec![(name.clone(), item), ("index".into(), to_val(&Value::from(j)))]);
                let key = kid.get("key").and_then(Value::as_str).map(|k| value_text(&to_json(&self.exprs.first(k, &c)))).unwrap_or(j.to_string());
                rows.push((one.clone(), c, format!("{at}[{key}]")));
            }
        }
        let keys: Vec<String> = rows.iter().map(|r| r.2.clone()).collect();
        let entry = self.virtual_lists.entry(id.to_string()).or_insert_with(|| (ListState::new(0, ListAlignment::Top, px(400.)), vec![], Default::default()));
        let state = entry.0.clone();
        if entry.1 != keys {
            let (old, new) = (&entry.1, &keys);
            let head = old.iter().zip(new).take_while(|(a, b)| a == b).count();
            let tail = old[head..].iter().rev().zip(new[head..].iter().rev()).take_while(|(a, b)| a == b).count();
            state.splice(head..old.len() - tail, new.len() - tail - head);
            entry.1 = keys;
        }
        // Rows off screen keep their last height until they scroll into view; content may have changed.
        state.remeasure_items(0..rows.len());
        entry.2 = std::rc::Rc::new(rows);
        let list_id = id.to_string();
        list(state, cx.processor(move |ws: &mut Workspace, ix: usize, window, cx| {
            let Some(row) = ws.virtual_lists.get(&list_id).and_then(|l| l.2.get(ix).cloned()) else { return div().into_any_element() };
            div().w_full().flex().flex_col().children(ws.render(&row.0, &row.1, &row.2, window, cx)).into_any_element()
        }))
        .size_full()
        .into_any_element()
    }

    /** Menu entries with `if` applied and `for` expanded. */
    pub(crate) fn menu_entries(&self, menu: &[Value], ctx: &Val) -> Vec<MenuEntry> {
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
            if m.get("separator").and_then(Value::as_bool) == Some(true) {
                if !matches!(out.last(), None | Some(MenuEntry::Separator)) {
                    out.push(MenuEntry::Separator);
                }
                continue;
            }
            let text = |k: &str| m.get(k).map(|v| value_text(&self.exprs.resolve(v, ctx))).filter(|t| !t.is_empty());
            let flag = |k: &str| match m.get(k) {
                Some(Value::Bool(b)) => *b,
                Some(Value::String(code)) => self.exprs.truthy(code, ctx),
                _ => false,
            };
            let label = text("label").unwrap_or_default();
            if let Some(Value::Array(items)) = m.get("items") {
                let children = self.menu_entries(items, ctx);
                if !children.is_empty() {
                    out.push(MenuEntry::Submenu { label, icon: text("icon"), children });
                }
                continue;
            }
            out.push(MenuEntry::Item {
                label,
                action: self.exprs.resolve_action(m.get("do").unwrap_or(&Value::Null), ctx),
                icon: text("icon"),
                shortcut: text("shortcut"),
                checked: m.get("checked").map(|_| flag("checked")),
                danger: flag("danger"),
                disabled: flag("disabled"),
            });
        }
        while matches!(out.last(), Some(MenuEntry::Separator)) {
            out.pop();
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

/** What a node with `drag` carries; drop targets with `on.drop_item` get `value` as the action's `dragged`. */
#[derive(Clone)]
pub struct DragItem {
    pub kind: String,
    pub value: Value,
    pub label: String,
}

/** Where a dragged item would land: the target's `drop_key` and before | inside | after. */
#[derive(Clone, PartialEq)]
pub struct DropHint {
    pub key: String,
    pub zone: String,
    pub kind: String,
    /** What's being dragged (a worktree's path, a chat id). */
    pub value: Value,
}

/** The pill that follows the pointer while dragging. Look: theme `drag_preview`. */
pub struct DragPreview {
    label: String,
    /** A second line set while dragging: where a drop would move it ("Move to Email on Ask Redo"). */
    note: std::rc::Rc<std::cell::RefCell<String>>,
    style: Map<String, Value>,
    theme: Map<String, Value>,
}

impl Render for DragPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let note = self.note.borrow().clone();
        let accent = style::color(&Value::String("@accent".into()), &self.theme);
        style::apply(div(), &self.style, &self.theme)
            .child(self.label.clone())
            .when(!note.is_empty(), |d| d.child(div().text_size(px(11.5)).when_some(accent, |d, c| d.text_color(c)).child(note)))
    }
}

fn accepted(spec: Option<&Value>) -> Vec<String> {
    if let Some(Value::Array(list)) = spec {
        return list.iter().flat_map(|s| accepted(Some(s))).collect();
    }
    match spec.and_then(|s| s.get("accept")) {
        Some(Value::String(k)) => vec![k.clone()],
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        _ => vec![],
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

/** One line of a right-click menu (settings `menu`). Items take `icon`, `shortcut` (shown at the right),
`checked` (a checkbox), `danger` (red), and `disabled`; `items` makes a submenu. */
#[derive(Clone)]
pub(crate) enum MenuEntry {
    Separator,
    Item { label: String, action: Value, icon: Option<String>, shortcut: Option<String>, checked: Option<bool>, danger: bool, disabled: bool },
    Submenu { label: String, icon: Option<String>, children: Vec<MenuEntry> },
}

fn build_menu(
    mut m: gpui_kit::component::menu::PopupMenu,
    entries: Vec<MenuEntry>,
    view: &Entity<Workspace>,
    theme: &Map<String, Value>,
    window: &mut Window,
    cx: &mut Context<gpui_kit::component::menu::PopupMenu>,
) -> gpui_kit::component::menu::PopupMenu {
    let danger_color = style::color(&Value::String("@danger".into()), theme);
    let faint = style::color(&Value::String("@text_faint".into()), theme);
    for e in entries {
        match e {
            MenuEntry::Separator => m = m.separator(),
            MenuEntry::Submenu { label, icon, children } => {
                let (view, theme) = (view.clone(), theme.clone());
                m = m.submenu_with_icon(icon.map(menu_icon), label, window, cx, move |sub, window, cx| {
                    build_menu(sub, children.clone(), &view, &theme, window, cx)
                });
            }
            MenuEntry::Item { label, action, icon, shortcut, checked, danger, disabled } => {
                let color = if danger { danger_color } else { None };
                let item = if color.is_some() || shortcut.is_some() {
                    let (label, shortcut) = (SharedString::from(label), shortcut.map(SharedString::from));
                    PopupMenuItem::element(move |_, _| {
                        h_flex()
                            .w_full()
                            .gap_4()
                            .justify_between()
                            .child(div().when_some(color, |d, c| d.text_color(c)).child(label.clone()))
                            .when_some(shortcut.clone(), |d, k| d.child(div().when_some(faint, |d, c| d.text_color(c)).child(k)))
                    })
                } else {
                    PopupMenuItem::new(label)
                };
                let accent = style::color(&Value::String("@accent".into()), theme);
                let item = match (checked, icon, color) {
                    (Some(true), _, _) => item.icon(menu_icon("square-check".into()).when_some(accent, |i, c| i.text_color(c))),
                    (Some(false), _, _) => item.icon(menu_icon("square".into())),
                    (None, Some(i), Some(c)) => item.icon(menu_icon(i).text_color(c)),
                    (None, Some(i), None) => item.icon(menu_icon(i)),
                    _ => item,
                };
                let item = item.disabled(disabled);
                let view = view.clone();
                m = m.item(item.on_click(window.listener_for(&view, move |ws: &mut Workspace, _, window, cx| ws.run_action(&action, window, cx))));
            }
        }
    }
    m
}

fn menu_icon(name: String) -> Icon {
    Icon::empty().path(format!("icons/{name}.svg"))
}
