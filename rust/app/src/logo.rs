/*! The logo: SVG art from settings `logo`, drawn on a rounded tile. The sidebar's `logo` node, the Dock
icon, and the app bundle's icon all come from it. */

use resvg::{tiny_skia, usvg};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::Arc;

pub const DEFAULT_ART: &str = include_str!("../logo.svg");

/** `logo` settings with a node's props laid over them. */
pub fn spec(logo: &Value, over: &Map<String, Value>) -> Map<String, Value> {
    let mut s = logo.as_object().cloned().unwrap_or_default();
    for (k, v) in over {
        s.insert(k.clone(), v.clone());
    }
    s
}

/** SVG markup as given, or the file it names (an .svg or an image): relative paths look in the settings folder, then next to
the defaults; `logo.svg` falls back to the built-in art. */
pub fn art(name: &str) -> String {
    let name = name.trim();
    if name.starts_with('<') {
        return name.to_string();
    }
    let p = std::path::PathBuf::from(crate::script::expand(name));
    let mut tries = vec![];
    if p.is_absolute() {
        tries.push(p);
    } else {
        tries.push(crate::settings::dir().join(&p));
        if let Some(d) = crate::settings::live_defaults_path().and_then(|d| d.parent().map(|d| d.to_path_buf())) {
            tries.push(d.join(&p));
        }
    }
    tries.iter().find_map(|p| file_art(p)).unwrap_or_else(|| DEFAULT_ART.to_string())
}

/** An .svg as is; a PNG, JPEG, GIF, or WebP wrapped in an SVG at its own size. */
fn file_art(p: &std::path::Path) -> Option<String> {
    use base64::Engine as _;
    let ext = p.extension()?.to_str()?.to_ascii_lowercase();
    if ext == "svg" {
        return std::fs::read_to_string(p).ok();
    }
    let mime = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => return None,
    };
    let bytes = std::fs::read(p).ok()?;
    let size = imagesize::blob_size(&bytes).ok()?;
    let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Some(format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 {w} {h}"><image width="{w}" height="{h}" xlink:href="data:{mime};base64,{data}"/></svg>"#,
        w = size.width,
        h = size.height
    ))
}

fn color(v: Option<&Value>, theme: &Map<String, Value>) -> Option<String> {
    let s = v?.as_str()?;
    match s.strip_prefix('@') {
        Some(token) => theme.get(token)?.as_str().map(str::to_string),
        None => Some(s.to_string()),
    }
}

fn rgba(hex: &str) -> Option<tiny_skia::Color> {
    let h = hex.trim().strip_prefix('#')?;
    let h = match h.len() {
        3 | 4 => h.chars().flat_map(|c| [c, c]).collect::<String>(),
        _ => h.to_string(),
    };
    let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
    let a = if h.len() == 8 { byte(6)? } else { 255 };
    Some(tiny_skia::Color::from_rgba8(byte(0)?, byte(2)?, byte(4)?, a))
}

fn rounded(x: f32, y: f32, side: f32, r: f32) -> Option<tiny_skia::Path> {
    let k = 0.5523 * r;
    let (x1, y1) = (x + side, y + side);
    let mut pb = tiny_skia::PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x1 - r, y);
    pb.cubic_to(x1 - r + k, y, x1, y + r - k, x1, y + r);
    pb.line_to(x1, y1 - r);
    pb.cubic_to(x1, y1 - r + k, x1 - r + k, y1, x1 - r, y1);
    pb.line_to(x + r, y1);
    pb.cubic_to(x + r - k, y1, x, y1 - r + k, x, y1 - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish()
}

/** Renders the logo as a `px`-square PNG. Keys: `art`, `ink` (replaces `currentColor`), `tile` (a color,
or null for none), `corner` and `padding` (fractions of the tile's side), `margin` (fraction of the image). */
pub fn png(spec: &Map<String, Value>, theme: &Map<String, Value>, px: u32) -> Option<Vec<u8>> {
    let f = |k: &str, d: f32| spec.get(k).and_then(Value::as_f64).map(|v| v as f32).unwrap_or(d);
    let mut svg = art(spec.get("art").and_then(Value::as_str).unwrap_or("logo.svg"));
    if let Some(ink) = color(spec.get("ink"), theme) {
        svg = svg.replace("currentColor", &ink);
    }
    let tree = usvg::Tree::from_str(&svg, &usvg::Options::default()).ok()?;
    let mut pm = tiny_skia::Pixmap::new(px, px)?;
    let size = px as f32;
    let margin = f("margin", 0.) * size;
    let side = size - 2. * margin;
    let tile = color(spec.get("tile"), theme).and_then(|c| rgba(&c));
    let mut inner = (margin, side);
    let mut clip = None;
    if let Some(bg) = tile {
        if let Some(path) = rounded(margin, margin, side, f("corner", 0.225) * side) {
            let mut paint = tiny_skia::Paint::default();
            paint.set_color(bg);
            paint.anti_alias = true;
            pm.fill_path(&path, &paint, tiny_skia::FillRule::Winding, tiny_skia::Transform::identity(), None);
            let mut mask = tiny_skia::Mask::new(px, px)?;
            mask.fill_path(&path, tiny_skia::FillRule::Winding, true, tiny_skia::Transform::identity());
            clip = Some(mask);
        }
        // Negative padding zooms the art past the tile's edge; the tile clips it.
        let pad = f("padding", 0.06) * side;
        inner = (margin + pad, side - 2. * pad);
    }
    let ts = tree.size();
    let scale = (inner.1 / ts.width()).min(inner.1 / ts.height());
    let (dx, dy) = (inner.0 + (inner.1 - ts.width() * scale) / 2., inner.0 + (inner.1 - ts.height() * scale) / 2.);
    let mut art = tiny_skia::Pixmap::new(px, px)?;
    resvg::render(&tree, tiny_skia::Transform::from_row(scale, 0., 0., scale, dx, dy), &mut art.as_mut());
    pm.draw_pixmap(0, 0, art.as_ref(), &tiny_skia::PixmapPaint::default(), tiny_skia::Transform::identity(), clip.as_ref());
    pm.encode_png().ok()
}

thread_local! {
    static IMAGES: std::cell::RefCell<HashMap<String, Option<Arc<gpui_kit::Image>>>> = std::cell::RefCell::new(HashMap::new());
}

/** The logo as a GPUI image, cached by everything that shapes it (art files are re-read when settings reload). */
pub fn image(spec: &Map<String, Value>, theme: &Map<String, Value>, px: u32) -> Option<Arc<gpui_kit::Image>> {
    let key = format!("{px}:{}", Value::Object(spec.clone()));
    IMAGES.with(|m| {
        m.borrow_mut()
            .entry(key)
            .or_insert_with(|| png(spec, theme, px).map(|b| Arc::new(gpui_kit::Image::from_bytes(gpui_kit::ImageFormat::Png, b))))
            .clone()
    })
}

pub fn clear_cache() {
    IMAGES.with(|m| m.borrow_mut().clear());
}

/** `logo.dock` laid over the logo: what the Dock and the bundle icon show, or None when `dock` is false. */
pub fn dock_spec(logo: &Value) -> Option<Map<String, Value>> {
    match logo.get("dock") {
        Some(Value::Bool(false)) => None,
        Some(Value::Object(d)) => Some(spec(logo, d)),
        _ => Some(spec(logo, &Map::new())),
    }
}

/** Writes a macOS `.iconset` folder (every size iconutil wants) for the bundle icon. */
pub fn write_iconset(dir: &std::path::Path) -> anyhow::Result<()> {
    let s = crate::settings::load()?;
    let spec = dock_spec(&s.logo).ok_or_else(|| anyhow::anyhow!("logo.dock is false"))?;
    std::fs::create_dir_all(dir)?;
    for base in [16u32, 32, 128, 256, 512] {
        for (scale, suffix) in [(1, ""), (2, "@2x")] {
            let bytes = png(&spec, &s.theme, base * scale).ok_or_else(|| anyhow::anyhow!("rendering the logo"))?;
            std::fs::write(dir.join(format!("icon_{base}x{base}{suffix}.png")), bytes)?;
        }
    }
    Ok(())
}
