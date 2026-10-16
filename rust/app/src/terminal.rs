/*! A native terminal: bytes from the box's PTY go through alacritty_terminal's emulator and the grid is
painted with GPUI text runs. Keystrokes go back as bytes; the PTY follows this view's size. */

use crate::remote::Remote;
use crate::settings::{parse_color, TerminalSettings};
use agent_box_protocol::Call;
use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point as GridPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{Color as TermColor, NamedColor, Processor, StdSyncHandler};
use alacritty_terminal::term::TermMode;
use base64::Engine as _;
use serde_json::{json, Map, Value};
use gpui_kit::*;
use std::cell::Cell;
use std::rc::Rc;

/** The 16 ANSI colors unless `terminal.palette` gives its own. */
const PALETTE: [u32; 16] = [
    0x1e1e1f, 0xe5534b, 0x3fb950, 0xd29922, 0x488efc, 0xbc8cff, 0x39c5cf, 0xd6d6d8, 0x6e7681, 0xff7b72,
    0x56d364, 0xe3b341, 0x79c0ff, 0xd2a8ff, 0x56d4dd, 0xffffff,
];

/** `terminal` settings past fonts and colors: everything here has a default matching the app's original look. */
#[derive(Clone)]
struct Look {
    palette: [Hsla; 16],
    /** "block", "bar", "underline", "hollow", or "app" (whatever the program asks for). */
    cursor_shape: String,
    cursor_focused: f32,
    cursor_unfocused: f32,
    cursor_color: Option<Hsla>,
    /** "auto" (to the program when it asked for the mouse, e.g. tmux with `mouse on`; else this view's own
    scrollback), "local", "app", or "off". */
    wheel: String,
    wheel_lines: f32,
    select: bool,
    selection_color: Hsla,
    copy_key: String,
}

impl Look {
    fn from(ts: &TerminalSettings) -> Look {
        let x = &ts.extra;
        let mut palette = PALETTE.map(|c| Hsla::from(rgb(c)));
        if let Some(Value::Array(colors)) = x.get("palette") {
            for (slot, c) in palette.iter_mut().zip(colors) {
                if let Some(hex) = c.as_str() {
                    *slot = parse_color(hex);
                }
            }
        }
        let cursor = x.get("cursor").cloned().unwrap_or(Value::Null);
        let cnum = |k: &str, d: f64| cursor.get(k).and_then(Value::as_f64).unwrap_or(d) as f32;
        let text = |v: Option<&Value>, d: &str| v.and_then(Value::as_str).unwrap_or(d).to_string();
        Look {
            palette,
            cursor_shape: text(cursor.get("shape"), "block"),
            cursor_focused: cnum("focused_alpha", 0.75),
            cursor_unfocused: cnum("unfocused_alpha", 0.3),
            cursor_color: cursor.get("color").and_then(Value::as_str).map(parse_color),
            wheel: text(x.get("wheel"), "auto"),
            wheel_lines: x.get("wheel_lines").and_then(Value::as_f64).unwrap_or(1.) as f32,
            select: x.get("select").and_then(Value::as_bool).unwrap_or(true),
            selection_color: x.get("selection").and_then(Value::as_str).map(parse_color).unwrap_or_else(|| parse_color("#3a425299")),
            copy_key: normalize_combo(&text(x.get("copy_key"), "cmd-c")),
        }
    }
}

struct Size {
    cols: usize,
    rows: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

pub struct TerminalView {
    term: Term<VoidListener>,
    parser: Processor<StdSyncHandler>,
    remote: Remote,
    /** Which daemon the PTY lives on ("box", "mac"); terminal ids are only unique per daemon. */
    pub host: String,
    pub term_id: Option<u64>,
    /** settings `terminal.keys`, by normalized combo. */
    keys: std::collections::HashMap<String, Value>,
    pub session: Option<String>,
    pub cwd: Option<String>,
    /** Which of the folder's shells (none: the first), and a command to run instead of a shell. */
    name: Option<String>,
    command: Option<String>,
    /** The tmux session the daemon attached, once open. */
    opened: Option<String>,
    size: (u16, u16),
    focus: FocusHandle,
    font: Font,
    font_size: Pixels,
    line_height: f32,
    padding: [f32; 2],
    fg: Hsla,
    bg: Hsla,
    look: Look,
    /** Where the grid was last painted: (origin, cell width, line height), for mapping clicks to cells. */
    geom: Rc<Cell<(Point<Pixels>, Pixels, Pixels)>>,
    selecting: bool,
    /** Fractional wheel lines carried between events. */
    wheel_rest: f32,
}

impl TerminalView {
    pub fn new(remote: Remote, host: String, session: Option<String>, cwd: Option<String>, ts: &TerminalSettings, cx: &mut Context<Self>) -> Self {
        Self::shell(remote, host, session, cwd, None, None, ts, cx)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn shell(remote: Remote, host: String, session: Option<String>, cwd: Option<String>, name: Option<String>, command: Option<String>, ts: &TerminalSettings, cx: &mut Context<Self>) -> Self {
        let start = ts.extra.get("size").cloned().unwrap_or(Value::Null);
        let dim = |k: &str, d: u64| start.get(k).and_then(Value::as_u64).unwrap_or(d).clamp(2, 1000) as u16;
        let (cols, rows) = (dim("cols", 100), dim("rows", 30));
        let size = Size { cols: cols as usize, rows: rows as usize };
        let history = ts.extra.get("scrollback").and_then(Value::as_u64).unwrap_or(10_000) as usize;
        let config = Config { scrolling_history: history, ..Config::default() };
        Self {
            term: Term::new(config, &size, VoidListener),
            parser: Processor::new(),
            remote,
            host,
            term_id: None,
            session,
            cwd,
            name,
            command,
            opened: None,
            size: (cols, rows),
            focus: cx.focus_handle(),
            font: font(ts.font_family.clone()),
            font_size: px(ts.font_size),
            line_height: ts.line_height,
            padding: ts.padding,
            fg: parse_color(&ts.foreground),
            bg: parse_color(&ts.background),
            keys: ts.keys.iter().map(|(k, v)| (normalize_combo(k), v.clone())).collect(),
            look: Look::from(ts),
            geom: Rc::new(Cell::new((Point::default(), px(8.), px(16.)))),
            selecting: false,
            wheel_rest: 0.,
        }
    }

    /** Picks up edited `terminal` settings (keys, colors, palette, cursor, wheel, selection) without reopening. */
    pub fn apply_settings(&mut self, ts: &TerminalSettings) {
        self.set_keys(&ts.keys);
        self.font = font(ts.font_family.clone());
        self.font_size = px(ts.font_size);
        self.line_height = ts.line_height;
        self.padding = ts.padding;
        self.fg = parse_color(&ts.foreground);
        self.bg = parse_color(&ts.background);
        self.look = Look::from(ts);
    }

    /** The screen as text, newest `n` rows (what an agent reads in inspect.json). */
    pub fn screen_lines(&self, n: usize) -> Vec<String> {
        let rows = self.rows();
        let skip = rows.len().saturating_sub(n);
        rows.into_iter().skip(skip).map(|(t, _)| t.trim_end().to_string()).collect()
    }

    /** Types text into the shell as if pasted (bracketed when the program asked for it). */
    pub fn type_text(&mut self, text: &str) {
        self.to_bottom();
        if self.term.mode().contains(TermMode::BRACKETED_PASTE) && text.contains('\n') {
            self.write(format!("\x1b[200~{text}\x1b[201~").as_bytes());
        } else {
            self.write(text.as_bytes());
        }
    }

    /** Presses a key combo ("enter", "ctrl-c", "alt-left", "up"): `terminal.keys` first, then the usual bytes. */
    pub fn press(&mut self, combo: &str) -> bool {
        let combo = normalize_combo(combo);
        if let Some(Value::String(text)) = self.keys.get(&combo) {
            let t = text.clone();
            self.write(t.as_bytes());
            return true;
        }
        let parts: Vec<&str> = combo.split('-').collect();
        let Some((key, mods)) = parts.split_last() else { return false };
        let (ctrl, alt) = (mods.contains(&"ctrl"), mods.contains(&"alt"));
        let ch = (key.chars().count() == 1).then(|| if mods.contains(&"shift") { key.to_uppercase() } else { key.to_string() });
        match key_bytes(key, ctrl, alt, ch.as_deref()) {
            Some(b) => {
                self.to_bottom();
                self.write(&b);
                true
            }
            None => false,
        }
    }

    /** Scrolls this view's own history (`lines` > 0 goes back); "bottom" returns to the live screen. */
    pub fn scroll(&mut self, lines: i32) {
        self.term.scroll_display(Scroll::Delta(lines));
    }

    pub fn to_bottom(&mut self) {
        if self.term.grid().display_offset() > 0 {
            self.term.scroll_display(Scroll::Bottom);
        }
    }

    pub fn selection_text(&self) -> Option<String> {
        self.term.selection_to_string().filter(|s| !s.is_empty())
    }

    /** What inspect.json shows about one terminal. */
    pub fn describe(&self, lines: usize) -> Value {
        let cursor = self.term.renderable_content().cursor.point;
        json!({
            "host": self.host, "session": self.opened.clone().or(self.session.clone()), "term_id": self.term_id,
            "cwd": self.cwd, "command": self.command, "cols": self.size.0, "rows": self.size.1,
            "cursor": {"line": cursor.line.0, "column": cursor.column.0},
            "scrolled_back": self.term.grid().display_offset(),
            "mouse_reporting": self.term.mode().intersects(TermMode::MOUSE_MODE),
            "selection": self.selection_text(),
            "lines": self.screen_lines(lines),
        })
    }

    /** The grid cell under a window position. */
    fn cell_at(&self, pos: Point<Pixels>) -> (GridPoint, Side) {
        let (origin, cw, lh) = self.geom.get();
        let x = f32::from(pos.x - origin.x) / f32::from(cw).max(1.);
        let y = f32::from(pos.y - origin.y) / f32::from(lh).max(1.);
        let cols = self.term.columns();
        let col = (x.max(0.) as usize).min(cols.saturating_sub(1));
        let row = (y.max(0.) as usize).min(self.term.screen_lines().saturating_sub(1));
        let side = if x.fract() < 0.5 { Side::Left } else { Side::Right };
        let line = row as i32 - self.term.grid().display_offset() as i32;
        (GridPoint::new(Line(line), Column(col)), side)
    }

    fn on_mouse_down(&mut self, e: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
        if !self.look.select {
            return;
        }
        let (at, side) = self.cell_at(e.position);
        let ty = match e.click_count {
            2 => SelectionType::Semantic,
            n if n >= 3 => SelectionType::Lines,
            _ => SelectionType::Simple,
        };
        self.term.selection = Some(Selection::new(ty, at, side));
        self.selecting = true;
        cx.notify();
    }

    fn on_mouse_move(&mut self, e: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selecting || e.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let (at, side) = self.cell_at(e.position);
        if let Some(s) = self.term.selection.as_mut() {
            s.update(at, side);
            cx.notify();
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.selecting = false;
        if self.term.selection.as_ref().is_some_and(|s| s.is_empty()) {
            self.term.selection = None;
        }
        cx.notify();
    }

    fn on_wheel(&mut self, e: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let (_, _, lh) = self.geom.get();
        let dy = f32::from(e.delta.pixel_delta(lh).y) / f32::from(lh).max(1.) * self.look.wheel_lines + self.wheel_rest;
        let lines = dy.trunc() as i32;
        self.wheel_rest = dy - lines as f32;
        if lines == 0 {
            return;
        }
        let mode = *self.term.mode();
        let to_app = match self.look.wheel.as_str() {
            "off" => return,
            "app" => true,
            "local" => false,
            _ => mode.intersects(TermMode::MOUSE_MODE),
        };
        if to_app {
            let (at, _) = self.cell_at(e.position);
            let (col, row) = (at.column.0 + 1, (at.line.0 + self.term.grid().display_offset() as i32).max(0) as usize + 1);
            let button = if lines > 0 { 64 } else { 65 };
            for _ in 0..lines.unsigned_abs() {
                if mode.contains(TermMode::SGR_MOUSE) {
                    self.write(format!("\x1b[<{button};{col};{row}M").as_bytes());
                } else {
                    self.write(&[0x1b, b'[', b'M', 32 + button as u8, (32 + col.min(223)) as u8, (32 + row.min(223)) as u8]);
                }
            }
        } else if !mode.contains(TermMode::ALT_SCREEN) {
            self.scroll(lines);
            cx.notify();
        }
        cx.stop_propagation();
    }

    /** Opens the PTY on the box once the first layout has a real size. */
    fn open(&mut self, cx: &mut Context<Self>) {
        if self.term_id.is_some() {
            return;
        }
        self.term_id = Some(0);
        let remote = self.remote.clone();
        let call = Call::TermOpen {
            session: self.session.clone(),
            cwd: self.cwd.clone(),
            cols: self.size.0,
            rows: self.size.1,
            name: self.name.clone(),
            command: self.command.clone(),
        };
        cx.spawn(async move |this, cx| {
            if let Ok(v) = remote.call(call).await {
                let id = v["term_id"].as_u64();
                let session = v["session"].as_str().map(str::to_string);
                this.update(cx, |t, cx| {
                    t.term_id = id;
                    t.opened = session;
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /** Closes the terminal and ends its shell (a closed terminal tab). */
    pub fn kill(&self) {
        if let Some(id) = self.term_id.filter(|id| *id != 0) {
            self.remote.send(Call::TermClose { term_id: id });
        }
        if let Some(s) = self.opened.clone() {
            self.remote.send(Call::TermKill { session: s });
        }
    }

    pub fn output(&mut self, data: &str, cx: &mut Context<Self>) {
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) {
            self.parser.advance(&mut self.term, &bytes);
            cx.notify();
        }
    }

    fn write(&self, bytes: &[u8]) {
        if let Some(id) = self.term_id.filter(|id| *id != 0) {
            // The daemon writes `data` to the PTY as is.
            self.remote.send(Call::TermInput { term_id: id, data: String::from_utf8_lossy(bytes).into_owned() });
        }
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        if (cols, rows) == self.size || cols < 2 || rows < 2 {
            return;
        }
        self.size = (cols, rows);
        self.term.resize(Size { cols: cols as usize, rows: rows as usize });
        if let Some(id) = self.term_id.filter(|id| *id != 0) {
            self.remote.send(Call::TermResize { term_id: id, cols, rows });
        }
    }

    /** Picks up edited `terminal.keys` without reopening the terminal. */
    pub fn set_keys(&mut self, keys: &serde_json::Map<String, Value>) {
        self.keys = keys.iter().map(|(k, v)| (normalize_combo(k), v.clone())).collect();
    }

    fn on_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let k = &event.keystroke;
        let m = &k.modifiers;
        let mut combo: Vec<&str> = vec![];
        for (on, name) in [(m.control, "ctrl"), (m.alt, "alt"), (m.shift, "shift"), (m.platform, "cmd")] {
            if on {
                combo.push(name);
            }
        }
        combo.push(k.key.as_str());
        let combo = combo.join("-");
        if self.look.select && combo == self.look.copy_key {
            if let Some(text) = self.selection_text() {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                cx.stop_propagation();
                return;
            }
        }
        match self.keys.get(&combo) {
            Some(Value::String(text)) => {
                self.write(text.as_bytes());
                cx.stop_propagation();
                return;
            }
            Some(Value::Object(o)) if o.get("paste").and_then(Value::as_bool) == Some(true) => {
                if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
                    if self.term.mode().contains(TermMode::BRACKETED_PASTE) {
                        self.write(format!("\x1b[200~{text}\x1b[201~").as_bytes());
                    } else {
                        self.write(text.as_bytes());
                    }
                }
                cx.stop_propagation();
                return;
            }
            Some(Value::Null) => return,
            _ => {}
        }
        if m.platform {
            return;
        }
        let Some(bytes) = key_bytes(k.key.as_str(), m.control, m.alt, k.key_char.as_deref()) else { return };
        self.term.selection = None;
        self.to_bottom();
        self.write(&bytes);
        cx.stop_propagation();
    }

    fn color(&self, c: TermColor) -> Hsla {
        match c {
            TermColor::Named(NamedColor::Foreground) => self.fg,
            TermColor::Named(NamedColor::Background) => self.bg,
            TermColor::Named(n) if (n as usize) < 16 => self.look.palette[n as usize],
            TermColor::Named(_) => self.fg,
            TermColor::Spec(rgbc) => rgb(((rgbc.r as u32) << 16) | ((rgbc.g as u32) << 8) | rgbc.b as u32).into(),
            TermColor::Indexed(i) if i < 16 => self.look.palette[i as usize],
            TermColor::Indexed(i) if i >= 232 => {
                let v = 8 + (i as u32 - 232) * 10;
                rgb((v << 16) | (v << 8) | v).into()
            }
            TermColor::Indexed(i) => {
                let i = i as u32 - 16;
                let step = |x: u32| if x == 0 { 0 } else { 55 + x * 40 };
                rgb((step(i / 36) << 16) | (step((i / 6) % 6) << 8) | step(i % 6)).into()
            }
        }
    }

    /** One row of the grid as text plus color runs; a run changes wherever the cell style does. */
    /** Each screen row as text plus style runs of (bytes, cells, fg, bg, bold). A wide character's
    spacer cell becomes a space, so text columns stay lined up with the grid (and the cursor). */
    fn rows(&self) -> Vec<(String, Vec<(usize, usize, Hsla, Option<Hsla>, bool)>)> {
        let content = self.term.renderable_content();
        let rows = self.term.screen_lines();
        let cols = self.term.columns();
        let mut lines: Vec<(String, Vec<(usize, usize, Hsla, Option<Hsla>, bool)>)> = vec![(String::new(), vec![]); rows];
        let offset = content.display_offset as i32;
        for indexed in content.display_iter {
            let row = (indexed.point.line.0 + offset) as usize;
            if row >= rows || indexed.point.column.0 >= cols {
                continue;
            }
            let cell = indexed.cell;
            let (mut fg, mut bg) = (self.color(cell.fg), self.color(cell.bg));
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            let bg = (bg != self.bg).then_some(bg);
            let bold = cell.flags.contains(Flags::BOLD);
            let (text, runs) = &mut lines[row];
            let c = if cell.c == '\0' || cell.flags.contains(Flags::WIDE_CHAR_SPACER) { ' ' } else { cell.c };
            let len = c.len_utf8();
            text.push(c);
            match runs.last_mut() {
                Some(last) if last.2 == fg && last.3 == bg && last.4 == bold => {
                    last.0 += len;
                    last.1 += 1;
                }
                _ => runs.push((len, 1, fg, bg, bold)),
            }
        }
        lines
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.rows();
        let content = self.term.renderable_content();
        let (cursor, app_shape) = (content.cursor.point, content.cursor.shape);
        // Selected cells as (screen row, first column, last column) spans.
        let offset = content.display_offset as i32;
        let spans: Vec<(usize, usize, usize)> = content
            .selection
            .map(|sel| {
                let cols = self.term.columns();
                (0..self.term.screen_lines())
                    .filter_map(|r| {
                        let line = Line(r as i32 - offset);
                        let hit: Vec<usize> = (0..cols).filter(|c| sel.contains(GridPoint::new(line, Column(*c)))).collect();
                        Some((r, *hit.first()?, *hit.last()?))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let look = self.look.clone();
        let geom = self.geom.clone();
        let font = self.font.clone();
        let font_size = self.font_size;
        let line_height = font_size * self.line_height;
        let pad = self.padding;
        let cell_width = window
            .text_system()
            .advance(window.text_system().resolve_font(&font), font_size, 'm')
            .map(|s| s.width)
            .unwrap_or(font_size * 0.6);
        let fg = self.fg;
        let focused = self.focus.is_focused(window);
        let entity = cx.entity();
        div()
            .id("terminal")
            .track_focus(&self.focus)
            .key_context("Terminal")
            .on_key_down(cx.listener(Self::on_key))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_scroll_wheel(cx.listener(Self::on_wheel))
            .size_full()
            .child(
                canvas(
                    move |bounds, _, cx| {
                        let cols = ((bounds.size.width - px(pad[0] * 2.0)) / cell_width).floor().max(2.0) as u16;
                        let rows = ((bounds.size.height - px(pad[1] * 2.0)) / line_height).floor().max(2.0) as u16;
                        entity.update(cx, |t, cx| {
                            t.resize(cols, rows);
                            t.open(cx);
                        });
                    },
                    move |bounds, _, window, cx| {
                        let origin = bounds.origin + point(px(pad[0]), px(pad[1]));
                        geom.set((origin, cell_width, line_height));
                        for (r, a, b) in &spans {
                            let y = origin.y + line_height * *r as f32;
                            window.paint_quad(fill(Bounds::new(point(origin.x + cell_width * *a as f32, y), size(cell_width * (b - a + 1) as f32, line_height)), look.selection_color));
                        }
                        for (i, (text, runs)) in rows.iter().enumerate() {
                            let y = origin.y + line_height * i as f32;
                            let mut x = origin.x;
                            for (_, cells, _, bg, _) in runs.iter() {
                                let n = *cells as f32;
                                if let Some(bg) = bg {
                                    window.paint_quad(fill(Bounds::new(point(x, y), size(cell_width * n, line_height)), *bg));
                                }
                                x += cell_width * n;
                            }
                            let trimmed = text.trim_end();
                            if trimmed.is_empty() {
                                continue;
                            }
                            let mut left = trimmed.len();
                            let text_runs: Vec<TextRun> = runs
                                .iter()
                                .filter_map(|(len, _, color, _, bold)| {
                                    if left == 0 {
                                        return None;
                                    }
                                    let l = (*len).min(left);
                                    left -= l;
                                    let mut f = font.clone();
                                    if *bold {
                                        f.weight = FontWeight::BOLD;
                                    }
                                    Some(TextRun { len: l, font: f, color: *color, background_color: None, underline: None, strikethrough: None })
                                })
                                .collect();
                            let line = window.text_system().shape_line(trimmed.to_string().into(), font_size, &text_runs, Some(cell_width));
                            let _ = line.paint(point(origin.x, y), line_height, TextAlign::Left, None, window, cx);
                        }
                        let cx_ = origin.x + cell_width * cursor.column.0 as f32;
                        let cy = origin.y + line_height * (cursor.line.0 + offset).max(0) as f32;
                        let mut c = look.cursor_color.unwrap_or(fg);
                        c.a = if focused { look.cursor_focused } else { look.cursor_unfocused };
                        use alacritty_terminal::vte::ansi::CursorShape as Shape;
                        let shape = match look.cursor_shape.as_str() {
                            "bar" => Shape::Beam,
                            "underline" => Shape::Underline,
                            "hollow" => Shape::HollowBlock,
                            "app" => app_shape,
                            _ => Shape::Block,
                        };
                        let thin = px(2.);
                        match shape {
                            Shape::Hidden => {}
                            Shape::Beam => window.paint_quad(fill(Bounds::new(point(cx_, cy), size(thin, line_height)), c)),
                            Shape::Underline => window.paint_quad(fill(Bounds::new(point(cx_, cy + line_height - thin), size(cell_width, thin)), c)),
                            Shape::HollowBlock => window.paint_quad(outline(Bounds::new(point(cx_, cy), size(cell_width, line_height)), c, BorderStyle::Solid)),
                            Shape::Block => window.paint_quad(fill(Bounds::new(point(cx_, cy), size(cell_width, line_height)), c)),
                        }
                    },
                )
                .size_full(),
            )
    }
}

/** The bytes a key sends to the shell, as xterm does; None for keys that send nothing. */
fn key_bytes(key: &str, ctrl: bool, alt: bool, key_char: Option<&str>) -> Option<Vec<u8>> {
    Some(match key {
        "enter" => b"\r".to_vec(),
        "backspace" => b"\x7f".to_vec(),
        "tab" => b"\t".to_vec(),
        "escape" => b"\x1b".to_vec(),
        "up" => b"\x1b[A".to_vec(),
        "down" => b"\x1b[B".to_vec(),
        "right" => b"\x1b[C".to_vec(),
        "left" => b"\x1b[D".to_vec(),
        "home" => b"\x1b[H".to_vec(),
        "end" => b"\x1b[F".to_vec(),
        "pageup" => b"\x1b[5~".to_vec(),
        "pagedown" => b"\x1b[6~".to_vec(),
        "delete" => b"\x1b[3~".to_vec(),
        "space" if !ctrl => if alt { b"\x1b ".to_vec() } else { b" ".to_vec() },
        k if ctrl && k.len() == 1 => {
            let c = k.as_bytes()[0].to_ascii_lowercase();
            if c.is_ascii_lowercase() { vec![c - b'a' + 1] } else { return None }
        }
        _ => match key_char {
            Some(s) if alt => [b"\x1b".as_slice(), s.as_bytes()].concat(),
            Some(s) => s.as_bytes().to_vec(),
            None => return None,
        },
    })
}

/** "opt+Left" and "Cmd-Backspace" both become "alt-left" / "cmd-backspace", modifiers in a fixed order. */
fn normalize_combo(combo: &str) -> String {
    let parts: Vec<String> = combo.to_ascii_lowercase().split(['-', '+']).filter(|p| !p.is_empty()).map(str::to_string).collect();
    let Some((key, mods)) = parts.split_last() else { return String::new() };
    let has = |names: &[&str]| mods.iter().any(|m| names.contains(&m.as_str()));
    let mut out: Vec<&str> = vec![];
    for (names, name) in [(&["ctrl", "control"][..], "ctrl"), (&["alt", "opt", "option"][..], "alt"), (&["shift"][..], "shift"), (&["cmd", "command", "super", "meta"][..], "cmd")] {
        if has(names) {
            out.push(name);
        }
    }
    out.push(key.as_str());
    out.join("-")
}

#[cfg(test)]
mod tests {
    #[test]
    fn combos_normalize() {
        assert_eq!(super::normalize_combo("Opt+Left"), "alt-left");
        assert_eq!(super::normalize_combo("cmd-shift-Backspace"), "shift-cmd-backspace");
        assert_eq!(super::normalize_combo("shift-enter"), "shift-enter");
    }

    #[test]
    fn keys_turn_into_bytes() {
        assert_eq!(super::key_bytes("c", true, false, Some("c")), Some(vec![3]));
        assert_eq!(super::key_bytes("up", false, false, None), Some(b"\x1b[A".to_vec()));
        assert_eq!(super::key_bytes("b", false, true, Some("b")), Some(b"\x1bb".to_vec()));
        assert_eq!(super::key_bytes("f1", false, false, None), None);
    }
}

/** Actions this module handles (named in SETTINGS.md with the rest). */
pub(crate) const ACTIONS: &[(&str, &str)] = &[
    ("terminal.type", "{text?, keys?: [\"enter\", \"ctrl-c\", \"up\", ...], path?, id?}: type into the worktree's active terminal tab (or tab `id`); text first, then each key"),
    ("terminal.scroll", "{lines?, to?: \"bottom\", path?, id?}: scroll a terminal's own history (lines > 0 goes back)"),
    ("terminal.copy", "{path?, id?}: copy a terminal's selection to the clipboard"),
];

/** The terminal view for a tab: `path` (default: the selected worktree) and tab `id` (default: its active tab). */
fn find(ws: &crate::workspace::Workspace, a: &Map<String, Value>) -> Option<Entity<TerminalView>> {
    let path = a.get("path").and_then(Value::as_str).map(str::to_string).or_else(|| ws.selected.clone())?;
    let id = a.get("id").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| ws.term_active(&path));
    ws.terminals.get(&crate::workspace::Workspace::term_key(&path, &id)).cloned()
}

/** Runs `name` if it's one of this module's actions; false otherwise. */
pub(crate) fn dispatch(ws: &mut crate::workspace::Workspace, name: &str, a: &Map<String, Value>, _window: &mut Window, cx: &mut Context<crate::workspace::Workspace>) -> bool {
    if !matches!(name, "terminal.type" | "terminal.scroll" | "terminal.copy") {
        return false;
    }
    let Some(t) = find(ws, a) else {
        ws.exprs.note(format!("{name}: no open terminal there"));
        return true;
    };
    match name {
        "terminal.type" => {
            let text = a.get("text").and_then(Value::as_str).map(str::to_string);
            let keys: Vec<String> = match a.get("keys") {
                Some(Value::Array(k)) => k.iter().filter_map(Value::as_str).map(str::to_string).collect(),
                Some(Value::String(k)) => vec![k.clone()],
                _ => vec![],
            };
            let bad = t.update(cx, |t, cx| {
                if let Some(text) = &text {
                    t.type_text(text);
                }
                let bad: Vec<String> = keys.iter().filter(|k| !t.press(k)).cloned().collect();
                cx.notify();
                bad
            });
            if !bad.is_empty() {
                ws.exprs.note(format!("terminal.type: unknown keys {bad:?}"));
            }
        }
        "terminal.scroll" => {
            let bottom = a.get("to").and_then(Value::as_str) == Some("bottom");
            let lines = a.get("lines").and_then(Value::as_i64).unwrap_or(0) as i32;
            t.update(cx, |t, cx| {
                if bottom { t.to_bottom() } else { t.scroll(lines) }
                cx.notify();
            });
        }
        _ => {
            if let Some(text) = t.read(cx).selection_text() {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
        }
    }
    true
}

/** What inspect.json shows about this module's views: every open terminal, by view key (`PATH` or `PATH#ID`). */
pub(crate) fn inspect(ws: &crate::workspace::Workspace, cx: &App) -> Value {
    let n = ws.settings.num("inspect.terminal_lines", 40.).max(0.) as usize;
    Value::Object(ws.terminals.iter().map(|(k, t)| (k.clone(), t.read(cx).describe(n))).collect())
}
