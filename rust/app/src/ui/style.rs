/*! Style keys from settings applied to GPUI elements. Colors are `#rrggbb[aa]` or `@token` from the
theme; lengths are points; anything unknown is ignored (and reported in the settings panel). */

use crate::settings::parse_color;
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;
use serde_json::{Map, Value};

pub const STYLE_KEYS: &[(&str, &str)] = &[
    ("width / height", "number (points), \"full\", \"auto\", or \"50%\""),
    ("min_width / min_height / max_width / max_height", "number"),
    ("grow", "number: share of free space along the parent's direction (flex-grow)"),
    ("shrink", "number (flex-shrink); 0 keeps the size"),
    ("padding / margin", "number, [vertical, horizontal], [top, right, bottom, left], or {top,right,bottom,left}"),
    ("gap", "number: space between children"),
    ("direction", "\"row\" or \"column\" (for box)"),
    ("self", "this node's own cross-axis alignment in its parent: start | center | end | stretch"),
    ("align", "cross-axis: start | center | end | stretch | baseline"),
    ("justify", "main-axis: start | center | end | between | around"),
    ("wrap", "bool: wrap children onto new lines"),
    ("background_image", "a picture behind the node's content: a file (absolute, ~/, or in the settings folder) or http(s) URL; works on every node and every style object in settings (chat bubbles, cards, chips, the composer). With `background_fit` (cover, contain, fill, scale_down, none), `background_opacity` (0..1) and `background_dim` (a color laid over it). It follows the node's `radius`. Give `background` a see-through color (or none) so it shows."),
    ("background", "color: `#rrggbb`, `#rrggbbaa`, `hsl(h, s, l)` / `hsl(h, s, l, a)` (degrees, percents, 0..1), `@token`, or `transparent`"),
    ("color", "text color"),
    ("border", "number: border width on all sides; border_top/right/bottom/left for one side"),
    ("border_color", "color"),
    ("radius", "number, or {top_left, top_right, bottom_left, bottom_right}"),
    ("shadow", "sm | md | lg | xl | none"),
    ("opacity", "0..1"),
    ("font_size", "number"),
    ("font_weight", "100..900 or thin | light | normal | medium | semibold | bold | black"),
    ("font_family", "font name, or @font_ui / @font_mono"),
    ("italic", "bool"),
    ("line_height", "number (multiplier when < 4, else points)"),
    ("text_align", "left | center | right"),
    ("truncate", "bool: one line with an ellipsis"),
    ("overflow / overflow_x / overflow_y", "hidden | visible | scroll (for nodes, a `scroll` node scrolls; `scroll` here is for markdown parts like tables)"),
    ("position", "relative | absolute"),
    ("top / right / bottom / left", "number (for position: absolute)"),
    ("cursor", "pointer | text | default | col_resize | row_resize | grab"),
    ("hidden", "bool: render nothing"),
    ("hover", "object of style keys applied while the pointer is over the node"),
    ("active", "object of style keys applied while it's pressed"),
    ("drag_over_item", "object of style keys applied while an accepted dragged item (see `drag` / `on.drop_item`) is over the node"),
    ("drag_over", "object of style keys applied while files are dragged over the node (pair with `on.drop`)"),
];

pub fn color(v: &Value, theme: &Map<String, Value>) -> Option<Hsla> {
    let s = v.as_str()?;
    if s.is_empty() || s == "none" || s == "transparent" {
        return Some(transparent_black());
    }
    if let Some(token) = s.strip_prefix('@') {
        return theme.get(token).and_then(|t| color(t, theme));
    }
    if let Some(args) = s.strip_prefix("hsl(").and_then(|r| r.strip_suffix(')')) {
        let n: Vec<f32> = args.split(',').filter_map(|x| x.trim().trim_end_matches('%').parse().ok()).collect();
        if n.len() >= 3 {
            return Some(hsla(n[0] / 360., n[1] / 100., n[2] / 100., n.get(3).copied().unwrap_or(1.)));
        }
    }
    s.starts_with('#').then(|| parse_color(s))
}

fn num(v: Option<&Value>) -> Option<f32> {
    v.and_then(Value::as_f64).map(|x| x as f32)
}

fn length(v: &Value) -> Option<Length> {
    match v {
        Value::Number(n) => Some(px(n.as_f64()? as f32).into()),
        Value::String(s) if s == "full" => Some(relative(1.).into()),
        Value::String(s) if s == "auto" => Some(Length::Auto),
        Value::String(s) if s.ends_with('%') => s.trim_end_matches('%').parse::<f32>().ok().map(|p| relative(p / 100.).into()),
        _ => None,
    }
}

/** An inset: points, or "full" / "50%" of the parent (so `"bottom": "full"` sits just above it). */
fn inset(v: &Value) -> Option<Length> {
    match v {
        Value::Number(n) => Some(px(n.as_f64()? as f32).into()),
        Value::String(s) if s == "full" => Some(relative(1.).into()),
        Value::String(s) if s.ends_with('%') => s.trim_end_matches('%').parse::<f32>().ok().map(|p| relative(p / 100.).into()),
        _ => None,
    }
}

pub(crate) fn edges(v: &Value) -> Option<[f32; 4]> {
    match v {
        Value::Number(n) => {
            let x = n.as_f64()? as f32;
            Some([x, x, x, x])
        }
        Value::Array(a) if a.len() == 2 => Some([num(a.first())?, num(a.get(1))?, num(a.first())?, num(a.get(1))?]),
        Value::Array(a) if a.len() == 4 => Some([num(a.first())?, num(a.get(1))?, num(a.get(2))?, num(a.get(3))?]),
        Value::Object(o) => Some([
            num(o.get("top")).unwrap_or(0.),
            num(o.get("right")).unwrap_or(0.),
            num(o.get("bottom")).unwrap_or(0.),
            num(o.get("left")).unwrap_or(0.),
        ]),
        _ => None,
    }
}

fn weight(v: &Value) -> Option<FontWeight> {
    if let Some(n) = v.as_f64() {
        return Some(FontWeight(n as f32));
    }
    Some(match v.as_str()? {
        "thin" => FontWeight::THIN,
        "light" => FontWeight::LIGHT,
        "normal" => FontWeight::NORMAL,
        "medium" => FontWeight::MEDIUM,
        "semibold" => FontWeight::SEMIBOLD,
        "bold" => FontWeight::BOLD,
        "black" => FontWeight::BLACK,
        _ => return None,
    })
}

/** Applies every recognised key to a style refinement (shared by the base style and hover/active). */
pub fn refine(s: &mut StyleRefinement, style: &Map<String, Value>, theme: &Map<String, Value>) {
    for (k, v) in style {
        match k.as_str() {
            "width" => s.size.width = length(v),
            "height" => s.size.height = length(v),
            "min_width" => s.min_size.width = length(v),
            "min_height" => s.min_size.height = length(v),
            "max_width" => s.max_size.width = length(v),
            "max_height" => s.max_size.height = length(v),
            "grow" => {
                s.flex_grow = num(Some(v));
                if s.flex_basis.is_none() {
                    s.flex_basis = Some(px(0.).into());
                }
            }
            "shrink" => s.flex_shrink = num(Some(v)),
            "basis" => s.flex_basis = length(v),
            "padding" => {
                if let Some([t, r, b, l]) = edges(v) {
                    s.padding.top = Some(px(t).into());
                    s.padding.right = Some(px(r).into());
                    s.padding.bottom = Some(px(b).into());
                    s.padding.left = Some(px(l).into());
                }
            }
            "margin" => {
                if let Some([t, r, b, l]) = edges(v) {
                    s.margin.top = Some(px(t).into());
                    s.margin.right = Some(px(r).into());
                    s.margin.bottom = Some(px(b).into());
                    s.margin.left = Some(px(l).into());
                }
            }
            "gap" => {
                if let Some(g) = num(Some(v)) {
                    s.gap.width = Some(px(g).into());
                    s.gap.height = Some(px(g).into());
                }
            }
            "direction" => s.flex_direction = Some(if v.as_str() == Some("row") { FlexDirection::Row } else { FlexDirection::Column }),
            "align" => {
                s.align_items = match v.as_str() {
                    Some("start") => Some(AlignItems::FlexStart),
                    Some("center") => Some(AlignItems::Center),
                    Some("end") => Some(AlignItems::FlexEnd),
                    Some("stretch") => Some(AlignItems::Stretch),
                    Some("baseline") => Some(AlignItems::Baseline),
                    _ => s.align_items,
                }
            }
            "justify" => {
                s.justify_content = match v.as_str() {
                    Some("start") => Some(JustifyContent::FlexStart),
                    Some("center") => Some(JustifyContent::Center),
                    Some("end") => Some(JustifyContent::FlexEnd),
                    Some("between") => Some(JustifyContent::SpaceBetween),
                    Some("around") => Some(JustifyContent::SpaceAround),
                    _ => s.justify_content,
                }
            }
            "self" => {
                s.align_self = match v.as_str() {
                    Some("start") => Some(AlignSelf::FlexStart),
                    Some("center") => Some(AlignSelf::Center),
                    Some("end") => Some(AlignSelf::FlexEnd),
                    Some("stretch") => Some(AlignSelf::Stretch),
                    _ => s.align_self,
                }
            }
            "wrap" => s.flex_wrap = Some(if v.as_bool() == Some(true) { FlexWrap::Wrap } else { FlexWrap::NoWrap }),
            "background" => {
                if let Some(c) = color(v, theme) {
                    s.background = Some(c.into());
                }
            }
            "color" => {
                if let Some(c) = color(v, theme) {
                    s.text.color = Some(c);
                }
            }
            "border" | "border_top" | "border_right" | "border_bottom" | "border_left" => {
                let w = num(Some(v)).map(|w| px(w).into());
                match k.as_str() {
                    "border" => {
                        s.border_widths.top = w;
                        s.border_widths.right = w;
                        s.border_widths.bottom = w;
                        s.border_widths.left = w;
                    }
                    "border_top" => s.border_widths.top = w,
                    "border_right" => s.border_widths.right = w,
                    "border_bottom" => s.border_widths.bottom = w,
                    _ => s.border_widths.left = w,
                }
            }
            "border_color" => s.border_color = color(v, theme),
            "radius" => match v {
                Value::Object(o) => {
                    s.corner_radii.top_left = num(o.get("top_left")).map(|x| px(x).into());
                    s.corner_radii.top_right = num(o.get("top_right")).map(|x| px(x).into());
                    s.corner_radii.bottom_left = num(o.get("bottom_left")).map(|x| px(x).into());
                    s.corner_radii.bottom_right = num(o.get("bottom_right")).map(|x| px(x).into());
                }
                _ => {
                    let r = num(Some(v)).map(|x| px(x).into());
                    s.corner_radii.top_left = r;
                    s.corner_radii.top_right = r;
                    s.corner_radii.bottom_left = r;
                    s.corner_radii.bottom_right = r;
                }
            },
            "opacity" => s.opacity = num(Some(v)),
            "font_size" => s.text.font_size = num(Some(v)).map(|x| px(x).into()),
            "font_weight" => s.text.font_weight = weight(v),
            "font_family" => {
                let f = v.as_str().map(|f| f.strip_prefix('@').and_then(|t| theme.get(t)).and_then(Value::as_str).unwrap_or(f));
                s.text.font_family = f.map(|f| f.to_string().into());
            }
            "italic" => s.text.font_style = Some(if v.as_bool() == Some(true) { FontStyle::Italic } else { FontStyle::Normal }),
            "line_height" => {
                if let Some(l) = num(Some(v)) {
                    s.text.line_height = Some(if l < 4. { relative(l).into() } else { px(l).into() });
                }
            }
            "text_align" => {
                s.text.text_align = match v.as_str() {
                    Some("center") => Some(TextAlign::Center),
                    Some("right") => Some(TextAlign::Right),
                    _ => Some(TextAlign::Left),
                }
            }
            "truncate" => {
                if v.as_bool() == Some(true) {
                    s.overflow.x = Some(Overflow::Hidden);
                    s.text.white_space = Some(WhiteSpace::Nowrap);
                    s.text.text_overflow = Some(TextOverflow::Truncate("…".into()));
                }
            }
            "overflow" | "overflow_x" | "overflow_y" => {
                let o = match v.as_str() {
                    Some("hidden") => Overflow::Hidden,
                    Some("scroll") => Overflow::Scroll,
                    _ => Overflow::Visible,
                };
                if k != "overflow_y" {
                    s.overflow.x = Some(o);
                }
                if k != "overflow_x" {
                    s.overflow.y = Some(o);
                }
            }
            "position" => s.position = Some(if v.as_str() == Some("absolute") { Position::Absolute } else { Position::Relative }),
            "top" => s.inset.top = inset(v),
            "right" => s.inset.right = inset(v),
            "bottom" => s.inset.bottom = inset(v),
            "left" => s.inset.left = inset(v),
            "shadow" => {
                // `theme.shadows.NAME`: {color, x, y, blur, spread} (or a list of them) replaces or adds a preset.
                if let Some(custom) = v.as_str().and_then(|n| theme.get("shadows").and_then(|s| s.get(n))) {
                    let one = |c: &Value| BoxShadow {
                        color: c.get("color").and_then(|x| color(x, theme)).unwrap_or(hsla(0., 0., 0., 0.3)),
                        offset: point(px(c.get("x").and_then(Value::as_f64).unwrap_or(0.) as f32), px(c.get("y").and_then(Value::as_f64).unwrap_or(4.) as f32)),
                        blur_radius: px(c.get("blur").and_then(Value::as_f64).unwrap_or(10.) as f32),
                        spread_radius: px(c.get("spread").and_then(Value::as_f64).unwrap_or(0.) as f32),
                        inset: c.get("inset").and_then(Value::as_bool).unwrap_or(false),
                    };
                    s.box_shadow = Some(match custom {
                        Value::Array(list) => list.iter().map(one).collect(),
                        c => vec![one(c)],
                    });
                    continue;
                }
                s.box_shadow = match v.as_str() {
                    Some("sm") => Some(vec![BoxShadow { color: hsla(0., 0., 0., 0.25), offset: point(px(0.), px(1.)), blur_radius: px(2.), spread_radius: px(0.), inset: false }]),
                    Some("md") => Some(vec![BoxShadow { color: hsla(0., 0., 0., 0.3), offset: point(px(0.), px(4.)), blur_radius: px(10.), spread_radius: px(0.), inset: false }]),
                    Some("lg") => Some(vec![BoxShadow { color: hsla(0., 0., 0., 0.35), offset: point(px(0.), px(10.)), blur_radius: px(24.), spread_radius: px(0.), inset: false }]),
                    Some("xl") => Some(vec![BoxShadow { color: hsla(0., 0., 0., 0.4), offset: point(px(0.), px(18.)), blur_radius: px(44.), spread_radius: px(0.), inset: false }]),
                    _ => Some(vec![]),
                }
            }
            "cursor" => {
                s.mouse_cursor = match v.as_str() {
                    Some("pointer") => Some(CursorStyle::PointingHand),
                    Some("text") => Some(CursorStyle::IBeam),
                    Some("col_resize") => Some(CursorStyle::ResizeLeftRight),
                    Some("row_resize") => Some(CursorStyle::ResizeUpDown),
                    Some("grab") => Some(CursorStyle::OpenHand),
                    _ => Some(CursorStyle::Arrow),
                }
            }
            _ => {}
        }
    }
}

/** `apply` for elements that can't hold children (images): no `background_image`. */
pub fn apply_plain<E: Styled>(mut el: E, style: &Map<String, Value>, theme: &Map<String, Value>) -> E {
    refine(el.style(), style, theme);
    el
}

pub fn apply<E: Styled + ParentElement>(mut el: E, style: &Map<String, Value>, theme: &Map<String, Value>) -> E {
    refine(el.style(), style, theme);
    // `background_image` (any node, any style map): a picture behind the node's content, cut to its corners.
    if let Some(src) = style.get("background_image").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        let fit = style.get("background_fit").and_then(Value::as_str).unwrap_or("cover");
        let opacity = style.get("background_opacity").and_then(Value::as_f64).unwrap_or(1.) as f32;
        let radius = match style.get("radius") {
            Some(Value::Number(n)) => n.as_f64().unwrap_or(0.) as f32,
            _ => 0.,
        };
        if style.get("position").is_none() {
            el.style().position = Some(Position::Relative);
        }
        let dim = style.get("background_dim").and_then(|d| color(d, theme));
        el = el.child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .rounded(px(radius))
                .overflow_hidden()
                .child(crate::workspace::image_element(src, fit).size_full().opacity(opacity))
                .when_some(dim, |d, c| d.child(div().absolute().inset_0().bg(c)))
                .into_any_element(),
        );
    }
    el
}
