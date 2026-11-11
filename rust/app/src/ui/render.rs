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
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

thread_local! {
    /** The context `views.main` was drawn with this frame, for parts drawn outside the node tree (list rows). */
    static FRAME_CTX: std::cell::RefCell<Option<Val>> = const { std::cell::RefCell::new(None) };
}

pub(crate) fn frame_ctx() -> Val {
    FRAME_CTX.with(|c| c.borrow().clone()).unwrap_or_else(|| to_val(&Value::Object(Map::new())))
}

/** A style value easing toward its latest target: numbers use one lane, colors four (RGBA). */
pub struct Transition {
    from: Vec<f32>,
    to: Vec<f32>,
    start: Instant,
    duration: Duration,
    pub seen: u64,
    easing: crate::ui::motion::Easing,
}

impl Transition {
    fn value(&self) -> (Vec<f32>, bool) {
        let t = (self.start.elapsed().as_secs_f32() / self.duration.as_secs_f32().max(0.001)).min(1.);
        let e = self.easing.apply(t);
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
    ("scroll", "A scrolling container. `props.axis`: y (default) | x | both. `props.scrollbar`: never (default) | scrolling | hover | always. `props.handle` names it for `ui.scroll` (default: its id). `props.virtual`: true builds only the children on screen (each `for` item is one row), for long lists; then `props.align` top | bottom (rows sit at the bottom when they don't fill it), `props.follow` (stay at the end as rows are added), `props.overdraw` (points drawn past each edge, 400); each row is a `region` unless `props.region_rows` is false."),
    ("text", "Text. `props.text` (templates allowed); a plain string node is shorthand for this. `props.selectable`: true lets it be selected and copied."),
    ("icon", "A Lucide icon. `props.name` (e.g. git-branch, panel-left, plus), `props.size` (points), `props.spin` (bool)."),
    ("image", "A picture, animated GIF/WebP, or video (mp4, mov, m4v, webm; muted, looping): `props.src` (a file: absolute, ~/, or relative to the settings folder; or an http(s) URL) and `props.fit` (cover, contain, fill, scale_down, none). Size it with `style`; settings `background` puts one behind the whole window."),
    ("logo", "The app's logo (settings `logo`) at `props.size` (24). Any `logo` key in props overrides it for this node, e.g. `{\"tile\": null}` for bare art."),
    ("spacer", "Takes the remaining space (grow 1)."),
    ("divider", "A 1-point line. `props.axis`: x (default, horizontal) | y."),
    ("markdown", "Rendered Markdown. `props.text`, `props.id` for a stable identity, `props.selectable` (default `chat.selectable`). Looks like settings `markdown` (same keys as `chat.markdown`, which it defaults to), with `props.markdown` over it."),
    ("select", "A dropdown: a button showing the chosen option that opens a menu of `props.options` ([values] or [{value, label}]) bound to `props.state` (or `props.value`), `props.placeholder` when nothing is chosen, `props.icon` (chevron-down). `on.change` gets `.value`. Look: `theme.select` and `style`. Agents: `ui.menu {id, item}` or `ui.click {id: \"ID:VALUE\"}`."),
    ("transcript", "The built-in chat transcript. `props.chat` (chat id; default the selected chat). Look: settings `chat`."),
    ("composer", "The built-in message box for a chat. `props.chat`, `props.placeholder`."),
    ("screen", "A chat's live terminal while its agent shows a screen of its own (`.chat.screen`, a menu like Claude Code's /artifacts): typing goes to the agent. `props.chat`."),
    ("waiting", "The built-in card for a chat waiting on a permission or question, with its answer buttons. `props.chat`."),
    ("terminal", "The built-in terminal for a worktree. `props.path` (default the selected worktree). Look: settings `terminal`."),
    ("attachments", "Preview chips for files attached to a chat but not sent yet (spinner while uploading, × to remove). `props.chat` (default the selected chat), `props.size`. Look: settings `chat.attachments`, `chat.attachment`."),
    ("diff", "The built-in diff of a worktree: a file list beside the changes. `props.path` (default the selected worktree), `props.base` (default `state.diff_base`: uncommitted | branch). Click a file to jump, a file header to collapse, a line number to open it in the editor. Look: settings `diff`."),
    ("resize_handle", "A drag handle that sets a numeric state value. `props.state` (key), `props.axis` x|y, `props.min` (80), `props.max` (2000), `props.default` (300, the size while the state is unset), `props.invert` (bool, for handles on the far edge). `on.double_click` works on it (e.g. a state.set back to a size)."),
    ("(every node)", "Besides `type`, `props`, `style`, `children`: `id` (stable identity; listed in inspect.json `layout`), `for`/`as`/`key`, `if`, `on` (events: click, double_click, mouse_down, mouse_up, right_click, hover_start, hover_end, scroll (`.dx`/`.dy` added to the action), key ({KEY: action} or [{key, do}], while focused), focus, blur, click_outside, drop, drop_item, change, submit), `focusable` (true: takes keyboard focus; Enter/Space click it, Tab moves on; `.focused_node` is the focused node's id), `menu` (entries) with `menu_on` (right_click (default) | click), `tooltip`, `drag`, `drop_key`, `drop_zones`, `layer` (top | above | a number: paint above lower layers), `occlude`, `animate`, `motion`, `transition`, `region` (true: drawn as its own cached view, again only when what its expressions read changes or an animation inside it asks; a big area that changes on its own (a list, the transcript, a panel) as a region keeps the rest of the window still. Its size comes from its style, or its content's along its parent's direction; regions inside a region that redraws redraw with it)."),
    ("<Component>", "Any name under `components`; its `props` are available inside as `.props`. `style` merges into its root's, and `id`, `on`, `menu`, `tooltip`, `drag`, `drop_key`, `drop_zones`, `layer`, `animate`, `motion` and `transition` set on the use replace its root's."),
];

impl Workspace {
    /** Where a looping animation of `period_ms` is now (0..1). Looping animations redraw at
    `motion.animation_fps` (0: they hold still) instead of every display frame, since a frame redraws the whole window. */
    pub(crate) fn anim_phase(&mut self, period_ms: f32, cx: &mut Context<Self>) -> f32 {
        let fps = self.settings.motion.get("animation_fps").and_then(Value::as_f64).unwrap_or(60.);
        self.anim_phase_at(period_ms, fps, cx)
    }

    /** A slow fade (a running step's pulse) at `motion.pulse_fps`: smooth at far fewer redraws than a spin. */
    pub(crate) fn pulse_phase(&mut self, period_ms: f32, cx: &mut Context<Self>) -> f32 {
        let fps = self.settings.motion.get("pulse_fps").and_then(Value::as_f64).unwrap_or(20.);
        self.anim_phase_at(period_ms, fps, cx)
    }

    fn anim_phase_at(&mut self, period_ms: f32, fps: f64, cx: &mut Context<Self>) -> f32 {
        if fps <= 0. {
            return 0.;
        }
        // Only the region drawing it redraws (or the window, outside any), on the display frame its next
        // frame is due (its view asks once drawn); a slow timer stands by in case nothing does.
        let owner = super::region::owner();
        super::region::want_frame(&owner, fps);
        let key = format!("{owner}@{}", fps.round());
        if !self.anim_ticks.contains_key(&key) {
            let wait = Duration::from_millis(250);
            let (o, k) = (owner, key.clone());
            let task = cx.spawn(async move |this, cx| {
                cx.background_executor().timer(wait).await;
                let _ = this.update(cx, |ws, cx| {
                    ws.anim_ticks.remove(&k);
                    ws.notify_owner(&o, cx);
                });
            });
            self.anim_ticks.insert(key, task);
        }
        (self.anim_epoch.elapsed().as_secs_f32() * 1000. % period_ms) / period_ms
    }

    pub(crate) fn spinner(&mut self, size: Option<&str>, color: Option<Hsla>, cx: &mut Context<Self>) -> AnyElement {
        let period = self.settings.num("motion.spinner.period_ms", 800.) as f32;
        let icon = match self.settings.get("motion.spinner.icon").and_then(Value::as_str) {
            Some(name) => Icon::empty().path(format!("icons/{name}.svg")),
            None => Icon::new(gpui_kit::component::IconName::Loader),
        };
        let icon = match size {
            Some("xsmall") => icon.xsmall(),
            Some("small") => icon.small(),
            Some("large") => icon.large(),
            _ => icon,
        };
        self.turning(icon, color, period, true, cx)
    }

    /** A spinning `icon`: turned by the anim layer (see ui::anim), else by redrawing what it's in. */
    pub(crate) fn turning(&mut self, icon: Icon, color: Option<Hsla>, period_ms: f32, ease: bool, cx: &mut Context<Self>) -> AnyElement {
        if super::anim::enabled() {
            return super::anim::slot(super::region::owner(), icon, color, period_ms, ease);
        }
        let t = self.anim_phase(period_ms, cx);
        let t = if ease { ease_in_out(t) } else { t };
        let icon = match color {
            Some(c) => icon.text_color(c),
            None => icon,
        };
        div().child(icon.rotate(percentage(t))).into_any_element()
    }

    /** A settings replacement for a built-in part: `spec` is a component name or a node, drawn with `.props`
    over this frame's context. None when `spec` is unset, null or false (the caller draws its own look). */
    pub(crate) fn render_part(&mut self, spec: Option<&Value>, props: Value, path: &str, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let node = match spec? {
            Value::String(name) if !name.is_empty() => serde_json::json!({ "type": name }),
            n @ Value::Object(_) => n.clone(),
            _ => return None,
        };
        let ctx = with(&frame_ctx(), vec![("props".into(), to_val(&props))]);
        let mut els = self.render(&node, &ctx, path, window, cx);
        Some(if els.len() == 1 { els.pop().unwrap_or_else(|| div().into_any_element()) } else { div().flex().flex_col().children(els).into_any_element() })
    }

    pub(crate) fn render_view(&mut self, name: &str, ctx: &Val, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let main_ctx;
        let ctx = if name == "main" {
            super::drive::new_frame();
            super::drive::load_fonts(&self.settings.theme, window);
            let focused = super::drive::ui(|u| u.focused_last.clone()).map(Value::String).unwrap_or(Value::Null);
            main_ctx = with(ctx, vec![("focused_node".into(), to_val(&focused))]);
            FRAME_CTX.with(|c| *c.borrow_mut() = Some(main_ctx.clone()));
            &main_ctx
        } else {
            ctx
        };
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
            let mut out = vec![];
            for (i, item) in items.into_iter().enumerate() {
                let c = with(ctx, vec![(name.clone(), item), ("index".into(), to_val(&Value::from(i)))]);
                let key = obj.get("key").and_then(Value::as_str).map(|k| value_text(&to_json(&self.exprs.first(k, &c)))).unwrap_or(i.to_string());
                out.extend(self.render_node(obj, &c, &format!("{path}[{key}]"), window, cx));
            }
            return out;
        }
        self.render_node(obj, ctx, path, window, cx)
    }

    /** A component as one use of it sets it up: the use's `style` merged into its root's, and the use's own
    `id`, `on`, `menu` and the rest in place of its root's. Built once per use until the settings change. */
    fn component_use(&mut self, kind: &str, obj: &Map<String, Value>) -> Option<std::rc::Rc<Value>> {
        const OWN: [&str; 14] = ["id", "animate", "motion", "transition", "tooltip", "on", "menu", "menu_on", "focusable", "drag", "drop_key", "drop_zones", "layer", "region"];
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for k in std::iter::once("style").chain(OWN) {
            k.hash(&mut h);
            hash_json(obj.get(k), &mut h);
        }
        let key = (kind.to_string(), h.finish());
        if let Some((rev, comp)) = self.component_uses.get(&key) {
            if *rev == self.settings_rev {
                return Some(comp.clone());
            }
        }
        let mut comp = self.settings.components.get(kind)?.clone();
        if let Value::Object(base) = &mut comp {
            if let Some(Value::Object(extra)) = obj.get("style") {
                if let Value::Object(st) = base.entry("style").or_insert(Value::Object(Map::new())) {
                    for (k, v) in extra {
                        st.insert(k.clone(), v.clone());
                    }
                }
            }
            for k in OWN {
                if let Some(v) = obj.get(k) {
                    base.insert(k.into(), v.clone());
                }
            }
        }
        let comp = std::rc::Rc::new(comp);
        if self.component_uses.values().next().is_some_and(|(rev, _)| *rev != self.settings_rev) {
            self.component_uses.clear();
        }
        self.component_uses.insert(key, (self.settings_rev, comp.clone()));
        Some(comp)
    }

    /** Draws one node (its `for` already applied). */
    fn render_node(&mut self, obj: &Map<String, Value>, ctx: &Val, path: &str, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        if let Some(code) = obj.get("if").and_then(Value::as_str) {
            if !self.exprs.truthy(code, ctx) {
                return vec![];
            }
        }
        let kind = obj.get("type").and_then(Value::as_str).unwrap_or("box").to_string();
        if kind == "select" && !self.settings.components.contains_key("select") {
            let node = self.select_node(obj, ctx, path);
            return self.render(&node, ctx, path, window, cx);
        }
        if let Some(comp) = self.component_use(&kind, obj) {
            let props = self.exprs.resolve_val(obj.get("props").unwrap_or(&Value::Null), ctx);
            let c = with(ctx, vec![("props".into(), props)]);
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
        if obj.get("region").and_then(Value::as_bool) == Some(true) {
            return vec![self.place_region(obj, ctx, path, &id, &st, cx)];
        }
        // `hover_ms` (and ui.hover) apply hover styles from tracked state, so they can ease.
        let hover_ms = st.get("hover_ms").and_then(Value::as_u64);
        let (hovered, forced) = super::drive::ui(|u| (u.hovered.contains(&id), u.forced_hover.as_deref() == Some(id.as_str())));
        let mut eased: Vec<(String, u64)> = vec![];
        let tracks_hover = hover_ms.is_some() && st.get("hover").is_some_and(Value::is_object);
        if let Some(Value::Object(h)) = st.get("hover").cloned().filter(|_| tracks_hover || forced) {
            st.remove("hover");
            if hovered || forced {
                st.extend(h.clone());
            }
            if let Some(ms) = hover_ms {
                eased.extend(h.keys().map(|k| (k.clone(), ms)));
            }
        }
        let on = obj.get("on").and_then(Value::as_object);
        let focusable = obj.get("focusable").is_some_and(|f| match f {
            Value::Bool(b) => *b,
            Value::String(code) => self.exprs.truthy(code, ctx),
            _ => false,
        }) || on.is_some_and(|o| o.contains_key("key"));
        let focus = focusable.then(|| super::drive::focus_handle(&id, cx));
        let focused = focus.as_ref().is_some_and(|h| h.is_focused(window));
        if focused {
            super::drive::ui(|u| u.focused = Some(id.clone()));
            if let Some(Value::Object(f)) = st.get("focus").cloned() {
                st.extend(f);
            }
        }
        if focusable {
            let was = super::drive::ui(|u| u.focused_last.as_deref() == Some(id.as_str()));
            let event = match (focused, was) {
                (true, false) => on.and_then(|o| o.get("focus")),
                (false, true) => on.and_then(|o| o.get("blur")),
                _ => None,
            };
            if let Some(a) = event {
                let a = self.exprs.resolve_action(a, ctx);
                self.pending_actions.push(a);
                cx.notify();
            }
        }
        self.apply_transitions(obj, &mut st, &id, &eased, window);
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
            "text" if props.get("selectable").and_then(Value::as_bool) == Some(true) => {
                let text = markdown_escape(&value_text(props.get("text").unwrap_or(&Value::Null)));
                vec![TextView::markdown(SharedString::from(format!("{id}:text")), text).selectable(true).into_any_element()]
            }
            "text" => vec![value_text(props.get("text").unwrap_or(&Value::Null)).into_any_element()],
            "icon" => {
                let name = props.get("name").and_then(Value::as_str).unwrap_or("circle");
                let size = props.get("size").and_then(Value::as_f64).unwrap_or(14.) as f32;
                let mut icon = Icon::empty().path(format!("icons/{name}.svg")).size(px(size));
                // Without props.color the icon takes the node's text color, hover included.
                if let Some(c) = props.get("color").and_then(|c| style::color(c, &theme)) {
                    icon = icon.text_color(c);
                }
                if props.get("spin").and_then(Value::as_bool) == Some(true) {
                    let period = self.settings.num("motion.spin_period_ms", 900.) as f32;
                    vec![self.turning(icon, None, period, false, cx)]
                } else {
                    vec![icon.into_any_element()]
                }
            }
            "image" => {
                // props: src (a file or URL), fit (cover | contain | fill | scale_down | none). Size it with style.
                let src = props.get("src").map(value_text).unwrap_or_default();
                let fit = props.get("fit").and_then(Value::as_str).unwrap_or("cover").to_string();
                if src.is_empty() { vec![] } else { vec![crate::workspace::media_element(&src, &fit)] }
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
            "markdown" => {
                let text = value_text(props.get("text").unwrap_or(&Value::Null));
                let mid = props.get("id").map(value_text).unwrap_or(id.clone());
                let mut md = self.settings.get("markdown").or_else(|| self.settings.chat.get("markdown")).and_then(Value::as_object).cloned();
                if let Some(Value::Object(over)) = props.get("markdown") {
                    md.get_or_insert_with(Map::new).extend(over.clone());
                }
                let selectable = props.get("selectable").and_then(Value::as_bool).unwrap_or(self.settings.chat.get("selectable").and_then(Value::as_bool) != Some(false));
                vec![self.markdown_with(&mid, &text, md.as_ref(), selectable, &theme).into_any_element()]
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
                let chat = chat.and_then(|c| self.chat(&c).cloned()).filter(|c| c.state == "waiting");
                let spec = self.settings.chat.get("render").and_then(|r| r.get("waiting")).cloned();
                chat.map(|c| {
                    let custom = spec.as_ref().filter(|s| !s.is_null()).and_then(|spec| {
                        let props = self.waiting_props(&c);
                        self.render_part(Some(spec), props, &format!("{path}:waiting"), window, cx)
                    });
                    custom.unwrap_or_else(|| self.waiting_card(&c, cx))
                })
                .into_iter()
                .collect()
            }
            "screen" => {
                let chat = props.get("chat").and_then(Value::as_str).map(str::to_string).or_else(|| self.current_chat().map(|c| c.id.clone()));
                let term = chat.and_then(|c| self.chat_screen(&c, window, cx));
                if let Some(t) = &term {
                    t.update(cx, |t, cx| t.set_overrides(&props, &theme, cx));
                }
                term.map(|t| div().size_full().child(t).into_any_element()).into_iter().collect()
            }
            "terminal" => {
                let path = props.get("path").and_then(Value::as_str).map(str::to_string).or_else(|| self.selected.clone());
                let queued = crate::terminal::take_actions();
                if !queued.is_empty() {
                    self.pending_actions.extend(queued);
                    cx.notify();
                }
                let term = path.and_then(|p| self.active_terminal(&p, cx));
                if let Some(t) = &term {
                    t.update(cx, |t, cx| t.set_overrides(&props, &theme, cx));
                }
                term.map(|t| div().size_full().child(t).into_any_element()).into_iter().collect()
            }
            "diff" => vec![self.diff_view(&props, window, cx)],
            "attachments" => {
                let chat = props.get("chat").and_then(Value::as_str).map(str::to_string).or_else(|| self.current_chat().map(|c| c.id.clone()));
                self.attachments_view(chat, &props, cx).into_iter().collect()
            }
            k if super::widgets::NODE_TYPES.iter().any(|(n, _)| *n == k) => self.render_widget(k, &id, &props, obj, ctx, &st, window, cx),
            "resize_handle" => vec![],
            "scroll" => vec![],
            _ => vec![],
        };
        let virtual_list = kind == "scroll" && props.get("virtual").and_then(Value::as_bool) == Some(true);
        // "motion": false holds still the whole subtree, so a node that appears (a popover button's copy) doesn't
        // fade or slide its contents in.
        let still = obj.get("motion").and_then(Value::as_bool) == Some(false);
        self.motion_off += still as usize;
        // Regions among the children need to know which way they're laid out (see place_region).
        let row = kind == "row" || st.get("direction").and_then(Value::as_str) == Some("row");
        let saved_row = std::mem::replace(&mut self.in_row, row);
        let kids = if virtual_list { vec![self.virtual_list(obj, &props, ctx, path, &id, cx)] } else if super::widgets::OWNS_CHILDREN.contains(&kind.as_str()) { vec![] } else { children(self, window, cx) };
        self.motion_off -= still as usize;
        self.in_row = saved_row;

        let mut el = el.id(SharedString::from(id.clone()));
        let mut scrollbar: Option<AnyElement> = None;
        if kind == "input" {
            // Clicks in a field are for the field, not a drag or click handler around it.
            el = el.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        }
        if virtual_list {
            el = el.overflow_hidden();
        } else if kind == "scroll" {
            let axis = props.get("axis").and_then(Value::as_str).unwrap_or("y");
            el = match axis {
                "x" => el.overflow_x_scroll(),
                "both" => el.overflow_scroll(),
                _ => el.overflow_y_scroll(),
            };
            let name = props.get("handle").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| id.clone());
            let h = self.scroll_handles.entry(name).or_default().clone();
            el = el.track_scroll(&h);
            if let Some(mode) = scrollbar_mode(props.get("scrollbar")) {
                if !st.contains_key("position") {
                    el = el.relative();
                }
                let bar = match axis {
                    "x" => gpui_kit::component::scroll::Scrollbar::horizontal(&h),
                    "both" => gpui_kit::component::scroll::Scrollbar::new(&h),
                    _ => gpui_kit::component::scroll::Scrollbar::vertical(&h),
                };
                scrollbar = Some(bar.mode(mode).into_any_element());
            }
        }
        if kind == "resize_handle" {
            let key = props.get("state").and_then(Value::as_str).unwrap_or("").to_string();
            let vertical = props.get("axis").and_then(Value::as_str) == Some("y");
            let invert = props.get("invert").and_then(Value::as_bool) == Some(true);
            let start_default = props.get("default").and_then(Value::as_f64).unwrap_or(300.);
            let (min, max) = (
                props.get("min").and_then(Value::as_f64).unwrap_or(80.) as f32,
                props.get("max").and_then(Value::as_f64).unwrap_or(2000.) as f32,
            );
            el = el
                .cursor(if vertical { CursorStyle::ResizeUpDown } else { CursorStyle::ResizeLeftRight })
                .on_mouse_down(MouseButton::Left, cx.listener(move |ws, e: &MouseDownEvent, _, cx| {
                    let start_size = ws.state.get(&key).and_then(Value::as_f64).unwrap_or(start_default) as f32;
                    let start = if vertical { f32::from(e.position.y) } else { f32::from(e.position.x) };
                    ws.drag = Some(crate::workspace::Drag { key: key.clone(), vertical, invert, start, start_size, min, max });
                    cx.stop_propagation();
                }));
        }
        if tracks_hover || on.is_some_and(|o| o.contains_key("hover_start") || o.contains_key("hover_end")) {
            let get = |k: &str| on.and_then(|o| o.get(k)).map(|a| self.exprs.resolve_action(a, ctx));
            let (start, end) = (get("hover_start"), get("hover_end"));
            super::drive::ui(|u| u.hovers.push((id.clone(), start.clone(), end.clone())));
            let me = id.clone();
            el = el.on_hover(cx.listener(move |ws, over: &bool, window, cx| {
                let changed = super::drive::ui(|u| if *over { u.hovered.insert(me.clone()) } else { u.hovered.remove(&me) });
                if !changed {
                    return;
                }
                ws.refresh_regions = true;
                match (*over, &start, &end) {
                    (true, Some(a), _) | (false, _, Some(a)) => ws.run_action(a, window, cx),
                    _ => cx.notify(),
                }
            }));
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
            // Which node a press landed on: the innermost one with a handler records it first (presses bubble
            // outward), so a header can tell a press of its own from one on a button inside it.
            let pid = id.clone();
            el = el.on_mouse_down(MouseButton::Left, cx.listener(move |ws, e: &MouseDownEvent, _, cx| {
                gpui_kit::base::GlobalState::suppress_text_selection(cx);
                ws.note_press(&pid, e.position, e.click_count);
            }));
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
        if let Some(Value::Object(a)) = st.get("active") {
            let (a, theme) = (a.clone(), theme.clone());
            el = el.active(move |mut s| {
                style::refine(&mut s, &a, &theme);
                s
            });
        }
        let mut keyboard_click: Option<Value> = None;
        let menu_on_click = obj.get("menu_on").and_then(Value::as_str) == Some("click") && obj.get("menu").is_some_and(Value::is_array);
        if let Some(Value::Object(on)) = obj.get("on") {
            let get = |k: &str| on.get(k).map(|a| self.exprs.resolve_action(a, ctx));
            let (click, double, down, drop) = (get("click"), get("double_click"), get("mouse_down"), get("drop"));
            let label = obj
                .get("tooltip")
                .map(|t| value_text(&self.exprs.resolve(t, ctx)))
                .filter(|t| !t.is_empty())
                .or_else(|| first_text(obj).map(|t| value_text(&self.exprs.resolve(&t, ctx))).filter(|t| !t.is_empty()))
                .unwrap_or_default();
            if let Some(c) = click.clone() {
                self.clickables.push((id.clone(), label.clone(), c));
            }
            // Double-clicks and presses are clickable by agents too: `ui.click {id, double: true}` / `{id, press: true}`.
            if let Some(d) = double.clone() {
                self.clickables.push((format!("{id}:double"), label.clone(), d));
            }
            if let Some(d) = down.clone() {
                self.clickables.push((format!("{id}:press"), label.clone(), d));
            }
            if let Some(r) = get("right_click") {
                self.clickables.push((format!("{id}:right_click"), label.clone(), r.clone()));
                el = el.on_mouse_down(MouseButton::Right, cx.listener(move |ws, _: &MouseDownEvent, window, cx| ws.run_action(&r, window, cx)));
            }
            if let Some(u) = get("mouse_up") {
                el = el.on_mouse_up(MouseButton::Left, cx.listener(move |ws, _: &MouseUpEvent, window, cx| ws.run_action(&u, window, cx)));
            }
            if let Some(o) = get("click_outside") {
                el = el.on_mouse_down_out(cx.listener(move |ws, _: &MouseDownEvent, window, cx| ws.run_action(&o, window, cx)));
            }
            if let Some(w) = get("scroll") {
                el = el.on_scroll_wheel(cx.listener(move |ws, e: &ScrollWheelEvent, window, cx| {
                    let d = e.delta.pixel_delta(window.line_height());
                    let a = super::drive::with_args(&w, &[("dx", Value::from(f32::from(d.x) as f64)), ("dy", Value::from(f32::from(d.y) as f64))]);
                    ws.run_action(&a, window, cx);
                }));
            }
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
                super::drive::ui(|u| u.drops.push(super::drive::DropTarget { key: drop_key.clone(), zones: zones.clone(), specs: specs.clone(), ctx: ctx.clone() }));
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
                    let edge = ws.settings.num("drag.edge", 0.5) as f32;
                    let band = ws.settings.num("drag.inside_band", 0.25) as f32;
                    let rel = (f32::from(e.event.position.y - e.bounds.origin.y) / f32::from(e.bounds.size.height).max(1.)).clamp(0., 1.);
                    let rel_x = (f32::from(e.event.position.x - e.bounds.origin.x) / f32::from(e.bounds.size.width).max(1.)).clamp(0., 1.);
                    let zone = match mode.as_str() {
                        "edges" => if rel < edge { "before" } else { "after" },
                        "edges_x" => if rel_x < edge { "before" } else { "after" },
                        "edges+inside" => if rel < band { "before" } else if rel > 1. - band { "after" } else { "inside" },
                        "before" => "before",
                        "after" => "after",
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
                let known = matches!(
                    event.as_str(),
                    "click" | "double_click" | "mouse_down" | "mouse_up" | "right_click" | "hover_start" | "hover_end" | "scroll" | "key" | "focus" | "blur" | "click_outside" | "drop" | "drop_item"
                );
                if !known && !(event == "submit" && kind == "input") && !super::widgets::EVENTS.contains(&event.as_str()) {
                    self.exprs.note(format!("unknown event `{event}` at {path}"));
                }
            }
            let (has_double, has_down) = (double.is_some(), down.is_some());
            if let Some(down) = down {
                let me = id.clone();
                el = el.on_mouse_down(MouseButton::Left, cx.listener(move |ws, e: &MouseDownEvent, window, cx| {
                    // Only presses on this node itself: not on a button inside it, and a double press only
                    // when its first half landed here too.
                    if !ws.press_is(&me, e.position) {
                        ws.log_input(&me, e.click_count, "not its own");
                        return;
                    }
                    match (e.click_count, &double) {
                        (2, Some(d)) if ws.first_press_is(&me) => ws.run_action(d, window, cx),
                        (2, Some(_)) => ws.log_input(&me, 2, "first press elsewhere"),
                        (1, _) => ws.run_action(&down, window, cx),
                        _ => {}
                    }
                }));
            } else if let Some(d) = double.clone() {
                let me = id.clone();
                el = el.on_click(cx.listener(move |ws, e: &ClickEvent, window, cx| {
                    if e.click_count() == 2 && ws.first_press_is(&me) {
                        ws.run_action(&d, window, cx);
                    }
                }));
            }
            if let Some(c) = click.clone().filter(|_| focusable) {
                keyboard_click = Some(c);
            }
            if let Some(c) = click {
                // A button takes every press as a click (a quick second one too); only a node that also
                // has a double_click leaves the second press of a pair to it.
                el = el.on_click(cx.listener(move |ws, e: &ClickEvent, window, cx| {
                    if !has_double || e.click_count() < 2 {
                        ws.run_action(&c, window, cx);
                    }
                }));
                if !has_down && !has_double && !obj.contains_key("drag") {
                    // Its presses are its own: the header around it mustn't read them as dragging the window
                    // or (pressed twice) zooming it.
                    let pid = id.clone();
                    el = el.on_mouse_down(MouseButton::Left, cx.listener(move |ws, e: &MouseDownEvent, _, cx| {
                        gpui_kit::base::GlobalState::suppress_text_selection(cx);
                        ws.note_press(&pid, e.position, e.click_count);
                        cx.stop_propagation();
                    }));
                }
            }
        }
        if let Some(h) = &focus {
            let keys = key_map(on.and_then(|o| o.get("key")), |a| self.exprs.resolve_action(a, ctx));
            super::drive::ui(|u| u.keys.push((id.clone(), keys.clone(), keyboard_click.clone())));
            let clicks = keyboard_click.is_some();
            el = el.track_focus(h).on_key_down(cx.listener(move |ws, e: &KeyDownEvent, window, cx| {
                let pressed = e.keystroke.unparse();
                if let Some((_, a)) = keys.iter().find(|(k, _)| *k == pressed) {
                    ws.run_action(a, window, cx);
                    cx.stop_propagation();
                } else if let (Some(c), "enter" | "space") = (&keyboard_click, pressed.as_str()) {
                    ws.run_action(c, window, cx);
                    cx.stop_propagation();
                } else if pressed == "tab" || pressed == "shift-tab" {
                    if pressed == "tab" { window.focus_next(cx) } else { window.focus_prev(cx) }
                    cx.stop_propagation();
                }
            }));
            if !st.contains_key("cursor") && clicks {
                el = el.cursor_pointer();
            }
        }
        if let Some(tip) = obj.get("tooltip") {
            let tip = value_text(&self.exprs.resolve(tip, ctx));
            if !tip.is_empty() {
                super::drive::ui(|u| u.tooltips.push((id.clone(), tip.clone())));
                let look = theme.get("tooltip").and_then(Value::as_object).cloned().unwrap_or_default();
                if let Some(ms) = theme.get("tooltip_delay_ms").and_then(Value::as_u64) {
                    el = el.tooltip_show_delay(Duration::from_millis(ms));
                }
                let theme = theme.clone();
                el = el.tooltip(move |w, cx| {
                    let mut t = gpui_kit::component::tooltip::Tooltip::new(tip.clone());
                    style::refine(t.style(), &look, &theme);
                    t.build(w, cx)
                });
            }
        }
        if let (true, Some(Value::Array(menu))) = (menu_on_click, obj.get("menu")) {
            let (spec, menu_ctx, mid) = (menu.clone(), ctx.clone(), id.clone());
            el = el.cursor_pointer().on_click(cx.listener(move |ws, e: &ClickEvent, window, cx| {
                let at = ws.motion.borrow().bounds.get(&mid).map(|b| point(px(b[0]), px(b[1] + b[3]))).unwrap_or(e.position());
                super::drive::toggle_menu(ws, &mid, &spec, &menu_ctx, at, window, cx);
            }));
        }
        let open_menu = super::drive::ui(|u| u.menu.as_ref().filter(|m| m.id == id).map(|m| (m.at, m.menu.clone())));
        let mut el = el.children(content).children(kids).children(scrollbar);
        if let Some((at, menu)) = open_menu {
            el = el.child(deferred(anchored().position(at).snap_to_window_with_margin(px(8.)).child(menu)).with_priority(3));
        }
        let layer = obj.get("layer").map(|l| value_text(&self.exprs.resolve(l, ctx))).unwrap_or_default();
        let z = layer.parse::<usize>().ok().or_else(|| st.get("z_index").and_then(Value::as_u64).map(|z| z as usize));
        if layer == "top" || obj.get("occlude").and_then(Value::as_bool) == Some(true) {
            el = el.occlude();
        }
        let el: AnyElement = if let (false, Some(Value::Array(menu))) = (menu_on_click, obj.get("menu")) {
            // Entries are worked out when the menu opens, not on every frame.
            let (spec, menu_ctx) = (menu.clone(), ctx.clone());
            let tip = obj.get("tooltip").map(|t| value_text(&self.exprs.resolve(t, ctx))).unwrap_or_default();
            self.menus.push((id.clone(), tip, std::rc::Rc::new(menu.clone()), ctx.clone()));
            let view = cx.entity();
            el.context_menu(move |menu, window, cx| {
                let entries = view.read(cx).menu_entries(&spec, &menu_ctx);
                let theme = view.read(cx).settings.theme.clone();
                build_menu(menu, entries, &view, &theme, window, cx)
            })
            .into_any_element()
        } else {
            if let (true, Some(Value::Array(menu))) = (menu_on_click, obj.get("menu")) {
                let tip = obj.get("tooltip").map(|t| value_text(&self.exprs.resolve(t, ctx))).unwrap_or_default();
                self.menus.push((id.clone(), tip, std::rc::Rc::new(menu.clone()), ctx.clone()));
            }
            el.into_any_element()
        };
        // A node lifted "above" (a popover's button while it's open) stays where it is: gliding it would read
        // its deferred position as a move.
        // Only what can move on its own glides (a node with an id, an item of a `for`); the rest moves with it,
        // and each glide costs a little every frame.
        let own = obj.contains_key("id") || path.ends_with(']');
        let moves = own && self.motion.borrow().enabled && !still && self.motion_off == 0 && layer != "above" && z.is_none();
        let el = if moves {
            crate::ui::motion::Motion::new(id, el, self.motion.clone()).into_any_element()
        } else if obj.contains_key("id") && !id.contains('/') {
            crate::ui::motion::Track::new(id, el, self.motion.clone()).into_any_element()
        } else {
            el
        };
        // "layer": "top" paints the node above everything else (popovers, dialogs) and keeps clicks off what's under it;
        // "above" goes over that too (the button that opened a popover stays a button while it's open).
        match (layer.as_str(), z) {
            ("top", _) => return vec![deferred(super::anim::in_layer(1, el)).with_priority(1).into_any_element()],
            ("above", _) => return vec![deferred(super::anim::in_layer(2, el)).with_priority(2).into_any_element()],
            (_, Some(n)) => return vec![deferred(super::anim::in_layer(n, el)).with_priority(n).into_any_element()],
            _ => {}
        }
        vec![el]
    }

    /** A `scroll` with `props.virtual`: its children (each `for` item counts as one) become rows of a list
    that builds and lays out only the rows on screen. */
    fn virtual_list(&mut self, obj: &Map<String, Value>, props: &Map<String, Value>, ctx: &Val, path: &str, id: &str, cx: &mut Context<Self>) -> AnyElement {
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
                // Each row draws as its own region (`props.region_rows`: false draws them all every frame).
                if props.get("region_rows").and_then(Value::as_bool) != Some(false) {
                    o.entry("region").or_insert(Value::Bool(true));
                }
            }
            for (j, item) in items.into_iter().enumerate() {
                let c = with(ctx, vec![(name.clone(), item), ("index".into(), to_val(&Value::from(j)))]);
                let key = kid.get("key").and_then(Value::as_str).map(|k| value_text(&to_json(&self.exprs.first(k, &c)))).unwrap_or(j.to_string());
                rows.push((one.clone(), c, format!("{at}[{key}]")));
            }
        }
        let keys: Vec<String> = rows.iter().map(|r| r.2.clone()).collect();
        let spec = (
            props.get("align").and_then(Value::as_str) == Some("bottom"),
            props.get("overdraw").and_then(Value::as_f64).unwrap_or(400.).max(0.) as u32,
            props.get("follow").and_then(Value::as_bool) == Some(true),
        );
        if super::drive::ui(|u| u.lists.insert(id.to_string(), spec)).is_some_and(|old| old != spec) {
            self.virtual_lists.remove(id);
        }
        let entry = self.virtual_lists.entry(id.to_string()).or_insert_with(|| {
            let state = ListState::new(0, if spec.0 { ListAlignment::Bottom } else { ListAlignment::Top }, px(spec.1 as f32));
            if spec.2 {
                state.set_follow_mode(FollowMode::Tail);
            }
            (state, vec![], Default::default())
        });
        let state = entry.0.clone();
        if entry.1 != keys {
            let (old, new) = (&entry.1, &keys);
            let head = old.iter().zip(new).take_while(|(a, b)| a == b).count();
            let tail = old[head..].iter().rev().zip(new[head..].iter().rev()).take_while(|(a, b)| a == b).count();
            state.splice(head..old.len() - tail, new.len() - tail - head);
            entry.1 = keys;
        }
        // Rows off screen keep their last height until they scroll into view; content may have changed,
        // which only a new context (or a frame drawing every region) can bring.
        let generation = (self.ctx_generation.get(), self.settings_rev);
        if self.refresh_now || self.list_generations.insert(id.to_string(), generation) != Some(generation) {
            state.remeasure_items(0..rows.len());
        }
        entry.2 = std::rc::Rc::new(rows);
        let list_id = id.to_string();
        let owner = super::region::owner();
        let bar = scrollbar_mode(props.get("scrollbar")).map(|m| gpui_kit::component::scroll::Scrollbar::vertical(&state).mode(m));
        let rows = list(state, cx.processor(move |ws: &mut Workspace, ix: usize, window, cx| {
            let Some(row) = ws.virtual_lists.get(&list_id).and_then(|l| l.2.get(ix).cloned()) else { return div().into_any_element() };
            ws.as_owner(&owner, window, |ws, window| div().w_full().flex().flex_col().children(ws.render(&row.0, &row.1, &row.2, window, cx)).into_any_element())
        }))
        .size_full();
        match bar {
            Some(bar) => div().relative().size_full().child(rows).child(bar).into_any_element(),
            None => rows.into_any_element(),
        }
    }

    /** What a `chat.render.waiting` component gets as `.props`: the question, what a permission prompt's
    tool would run (`detail`), its options (each with the action that answers it) and the cancel action. */
    fn waiting_props(&self, c: &agent_box_protocol::Chat) -> Value {
        let w = c.waiting_on.clone().unwrap_or_default();
        let message = crate::workspace::waiting_message(&w).unwrap_or_else(|| self.tr("waiting", "The agent is waiting for you", &[]));
        let detail = crate::workspace::waiting_detail(&w, self.settings.get("chat.waiting_detail_lines").and_then(Value::as_u64).unwrap_or(12) as usize);
        let (options, mut questions) = crate::workspace::waiting_options(&w, &self.settings);
        questions.retain(|q| *q != message);
        let options: Vec<Value> = options
            .iter()
            .enumerate()
            .map(|(i, label)| serde_json::json!({"label": label, "index": i, "do": {"do": "chat.answer", "chat": c.id, "option": i}}))
            .collect();
        serde_json::json!({
            "chat": c.id, "message": message, "detail": detail, "questions": questions, "options": options, "tool": w["tool_name"],
            "cancel": {"label": self.tr("waiting_cancel", "Cancel", &[]), "do": {"do": "chat.answer", "chat": c.id, "option": "cancel"}},
        })
    }

    /** A `select` as the nodes it's made of: a button (text + chevron) with a left-click menu of its options. */
    fn select_node(&mut self, obj: &Map<String, Value>, ctx: &Val, path: &str) -> Value {
        let props = match self.exprs.resolve(obj.get("props").unwrap_or(&Value::Null), ctx) {
            Value::Object(p) => p,
            _ => Map::new(),
        };
        let key = props.get("state").and_then(Value::as_str).map(str::to_string);
        let current = key.as_ref().and_then(|k| self.state.get(k).cloned()).or_else(|| props.get("value").cloned()).unwrap_or(Value::Null);
        let on_change = obj.get("on").and_then(|o| o.get("change"));
        let options = select_options(props.get("options"));
        let id = match obj.get("id").and_then(Value::as_str) {
            Some(own) => format!("{own}{}", loop_keys(path)),
            None => path.to_string(),
        };
        let mut items = vec![];
        for (value, label) in &options {
            let action = self.change_action(key.as_deref(), value.clone(), on_change, ctx);
            self.clickables.push((format!("{id}:{}", value_text(value)), label.clone(), action.clone()));
            items.push(serde_json::json!({"label": label, "checked": *value == current, "do": action}));
        }
        let shown = options.iter().find(|(v, _)| *v == current).map(|(_, l)| l.clone());
        let placeholder = props.get("placeholder").map(value_text).unwrap_or_else(|| self.tr("select_placeholder", "Choose…", &[]));
        let mut style = self.settings.theme.get("select").and_then(Value::as_object).cloned().unwrap_or_else(|| {
            serde_json::json!({"direction": "row", "align": "center", "gap": 6, "padding": [4, 10], "radius": 6, "border": 1, "border_color": "@line", "background": "@pill", "font_size": 12.5, "hover": {"background": "@pill_hover"}})
                .as_object()
                .cloned()
                .unwrap_or_default()
        });
        if let Some(Value::Object(own)) = obj.get("style") {
            style.extend(own.clone());
        }
        let mut node = Map::new();
        for k in ["id", "tooltip", "layer", "motion", "animate", "transition", "focusable"] {
            if let Some(v) = obj.get(k) {
                node.insert(k.into(), v.clone());
            }
        }
        if let Some(Value::Object(on)) = obj.get("on") {
            let rest: Map<String, Value> = on.iter().filter(|(k, _)| k.as_str() != "change").map(|(k, v)| (k.clone(), v.clone())).collect();
            node.insert("on".into(), Value::Object(rest));
        }
        node.insert("type".into(), Value::from("row"));
        node.insert("style".into(), Value::Object(style));
        node.insert("menu_on".into(), Value::from("click"));
        node.insert("menu".into(), Value::Array(items));
        let icon = props.get("icon").and_then(Value::as_str).unwrap_or("chevron-down");
        node.insert(
            "children".into(),
            serde_json::json!([
                {"type": "text", "props": {"text": shown.unwrap_or(placeholder)}},
                {"type": "icon", "props": {"name": icon, "size": 12}}
            ]),
        );
        Value::Object(node)
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
        let running = self.entered.get(id).filter(|(_, seen, _)| self.motion.borrow().recent(*seen)).map(|(start, _, spec)| (*start, spec.clone()));
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
        self.anim_owner.insert(id.to_string(), super::region::owner());
        let ms = spec.get("duration").and_then(Value::as_u64).unwrap_or(200).max(1) as f32;
        let t = (start.elapsed().as_secs_f32() * 1000. / ms).min(1.);
        if t >= 1. {
            return;
        }
        window.request_animation_frame();
        let e = crate::ui::motion::Easing::parse_or(spec.get("easing").and_then(Value::as_str), crate::ui::motion::Easing::EaseOutQuart).apply(t);
        let from = spec.get("from").cloned().unwrap_or(Value::Null);
        let f = |k: &str| from.get(k).and_then(Value::as_f64).map(|x| x as f32);
        if let Some(o) = f("opacity") {
            let base = st.get("opacity").and_then(Value::as_f64).unwrap_or(1.) as f32;
            st.insert("opacity".into(), Value::from((o + (base - o) * e) as f64));
        }
        // A node held by its far edge alone (`bottom: 0`) shifts by that edge: setting `top` would move it there.
        for (k, side, far) in [("x", "left", "right"), ("y", "top", "bottom")] {
            if let Some(d) = f(k) {
                let (side, d) = match (st.get(side).and_then(Value::as_f64), st.get(far).and_then(Value::as_f64)) {
                    (None, Some(_)) => (far, -d),
                    _ => (side, d),
                };
                let base = st.get(side).and_then(Value::as_f64).unwrap_or(0.) as f32;
                st.insert(side.into(), Value::from((base + d * (1. - e)) as f64));
            }
        }
    }

    /** `transition` (and `motion.style` for common keys): numbers and colors ease from their old value
    to the new one. Dragging and first appearances snap. */
    fn apply_transitions(&mut self, obj: &Map<String, Value>, st: &mut Map<String, Value>, id: &str, extra: &[(String, u64)], window: &mut Window) {
        let enabled = self.motion.borrow().enabled && obj.get("motion").and_then(Value::as_bool) != Some(false);
        let mut keys: Vec<(String, u64)> = match obj.get("transition") {
            Some(Value::Object(tr)) => tr.iter().map(|(k, ms)| (k.clone(), ms.as_u64().unwrap_or(200))).collect(),
            _ => vec![],
        };
        for (k, ms) in extra {
            if !keys.iter().any(|(x, _)| x == k) {
                keys.push((k.clone(), *ms));
            }
        }
        if let Some(ms) = self.settings.motion.get("style").and_then(|s| s.get("duration")).and_then(Value::as_u64) {
            for k in AUTO_KEYS {
                if st.contains_key(*k) && !keys.iter().any(|(x, _)| x == k) {
                    keys.push((k.to_string(), ms));
                }
            }
        }
        let generation = self.render_generation;
        let snap = !enabled || self.drag.is_some();
        let style_easing = crate::ui::motion::Easing::parse_or(self.settings.motion.get("style").and_then(|s| s.get("easing")).and_then(Value::as_str), crate::ui::motion::Easing::EaseOut);
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
                Some(t) if self.motion.borrow().recent(t.seen) && !snap => {
                    t.seen = generation;
                    if t.to.len() == target.len() && t.to.iter().zip(&target).any(|(a, b)| (a - b).abs() > 0.001) {
                        let (now, _) = t.value();
                        *t = Transition { from: now, to: target, start: Instant::now(), duration: Duration::from_millis(ms), seen: generation, easing: style_easing };
                    }
                    let (v, running) = t.value();
                    if running {
                        window.request_animation_frame();
                    }
                    v
                }
                _ => {
                    self.anim_owner.insert(key.clone(), super::region::owner());
                    self.transitions.insert(key, Transition { from: target.clone(), to: target.clone(), start: Instant::now(), duration: Duration::ZERO, seen: generation, easing: style_easing });
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
#[derive(Clone, PartialEq, Debug)]
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
    theme: std::rc::Rc<Map<String, Value>>,
}

impl Render for DragPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let note = self.note.borrow().clone();
        let note_style = self.theme.get("drag_preview_note").and_then(Value::as_object).cloned().unwrap_or_else(|| {
            serde_json::json!({"font_size": 11.5, "color": "@accent"}).as_object().cloned().unwrap_or_default()
        });
        style::apply(div(), &self.style, &self.theme)
            .child(self.label.clone())
            .when(!note.is_empty(), |d| d.child(style::apply(div(), &note_style, &self.theme).child(note)))
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
pub(crate) fn loop_keys(path: &str) -> String {
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

pub(crate) fn build_menu(
    mut m: gpui_kit::component::menu::PopupMenu,
    entries: Vec<MenuEntry>,
    view: &Entity<Workspace>,
    theme: &Map<String, Value>,
    window: &mut Window,
    cx: &mut Context<gpui_kit::component::menu::PopupMenu>,
) -> gpui_kit::component::menu::PopupMenu {
    let danger_color = style::color(&Value::String("@danger".into()), theme);
    let faint = style::color(&Value::String("@text_faint".into()), theme);
    // `theme.menu`: min_width, max_height (it scrolls past that), scrollable.
    if let Some(look) = theme.get("menu").and_then(Value::as_object) {
        if let Some(w) = look.get("min_width").and_then(Value::as_f64) {
            m = m.min_w(px(w as f32));
        }
        if let Some(h) = look.get("max_height").and_then(Value::as_f64) {
            m = m.max_h(px(h as f32)).scrollable(true);
        }
        if let Some(b) = look.get("scrollable").and_then(Value::as_bool) {
            m = m.scrollable(b);
        }
    }
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
                    (Some(true), _, _) => item.icon(menu_icon(theme.get("menu_check_icon").and_then(Value::as_str).unwrap_or("square-check").into()).when_some(accent, |i, c| i.text_color(c))),
                    (Some(false), _, _) => item.icon(menu_icon(theme.get("menu_uncheck_icon").and_then(Value::as_str).unwrap_or("square").into())),
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

/** `props.scrollbar` / `chat.scrollbar`: scrolling | hover | always; never (or unset) draws none. */
pub(crate) fn scrollbar_mode(v: Option<&Value>) -> Option<gpui_kit::component::scroll::ScrollbarMode> {
    use gpui_kit::component::scroll::ScrollbarMode;
    match v.and_then(Value::as_str) {
        Some("scrolling") => Some(ScrollbarMode::Scrolling),
        Some("hover") => Some(ScrollbarMode::Hover),
        Some("always") => Some(ScrollbarMode::Always),
        _ => None,
    }
}

/** The first `text` node (or plain string) under a node, unresolved: a clickable's label when it has no tooltip. */
fn first_text(obj: &Map<String, Value>) -> Option<Value> {
    if obj.get("type").and_then(Value::as_str) == Some("text") {
        return obj.get("props").and_then(|p| p.get("text")).cloned();
    }
    let kids = match obj.get("children")? {
        Value::Array(k) => k.clone(),
        one => vec![one.clone()],
    };
    kids.iter().find_map(|k| match k {
        Value::String(s) => Some(Value::String(s.clone())),
        Value::Object(o) if !o.contains_key("for") => first_text(o),
        _ => None,
    })
}

/** `on.key`: {"KEY": action} or [{key, do}], keys as `cmd-k`, `enter`, `shift-tab`. */
fn key_map(spec: Option<&Value>, resolve: impl Fn(&Value) -> Value) -> Vec<(String, Value)> {
    match spec {
        Some(Value::Array(list)) => list.iter().filter_map(|e| Some((e.get("key")?.as_str()?.to_string(), resolve(e.get("do")?)))).collect(),
        Some(Value::Object(o)) if o.contains_key("key") => key_map(Some(&Value::Array(vec![Value::Object(o.clone())])), resolve),
        Some(Value::Object(o)) => o.iter().map(|(k, a)| (k.clone(), resolve(a))).collect(),
        _ => vec![],
    }
}

/** A select's options as (value, label): [values] or [{value, label}]. */
fn select_options(v: Option<&Value>) -> Vec<(Value, String)> {
    v.and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .map(|o| match o {
                    Value::Object(m) => {
                        let value = m.get("value").or_else(|| m.get("id")).cloned().unwrap_or(Value::Null);
                        let label = m.get("label").map(value_text).unwrap_or_else(|| value_text(&value));
                        (value, label)
                    }
                    other => (other.clone(), value_text(other)),
                })
                .collect()
        })
        .unwrap_or_default()
}

/** Plain text as Markdown that renders as itself (punctuation escaped, line breaks kept). */
pub(crate) fn markdown_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push_str("\\\n");
        }
        for c in line.chars() {
            if "\\`*_{}[]<>()#+-.!|~&".contains(c) {
                out.push('\\');
            }
            out.push(c);
        }
    }
    out
}

/** Hashes a JSON value (absent hashes like null), for keys built from settings nodes. */
fn hash_json(v: Option<&Value>, h: &mut impl Hasher) {
    match v {
        None | Some(Value::Null) => 0u8.hash(h),
        Some(Value::Bool(b)) => (1u8, b).hash(h),
        Some(Value::Number(n)) => (2u8, n.to_string()).hash(h),
        Some(Value::String(s)) => (3u8, s).hash(h),
        Some(Value::Array(a)) => {
            (4u8, a.len()).hash(h);
            a.iter().for_each(|x| hash_json(Some(x), h));
        }
        Some(Value::Object(o)) => {
            (5u8, o.len()).hash(h);
            for (k, x) in o {
                k.hash(h);
                hash_json(Some(x), h);
            }
        }
    }
}

#[cfg(test)]
mod helper_tests {
    use super::{first_text, key_map, markdown_escape, select_options};
    use serde_json::{json, Value};

    #[test]
    fn plain_text_survives_markdown() {
        assert_eq!(markdown_escape("a*b"), "a\\*b");
        assert_eq!(markdown_escape("# x\ny"), "\\# x\\\ny");
    }

    #[test]
    fn clickables_are_named_by_their_text() {
        let o = json!({"type": "row", "children": [{"type": "icon"}, {"type": "text", "props": {"text": "Save"}}]});
        assert_eq!(first_text(o.as_object().unwrap()), Some(json!("Save")));
        let o = json!({"children": ["Hi {{ .x }}"]});
        assert_eq!(first_text(o.as_object().unwrap()), Some(json!("Hi {{ .x }}")));
        assert_eq!(first_text(json!({"children": [{"type": "icon"}]}).as_object().unwrap()), None);
    }

    #[test]
    fn keys_and_options_read_both_shapes() {
        let id = |v: &Value| v.clone();
        assert_eq!(key_map(Some(&json!({"enter": "a", "escape": "b"})), id).len(), 2);
        assert_eq!(key_map(Some(&json!([{"key": "cmd-k", "do": "x"}])), id), vec![("cmd-k".to_string(), json!("x"))]);
        assert_eq!(key_map(Some(&json!({"key": "up", "do": "y"})), id), vec![("up".to_string(), json!("y"))]);
        let opts = select_options(Some(&json!(["a", {"value": 2, "label": "Two"}])));
        assert_eq!(opts, vec![(json!("a"), "a".to_string()), (json!(2), "Two".to_string())]);
    }
}
