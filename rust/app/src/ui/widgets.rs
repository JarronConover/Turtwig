/*! Interactive nodes beyond boxes and text: form controls, progress, tabs, drawings and clocks. Each
binds to state like `input` does, so agents read and set them through `.state`. */

use super::expr::{to_val, with};
use super::render::value_text;
use super::style;
use crate::workspace::Workspace;
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::Icon;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use jaq_json::Val;
use resvg::{tiny_skia, usvg};
use serde_json::{json, Map, Value};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

pub const NODE_TYPES: &[(&str, &str)] = &[
    ("input", "A text field bound to a state key: `props.state` (key; the field follows it while not focused), `props.placeholder`, `props.autofocus`, `props.appearance` (false: no border), `props.password` (masked; `props.mask_toggle` adds an eye button), `props.disabled`, `props.max_length`, `props.field` (style object for the field: color, font_size, background, padding...). `props.multiline` makes it a text area (`props.rows`, or `props.min_rows`/`props.max_rows` to grow; Enter adds a line unless `props.submit_on_enter`, cmd-Enter submits). Read the text as `.state.<key>`. Or `props.value` (follows the context while the field isn't focused). `on.submit` runs on Enter, `on.change` on every edit, both with the text as `.input`."),
    ("spinner", "A loading spinner. `props.size`: xsmall | small | medium | large, or points; `props.color` (else style `color`)."),
    ("checkbox", "A checkbox bound to `props.state` (true/false; or `props.checked` without state) with `props.label`. `props.disabled`; looks: `props.box`, `props.box_checked` (style objects), `props.icon` (check), `props.icon_size`. `on.change` gets `.value`. Agents: `ui.click {id}`."),
    ("switch", "A toggle switch, like checkbox: `props.state` | `props.checked`, `props.label`, `props.disabled`; looks: `props.track`, `props.track_on`, `props.thumb` (style objects)."),
    ("slider", "A draggable value bound to `props.state`: `props.min` (0), `props.max` (1), `props.step` (0: continuous), or `props.value` without state. Looks: `props.track`, `props.fill`, `props.thumb` (style objects), `props.height` (18). `on.change` gets `.value`. Agents: state.set, or `ui.click {id: \"ID:inc\"}` / `ID:dec` (one step)."),
    ("progress", "A progress bar: `props.value` of `props.max` (1); `props.indeterminate` animates. Looks: `props.track`, `props.fill` (style objects), `props.color` (the fill), `props.height` (4)."),
    ("tabs", "A row of tabs bound to `props.state` (the selected id; or `props.value`): `props.items` ([{id, label, icon?}] or strings). Looks: `props.tab`, `props.tab_selected` (style objects over it; `hover` inside works), `props.gap`, `props.icon_size`. `on.change` gets `.value`. Agents: `ui.click {id: \"ID:ITEM\"}`."),
    ("svg", "Inline SVG drawn to a picture: `props.markup` (templates allowed, so jq can draw sparklines) or `props.src` (a file, relative to the settings folder). `currentColor` becomes `props.color` (else style `color`). Size: `props.width` / `props.height` (points; default the SVG's own size)."),
    ("chart", "A small chart: `props.kind` line | bar | area, `props.data` ([numbers], [{x, y}] or [{label, value}]), `props.color` (@accent), `props.width` (120) / `props.height` (32), `props.stroke_width` (1.5), `props.min` / `props.max` (default from the data; bars and areas start at 0), `props.fill_opacity` (area, 0.25)."),
    ("timer", "Redraws every `props.every_ms` (1000) and draws its `children` with `.now` (unix seconds) in the context, for clocks, countdowns and elapsed times: `{\"type\": \"timer\", \"children\": [\"{{ .now | floor | todate }}\"]}`."),
];

/** Events these nodes fire besides the ones every node has. */
pub const EVENTS: &[&str] = &["change"];

/** Widgets that draw their own `children` (render.rs must not draw them again). */
pub const OWNS_CHILDREN: &[&str] = &["timer"];

thread_local! {
    static TEXTAREAS: RefCell<HashMap<String, Entity<TextareaState>>> = RefCell::new(HashMap::new());
    static CHANGES: RefCell<HashMap<String, (Value, Val)>> = RefCell::new(HashMap::new());
    static TIMERS: RefCell<HashMap<String, Task<()>>> = RefCell::new(HashMap::new());
    static SVGS: RefCell<HashMap<String, Option<Arc<Image>>>> = RefCell::new(HashMap::new());
    static FONTS: RefCell<Option<Arc<usvg::fontdb::Database>>> = const { RefCell::new(None) };
    static SLIDING: RefCell<Option<String>> = const { RefCell::new(None) };
    static APPLIED: RefCell<HashMap<String, (String, bool)>> = RefCell::new(HashMap::new());
    /** Text the app itself put in a field (its change event arrives later), so that isn't reported as an edit. */
    static SET_BY_APP: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
}

/** Multi-line `input` fields by node id (single-line ones are in `Workspace::inputs`). */
pub(crate) fn textareas() -> Vec<(String, Entity<TextareaState>)> {
    TEXTAREAS.with(|t| t.borrow().iter().map(|(k, v)| (k.clone(), v.clone())).collect())
}

pub(crate) fn textarea(id: &str) -> Option<Entity<TextareaState>> {
    TEXTAREAS.with(|t| t.borrow().get(id).cloned())
}

/** Whether a field's placeholder or masking differ from what was last applied (recording the new ones). */
fn changed_look(id: &str, placeholder: &str, masked: bool) -> bool {
    APPLIED.with(|a| {
        let mut a = a.borrow_mut();
        let now = (placeholder.to_string(), masked);
        let changed = a.get(id).is_some_and(|old| *old != now);
        a.insert(id.to_string(), now);
        changed
    })
}

fn num(props: &Map<String, Value>, k: &str, d: f64) -> f64 {
    props.get(k).and_then(Value::as_f64).unwrap_or(d)
}

fn flag(props: &Map<String, Value>, k: &str) -> bool {
    props.get(k).and_then(Value::as_bool) == Some(true)
}

/** `defaults` with the style object `props[key]` over it. */
fn look(defaults: Value, over: Option<&Value>) -> Map<String, Value> {
    let mut m = match defaults {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    if let Some(Value::Object(o)) = over {
        for (k, v) in o {
            m.insert(k.clone(), v.clone());
        }
    }
    m
}

/** A div in `look`, with its `hover` object applied while hovered. */
fn styled(base: Div, look: &Map<String, Value>, theme: &Map<String, Value>) -> Div {
    let el = style::apply(base, look, theme);
    match look.get("hover") {
        Some(Value::Object(h)) => {
            let (h, theme) = (h.clone(), theme.clone());
            el.hover(move |mut s| {
                style::refine(&mut s, &h, &theme);
                s
            })
        }
        _ => el,
    }
}

fn is_on(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty() && s != "false",
        Value::Number(n) => n.as_f64().unwrap_or(0.) != 0.,
        _ => false,
    }
}

/** Where a slider lands for a pointer at `frac` (0..1 along the track). */
pub(crate) fn slider_value(frac: f64, min: f64, max: f64, step: f64) -> f64 {
    snap(min + frac.clamp(0., 1.) * (max - min), min, max, step)
}

pub(crate) fn snap(v: f64, min: f64, max: f64, step: f64) -> f64 {
    let (lo, hi) = if min <= max { (min, max) } else { (max, min) };
    let v = if step > 0. { min + ((v - min) / step).round() * step } else { v };
    // Rounding off float noise so 0.1 steps read as 0.3, not 0.30000000000000004.
    ((v.clamp(lo, hi)) * 1e9).round() / 1e9
}

/** How full a bar of `value` out of `max` is (0..1). */
pub(crate) fn fraction(value: f64, max: f64) -> f32 {
    if max <= 0. || !value.is_finite() {
        return 0.;
    }
    (value / max).clamp(0., 1.) as f32
}

/** Chart data as (label, value): numbers, {x, y}, or {label, value}. */
pub(crate) fn chart_points(data: &Value) -> Vec<(String, f64)> {
    let Value::Array(items) = data else { return vec![] };
    items
        .iter()
        .filter_map(|it| match it {
            Value::Number(n) => Some((String::new(), n.as_f64()?)),
            Value::Object(o) => {
                let v = o.get("y").or_else(|| o.get("value")).and_then(Value::as_f64)?;
                let l = o.get("x").or_else(|| o.get("label")).map(value_text).unwrap_or_default();
                Some((l, v))
            }
            Value::String(s) => Some((String::new(), s.trim().parse().ok()?)),
            _ => None,
        })
        .collect()
}

/** Values placed in a `w` x `h` box (y down): evenly spaced along x, scaled between `lo` and `hi`. */
pub(crate) fn scale_points(values: &[f64], w: f64, h: f64, lo: f64, hi: f64) -> Vec<(f64, f64)> {
    let span = if (hi - lo).abs() < f64::EPSILON { 1. } else { hi - lo };
    let n = values.len();
    values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let x = if n <= 1 { w / 2. } else { w * i as f64 / (n - 1) as f64 };
            (x, h - ((v - lo) / span).clamp(0., 1.) * h)
        })
        .collect()
}

/** SVG markup for a chart of `values` (see the `chart` node). */
pub(crate) fn chart_svg(kind: &str, values: &[f64], w: f64, h: f64, color: &str, stroke: f64, range: (Option<f64>, Option<f64>), fill_opacity: f64) -> String {
    let from_zero = kind != "line";
    let data_lo = values.iter().copied().fold(f64::INFINITY, f64::min);
    let data_hi = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let lo = range.0.unwrap_or(if from_zero { data_lo.min(0.) } else { data_lo });
    let hi = range.1.unwrap_or(if from_zero { data_hi.max(0.) } else { data_hi });
    let pad = stroke / 2.;
    let mut body = String::new();
    if kind == "bar" {
        let n = values.len().max(1) as f64;
        let slot = w / n;
        let bar = (slot * 0.8).max(1.);
        let span = if (hi - lo).abs() < f64::EPSILON { 1. } else { hi - lo };
        let zero = h - ((0f64.clamp(lo, hi) - lo) / span) * h;
        for (i, v) in values.iter().enumerate() {
            let y = h - ((v.clamp(lo, hi) - lo) / span) * h;
            let (top, height) = if y < zero { (y, zero - y) } else { (zero, y - zero) };
            body.push_str(&format!(r#"<rect x="{:.2}" y="{:.2}" width="{:.2}" height="{:.2}" fill="{color}"/>"#, slot * i as f64 + (slot - bar) / 2., top, bar, height.max(0.5)));
        }
    } else {
        let pts = scale_points(values, w - stroke, h - stroke, lo, hi);
        let line: Vec<String> = pts.iter().map(|(x, y)| format!("{:.2},{:.2}", x + pad, y + pad)).collect();
        if kind == "area" && !pts.is_empty() {
            let (first, last) = (pts[0].0 + pad, pts[pts.len() - 1].0 + pad);
            body.push_str(&format!(r#"<polygon points="{first:.2},{h:.2} {} {last:.2},{h:.2}" fill="{color}" fill-opacity="{fill_opacity}"/>"#, line.join(" ")));
        }
        body.push_str(&format!(r#"<polyline points="{}" fill="none" stroke="{color}" stroke-width="{stroke}" stroke-linejoin="round" stroke-linecap="round"/>"#, line.join(" ")));
    }
    format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}">{body}</svg>"#)
}

/** The SVG picture cache key: its markup and the pixel size it's drawn at. */
pub(crate) fn svg_key(markup: &str, w: u32, h: u32) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    markup.hash(&mut hasher);
    format!("{w}x{h}:{:016x}", hasher.finish())
}

fn hex(c: Hsla) -> String {
    let r = Rgba::from(c);
    let b = |x: f32| (x.clamp(0., 1.) * 255.).round() as u8;
    format!("#{:02x}{:02x}{:02x}", b(r.r), b(r.g), b(r.b))
}

fn svg_options() -> usvg::Options<'static> {
    let fonts = FONTS.with(|f| {
        f.borrow_mut()
            .get_or_insert_with(|| {
                let mut db = usvg::fontdb::Database::new();
                db.load_system_fonts();
                Arc::new(db)
            })
            .clone()
    });
    usvg::Options { fontdb: fonts, ..Default::default() }
}

/** The SVG's own size in points, if it parses. */
fn svg_size(markup: &str) -> Option<(f32, f32)> {
    let tree = usvg::Tree::from_str(markup, &svg_options()).ok()?;
    Some((tree.size().width(), tree.size().height()))
}

/** `markup` drawn into `w` x `h` pixels (fit inside, centered), cached. */
fn svg_image(markup: &str, w: u32, h: u32) -> Option<Arc<Image>> {
    let key = svg_key(markup, w, h);
    if let Some(hit) = SVGS.with(|m| m.borrow().get(&key).cloned()) {
        return hit;
    }
    let png = (|| {
        let tree = usvg::Tree::from_str(markup, &svg_options()).ok()?;
        let mut pm = tiny_skia::Pixmap::new(w.max(1), h.max(1))?;
        let (sw, sh) = (tree.size().width().max(0.001), tree.size().height().max(0.001));
        let scale = (w as f32 / sw).min(h as f32 / sh);
        let (dx, dy) = ((w as f32 - sw * scale) / 2., (h as f32 - sh * scale) / 2.);
        resvg::render(&tree, tiny_skia::Transform::from_row(scale, 0., 0., scale, dx, dy), &mut pm.as_mut());
        pm.encode_png().ok()
    })();
    let image = png.map(|b| Arc::new(Image::from_bytes(ImageFormat::Png, b)));
    SVGS.with(|m| {
        let mut m = m.borrow_mut();
        if m.len() > 256 {
            m.clear();
        }
        m.insert(key, image.clone());
    });
    image
}

impl Workspace {
    /** The content of a widget node (`kind` is one of NODE_TYPES). */
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render_widget(&mut self, kind: &str, id: &str, props: &Map<String, Value>, obj: &Map<String, Value>, ctx: &Val, st: &Map<String, Value>, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        match kind {
            "input" => self.input_widget(id, props, obj, ctx, window, cx),
            "spinner" => {
                let theme = &self.settings.theme;
                let color = props.get("color").or_else(|| st.get("color")).and_then(|c| style::color(c, theme));
                match props.get("size") {
                    Some(Value::Number(n)) => vec![self.sized_spinner(n.as_f64().unwrap_or(16.) as f32, color, cx)],
                    other => vec![self.spinner(other.and_then(Value::as_str), color, cx)],
                }
            }
            "checkbox" | "switch" => vec![self.toggle_widget(kind, id, props, obj, ctx, cx)],
            "slider" => vec![self.slider_widget(id, props, obj, ctx, cx)],
            "progress" => vec![self.progress_widget(props, cx)],
            "tabs" => vec![self.tabs_widget(id, props, obj, ctx, cx)],
            "svg" | "chart" => {
                let theme = &self.settings.theme;
                let color = props.get("color").or_else(|| st.get("color")).and_then(|c| style::color(c, theme));
                let markup = if kind == "chart" {
                    let values: Vec<f64> = chart_points(props.get("data").unwrap_or(&Value::Null)).into_iter().map(|p| p.1).collect();
                    let color = color.or_else(|| style::color(&Value::from("@accent"), theme)).map(hex).unwrap_or_else(|| "#488efc".into());
                    let range = (props.get("min").and_then(Value::as_f64), props.get("max").and_then(Value::as_f64));
                    let kind = props.get("kind").and_then(Value::as_str).unwrap_or("line");
                    chart_svg(kind, &values, num(props, "width", 120.), num(props, "height", 32.), &color, num(props, "stroke_width", 1.5), range, num(props, "fill_opacity", 0.25))
                } else {
                    let raw = match (props.get("markup"), props.get("src")) {
                        (Some(m), _) => value_text(m),
                        (None, Some(src)) => {
                            let p = std::path::PathBuf::from(crate::script::expand(&value_text(src)));
                            std::fs::read_to_string(if p.is_absolute() { p } else { crate::settings::dir().join(p) }).unwrap_or_default()
                        }
                        _ => String::new(),
                    };
                    match color {
                        Some(c) => raw.replace("currentColor", &hex(c)),
                        None => raw,
                    }
                };
                let own = svg_size(&markup);
                let w = props.get("width").and_then(Value::as_f64).map(|x| x as f32).or(own.map(|s| s.0)).unwrap_or(0.);
                let h = props.get("height").and_then(Value::as_f64).map(|x| x as f32).or(own.map(|s| s.1)).unwrap_or(0.);
                let scale = window.scale_factor();
                let (pw, ph) = ((w * scale).ceil() as u32, (h * scale).ceil() as u32);
                if pw == 0 || ph == 0 {
                    return vec![];
                }
                svg_image(&markup, pw, ph).map(|i| img(i).w(px(w)).h(px(h)).flex_none().into_any_element()).into_iter().collect()
            }
            "timer" => {
                let every = num(props, "every_ms", 1000.).max(16.) as u64;
                let key = id.to_string();
                if !TIMERS.with(|t| t.borrow().contains_key(&key)) {
                    let owner = super::region::owner();
                    let task = cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(std::time::Duration::from_millis(every)).await;
                        TIMERS.with(|t| t.borrow_mut().remove(&key));
                        let _ = this.update(cx, |ws, cx| ws.notify_owner(&owner, cx));
                    });
                    TIMERS.with(|t| t.borrow_mut().insert(id.to_string(), task));
                }
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.);
                let c = with(ctx, vec![("now".into(), to_val(&json!(now)))]);
                match obj.get("children") {
                    Some(Value::Array(kids)) => kids.iter().enumerate().flat_map(|(i, k)| self.render(k, &c, &format!("{id}/{i}"), window, cx)).collect(),
                    Some(one) => self.render(one, &c, &format!("{id}/0"), window, cx),
                    None => vec![],
                }
            }
            _ => vec![],
        }
    }

    fn sized_spinner(&mut self, size: f32, color: Option<Hsla>, cx: &mut Context<Self>) -> AnyElement {
        let t = self.anim_phase(self.settings.num("motion.spinner.period_ms", 800.) as f32, cx);
        let icon = match self.settings.get("motion.spinner.icon").and_then(Value::as_str) {
            Some(name) => Icon::empty().path(format!("icons/{name}.svg")),
            None => Icon::new(gpui_kit::component::IconName::Loader),
        };
        let icon = icon.size(px(size)).when_some(color, |i, c| i.text_color(c));
        div().child(icon.rotate(percentage(ease_in_out(t)))).into_any_element()
    }

    /** The node's bound state key and its value (state first, else `props[fallback]`). */
    fn bound(&self, props: &Map<String, Value>, fallback: &str) -> (Option<String>, Value) {
        let key = props.get("state").and_then(Value::as_str).filter(|k| !k.is_empty()).map(str::to_string);
        let v = key.as_ref().and_then(|k| self.state.get(k).cloned()).or_else(|| props.get(fallback).cloned()).unwrap_or(Value::Null);
        (key, v)
    }

    /** What setting a widget to `value` does: store it in state, then run `on.change` with `.value`. */
    pub(crate) fn change_action(&self, key: Option<&str>, value: Value, on_change: Option<&Value>, ctx: &Val) -> Value {
        let mut list = vec![];
        if let Some(k) = key {
            list.push(json!({"do": "state.set", "key": k, "value": value}));
        }
        if let Some(a) = on_change {
            list.push(self.exprs.resolve_action(a, &with(ctx, vec![("value".into(), to_val(&value))])));
        }
        Value::Array(list)
    }

    fn label_of(props: &Map<String, Value>, id: &str) -> String {
        props.get("label").map(value_text).filter(|l| !l.is_empty()).unwrap_or_else(|| id.to_string())
    }

    fn toggle_widget(&mut self, kind: &str, id: &str, props: &Map<String, Value>, obj: &Map<String, Value>, ctx: &Val, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.settings.theme.clone();
        let (key, v) = self.bound(props, "checked");
        let on = is_on(&v);
        let disabled = flag(props, "disabled");
        let action = self.change_action(key.as_deref(), Value::Bool(!on), obj.get("on").and_then(|o| o.get("change")), ctx);
        if !disabled {
            self.clickables.push((id.to_string(), Self::label_of(props, id), action.clone()));
        }
        let mark: AnyElement = if kind == "switch" {
            let track = look(json!({"width": 28, "height": 16, "radius": 8, "background": "@line", "padding": 2, "direction": "row", "align": "center"}), props.get("track"));
            let track = if on { look(Value::Object(track), Some(&props.get("track_on").cloned().unwrap_or(json!({"background": "@accent", "justify": "end"})))) } else { track };
            let thumb = look(json!({"width": 12, "height": 12, "radius": 6, "background": "#ffffff"}), props.get("thumb"));
            styled(div().flex().flex_none(), &track, &theme).child(styled(div(), &thumb, &theme)).into_any_element()
        } else {
            let base = look(json!({"width": 16, "height": 16, "radius": 4, "border": 1, "border_color": "@line", "align": "center", "justify": "center"}), props.get("box"));
            let lk = if on { look(Value::Object(base), Some(&props.get("box_checked").cloned().unwrap_or(json!({"background": "@accent", "border_color": "@accent", "color": "#ffffff"})))) } else { base };
            let icon = props.get("icon").map(value_text).unwrap_or_else(|| "check".into());
            let icon_color = lk.get("color").and_then(|c| style::color(c, &theme));
            styled(div().flex().flex_none(), &lk, &theme)
                .when(on, |d| d.child(Icon::empty().path(format!("icons/{icon}.svg")).size(px(num(props, "icon_size", 12.) as f32)).when_some(icon_color, |i, c| i.text_color(c))))
                .into_any_element()
        };
        let label = props.get("label").map(value_text).filter(|l| !l.is_empty());
        div()
            .id(SharedString::from(format!("{id}:toggle")))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(num(props, "gap", 8.) as f32))
            .when(disabled, |d| d.opacity(0.5))
            .when(!disabled, |d| d.cursor_pointer().on_click(cx.listener(move |ws, _, window, cx| ws.run_action(&action, window, cx))))
            .child(mark)
            .children(label)
            .into_any_element()
    }

    fn slider_widget(&mut self, id: &str, props: &Map<String, Value>, obj: &Map<String, Value>, ctx: &Val, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.settings.theme.clone();
        let (min, max, step) = (num(props, "min", 0.), num(props, "max", 1.), num(props, "step", 0.).max(0.));
        let (key, v) = self.bound(props, "value");
        let value = v.as_f64().or_else(|| value_text(&v).parse().ok()).unwrap_or(min);
        let frac = if (max - min).abs() < f64::EPSILON { 0. } else { ((value - min) / (max - min)).clamp(0., 1.) } as f32;
        let on_change = obj.get("on").and_then(|o| o.get("change")).cloned();
        let nudge = if step > 0. { step } else { (max - min) / 10. };
        for (suffix, d) in [("inc", nudge), ("dec", -nudge)] {
            let a = self.change_action(key.as_deref(), json!(snap(value + d, min, max, step)), on_change.as_ref(), ctx);
            self.clickables.push((format!("{id}:{suffix}"), format!("{} {suffix}", Self::label_of(props, id)), a));
        }
        let track = look(json!({"height": 4, "radius": 2, "background": "@line", "width": "full"}), props.get("track"));
        let fill = look(json!({"height": "full", "radius": 2, "background": "@accent"}), props.get("fill"));
        let thumb = look(json!({"width": 14, "height": 14, "radius": 7, "background": "#ffffff", "shadow": "sm"}), props.get("thumb"));
        let thumb_w = thumb.get("width").and_then(Value::as_f64).unwrap_or(14.) as f32;
        let bounds: Rc<Cell<Bounds<Pixels>>> = Rc::new(Cell::new(Bounds::default()));
        let set = {
            let (key, ctx, on_change, bounds) = (key.clone(), ctx.clone(), on_change.clone(), bounds.clone());
            move |ws: &mut Workspace, x: Pixels, window: &mut Window, cx: &mut Context<Workspace>| {
                let b = bounds.get();
                let f = f64::from(f32::from(x - b.origin.x) / f32::from(b.size.width).max(1.));
                let v = slider_value(f, min, max, step);
                let current = key.as_ref().and_then(|k| ws.state.get(k)).and_then(Value::as_f64);
                if current != Some(v) {
                    let a = ws.change_action(key.as_deref(), json!(v), on_change.as_ref(), &ctx);
                    ws.run_action(&a, window, cx);
                }
            }
        };
        let set = Rc::new(set);
        let (down_set, sid) = (set.clone(), id.to_string());
        let view = cx.entity().downgrade();
        let (move_id, capture) = (id.to_string(), bounds.clone());
        let watcher = canvas(
            move |b, _, _| capture.set(b),
            move |_, _, window, _| {
                let (view, set, me) = (view.clone(), set.clone(), move_id.clone());
                window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble || !SLIDING.with(|s| s.borrow().as_deref() == Some(me.as_str())) {
                        return;
                    }
                    if e.pressed_button != Some(MouseButton::Left) {
                        SLIDING.with(|s| *s.borrow_mut() = None);
                        return;
                    }
                    let _ = view.update(cx, |ws, cx| set(ws, e.position.x, window, cx));
                });
                window.on_mouse_event(|_: &MouseUpEvent, _, _, _| SLIDING.with(|s| *s.borrow_mut() = None));
            },
        )
        .absolute()
        .size_full()
        .top_0()
        .left_0();
        div()
            .id(SharedString::from(format!("{id}:slider")))
            .relative()
            .w_full()
            .h(px(num(props, "height", 18.) as f32))
            .flex()
            .items_center()
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, cx.listener(move |ws, e: &MouseDownEvent, window, cx| {
                gpui_kit::base::GlobalState::suppress_text_selection(cx);
                SLIDING.with(|s| *s.borrow_mut() = Some(sid.clone()));
                down_set(ws, e.position.x, window, cx);
                cx.stop_propagation();
            }))
            .child(styled(div().relative(), &track, &theme).child(styled(div().absolute().top_0().left_0().w(relative(frac)), &fill, &theme)))
            .child(div().absolute().left(relative(frac)).ml(px(-thumb_w / 2.)).child(styled(div(), &thumb, &theme)))
            .child(watcher)
            .into_any_element()
    }

    fn progress_widget(&mut self, props: &Map<String, Value>, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.settings.theme.clone();
        let frac = fraction(num(props, "value", 0.), num(props, "max", 1.));
        let h = num(props, "height", 4.);
        let track = look(json!({"height": h, "radius": h / 2., "background": "@line", "width": "full", "overflow": "hidden"}), props.get("track"));
        let mut fill_default = json!({"height": "full", "radius": h / 2., "background": "@accent"});
        if let Some(c) = props.get("color") {
            fill_default["background"] = c.clone();
        }
        let fill = look(fill_default, props.get("fill"));
        let bar = if flag(props, "indeterminate") {
            let t = self.anim_phase(num(props, "period_ms", 1200.) as f32, cx);
            div().absolute().top_0().h_full().w(relative(0.3)).left(relative(t * 1.3 - 0.3))
        } else {
            div().absolute().top_0().left_0().h_full().w(relative(frac))
        };
        styled(div().relative(), &track, &theme).child(styled(bar, &fill, &theme)).into_any_element()
    }

    fn tabs_widget(&mut self, id: &str, props: &Map<String, Value>, obj: &Map<String, Value>, ctx: &Val, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.settings.theme.clone();
        let (key, v) = self.bound(props, "value");
        let items: Vec<(String, String, Option<String>)> = match props.get("items") {
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(|it| match it {
                    Value::Object(o) => {
                        let tid = o.get("id").or_else(|| o.get("label")).map(value_text)?;
                        Some((tid.clone(), o.get("label").map(value_text).unwrap_or(tid), o.get("icon").map(value_text)))
                    }
                    other => Some((value_text(other), value_text(other), None)),
                })
                .collect(),
            _ => vec![],
        };
        let selected = Some(value_text(&v)).filter(|s| !s.is_empty()).or_else(|| items.first().map(|i| i.0.clone()));
        let base = look(json!({"padding": [4, 10], "radius": 6, "color": "@text_muted", "direction": "row", "align": "center", "gap": 6, "hover": {"color": "@text"}}), props.get("tab"));
        let chosen = look(Value::Object(base.clone()), Some(&props.get("tab_selected").cloned().unwrap_or(json!({"background": "@hover", "color": "@text"}))));
        let on_change = obj.get("on").and_then(|o| o.get("change")).cloned();
        let icon_size = num(props, "icon_size", 13.) as f32;
        let mut row = div().flex().flex_row().items_center().gap(px(num(props, "gap", 2.) as f32));
        for (tid, label, icon) in items {
            let action = self.change_action(key.as_deref(), Value::String(tid.clone()), on_change.as_ref(), ctx);
            self.clickables.push((format!("{id}:{tid}"), label.clone(), action.clone()));
            let lk = if selected.as_deref() == Some(tid.as_str()) { &chosen } else { &base };
            let color = lk.get("color").and_then(|c| style::color(c, &theme));
            row = row.child(
                styled(div().flex().cursor_pointer(), lk, &theme)
                    .id(SharedString::from(format!("{id}:{tid}")))
                    .when_some(icon, |d, i| d.child(Icon::empty().path(format!("icons/{i}.svg")).size(px(icon_size)).when_some(color, |i, c| i.text_color(c))))
                    .child(label)
                    .on_click(cx.listener(move |ws, _, window, cx| ws.run_action(&action, window, cx))),
            );
        }
        row.into_any_element()
    }

    fn input_widget(&mut self, id: &str, props: &Map<String, Value>, obj: &Map<String, Value>, ctx: &Val, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = self.settings.theme.clone();
        let key = props.get("state").and_then(Value::as_str).unwrap_or("").to_string();
        let multiline = flag(props, "multiline");
        let max_len = props.get("max_length").and_then(Value::as_u64).map(|n| n as usize);
        let placeholder = props.get("placeholder").map(value_text).unwrap_or_default();
        let start = self.state.get(&key).map(value_text).unwrap_or_default();
        match obj.get("on").and_then(|o| o.get("submit")) {
            Some(a) => self.input_submits.insert(id.to_string(), (a.clone(), ctx.clone())),
            None => self.input_submits.remove(id),
        };
        CHANGES.with(|c| match obj.get("on").and_then(|o| o.get("change")) {
            Some(a) => c.borrow_mut().insert(id.to_string(), (a.clone(), ctx.clone())),
            None => c.borrow_mut().remove(id),
        });
        // `value` follows the context while the field isn't being edited; else the bound state does.
        let want = props.get("value").map(value_text).or_else(|| (!key.is_empty()).then(|| start.clone()));
        let field = look(json!({}), props.get("field"));
        let disabled = flag(props, "disabled");
        let appearance = props.get("appearance").and_then(Value::as_bool).unwrap_or(true);
        let submit_on_enter = flag(props, "submit_on_enter");
        let (k, input_id) = (key.clone(), id.to_string());
        let app_id = id.to_string();
        let on_edit = move |ws: &mut Workspace, text: String, cx: &mut Context<Workspace>| {
            if SET_BY_APP.with(|m| m.borrow_mut().remove(&app_id)).as_deref() == Some(text.as_str()) {
                return;
            }
            if !k.is_empty() {
                ws.set_state(k.clone(), Value::String(text.clone()));
            }
            if let Some((a, c)) = CHANGES.with(|m| m.borrow().get(&input_id).cloned()) {
                let resolved = ws.exprs.resolve_action(&a, &with(&c, vec![("input".into(), to_val(&Value::String(text)))]));
                ws.pending_actions.push(resolved);
            }
            cx.notify();
        };
        let submit_id = id.to_string();
        let on_submit = move |ws: &mut Workspace, text: String, window: &mut Window, cx: &mut Context<Workspace>| {
            if let Some((action, ctx)) = ws.input_submits.get(&submit_id).cloned() {
                let c = with(&ctx, vec![("input".into(), to_val(&Value::String(text)))]);
                let resolved = ws.exprs.resolve_action(&action, &c);
                ws.pending_actions.push(resolved);
                // Leave the field so it shows the saved value again.
                window.blur(cx);
                cx.notify();
            }
        };
        if multiline {
            let entity = match textarea(id) {
                Some(e) => e,
                None => {
                    let rows = num(props, "rows", 3.).max(1.) as usize;
                    let e = cx.new(|cx| {
                        let s = TextareaState::new(window, cx).placeholder(placeholder.clone()).default_value(start.clone()).submit_on_enter(submit_on_enter);
                        match props.get("max_rows").and_then(Value::as_u64) {
                            Some(m) => s.auto_grow(num(props, "min_rows", 1.).max(1.) as usize, m as usize),
                            None => s.rows(rows),
                        }
                    });
                    self._subscriptions.push(cx.subscribe_in(&e, window, move |ws: &mut Workspace, e, ev: &InputEvent, window, cx| match ev {
                        InputEvent::Change => {
                            let mut text = e.read(cx).value().to_string();
                            if let Some(n) = max_len.filter(|n| text.chars().count() > *n) {
                                text = text.chars().take(n).collect();
                                let t = text.clone();
                                e.update(cx, |s, cx| s.set_value(t, window, cx));
                            }
                            on_edit(ws, text, cx);
                        }
                        InputEvent::PressEnter { secondary, .. } if *secondary || submit_on_enter => {
                            let text = e.read(cx).value().to_string();
                            on_submit(ws, text, window, cx);
                        }
                        _ => {}
                    }));
                    if flag(props, "autofocus") {
                        e.update(cx, |s, cx| s.focus(window, cx));
                    }
                    TEXTAREAS.with(|t| t.borrow_mut().insert(id.to_string(), e.clone()));
                    e
                }
            };
            if changed_look(id, &placeholder, false) {
                entity.update(cx, |s, cx| s.set_placeholder(placeholder, window, cx));
            }
            if let Some(v) = want {
                if !entity.read(cx).focus_handle(cx).is_focused(window) && entity.read(cx).value().as_ref() != v.as_str() {
                    SET_BY_APP.with(|m| m.borrow_mut().insert(id.to_string(), v.clone()));
                    entity.update(cx, |s, cx| s.set_value(v, window, cx));
                }
            }
            return vec![style::apply_plain(Textarea::new(&entity).appearance(appearance).disabled(disabled), &field, &theme).into_any_element()];
        }
        let password = flag(props, "password");
        let entity = match self.inputs.get(id) {
            Some(e) => e.clone(),
            None => {
                let e = cx.new(|cx| {
                    let s = InputState::new(window, cx).placeholder(placeholder.clone()).default_value(start.clone()).masked(password);
                    match max_len {
                        Some(n) => s.validate(move |t, _| t.chars().count() <= n),
                        None => s,
                    }
                });
                self._subscriptions.push(cx.subscribe_in(&e, window, move |ws: &mut Workspace, e, ev: &InputEvent, window, cx| match ev {
                    InputEvent::Change => {
                        let text = e.read(cx).value().to_string();
                        on_edit(ws, text, cx);
                    }
                    InputEvent::PressEnter { .. } => {
                        let text = e.read(cx).value().to_string();
                        on_submit(ws, text, window, cx);
                    }
                    _ => {}
                }));
                if flag(props, "autofocus") {
                    e.update(cx, |s, cx| s.focus(window, cx));
                }
                self.inputs.insert(id.to_string(), e.clone());
                e
            }
        };
        if changed_look(id, &placeholder, password) {
            entity.update(cx, |s, cx| {
                s.set_placeholder(placeholder, window, cx);
                s.set_masked(password, window, cx);
            });
        }
        if let Some(v) = want {
            if !entity.read(cx).focus_handle(cx).is_focused(window) && entity.read(cx).value().as_ref() != v.as_str() {
                SET_BY_APP.with(|m| m.borrow_mut().insert(id.to_string(), v.clone()));
                entity.update(cx, |s, cx| s.set_value(v, window, cx));
            }
        }
        let input = Input::new(&entity).appearance(appearance).disabled(disabled).when(password && flag(props, "mask_toggle"), |i| i.mask_toggle());
        vec![style::apply_plain(input, &field, &theme).into_any_element()]
    }
}

#[cfg(test)]
mod tests {
    use super::{chart_points, chart_svg, fraction, scale_points, slider_value, snap, svg_key};
    use serde_json::json;

    #[test]
    fn sliders_snap_to_steps_inside_the_range() {
        assert_eq!(slider_value(0.5, 0., 10., 1.), 5.);
        assert_eq!(slider_value(0.34, 0., 10., 1.), 3.);
        assert_eq!(slider_value(2., 0., 10., 0.), 10.);
        assert_eq!(slider_value(-1., 0., 10., 0.), 0.);
        assert_eq!(snap(0.29, 0., 1., 0.1), 0.3);
        assert_eq!(snap(7., 2., 8., 4.), 6., "steps count from min");
    }

    #[test]
    fn progress_clamps() {
        assert_eq!(fraction(0.5, 1.), 0.5);
        assert_eq!(fraction(30., 20.), 1.);
        assert_eq!(fraction(-3., 10.), 0.);
        assert_eq!(fraction(1., 0.), 0.);
        assert_eq!(fraction(f64::NAN, 1.), 0.);
    }

    #[test]
    fn chart_data_in_every_shape() {
        let p = chart_points(&json!([1, {"x": "a", "y": 2}, {"label": "b", "value": 3.5}, "4", null]));
        assert_eq!(p, vec![(String::new(), 1.), ("a".into(), 2.), ("b".into(), 3.5), (String::new(), 4.)]);
        let s = scale_points(&[0., 5., 10.], 100., 20., 0., 10.);
        assert_eq!(s, vec![(0., 20.), (50., 10.), (100., 0.)]);
        assert_eq!(scale_points(&[3., 3.], 10., 10., 3., 3.), vec![(0., 10.), (10., 10.)], "a flat series doesn't divide by zero");
        let svg = chart_svg("bar", &[1., 2.], 20., 10., "#ff0000", 1., (None, None), 0.25);
        assert_eq!(svg.matches("<rect").count(), 2);
        assert!(chart_svg("area", &[1., 2.], 20., 10., "#ff0000", 1., (None, None), 0.25).contains("<polygon"));
    }

    #[test]
    fn svg_cache_keys_follow_markup_and_size() {
        let a = svg_key("<svg/>", 10, 10);
        assert_eq!(a, svg_key("<svg/>", 10, 10));
        assert_ne!(a, svg_key("<svg/>", 20, 10));
        assert_ne!(a, svg_key("<svg></svg>", 10, 10));
    }
}
