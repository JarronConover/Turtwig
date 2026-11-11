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

actions!(terminal, [SendTab, SendBackTab]);

/** Tab and shift-Tab go to the shell: the window root binds them to moving focus, and a binding in the
terminal's own context comes first. */
pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("tab", SendTab, Some("Terminal")), KeyBinding::new("shift-tab", SendBackTab, Some("Terminal"))]);
}

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
    cursor_thickness: f32,
    /** Cursor blink half-period; 0 holds it on. */
    blink_ms: u64,
    bold_weight: FontWeight,
    bold_is_bright: bool,
    dim_alpha: f32,
    links: bool,
    /** The modifier a click needs to open a link: "cmd", "ctrl", "alt", "shift", or "none". */
    link_modifier: String,
    link_color: Option<Hsla>,
    cmd_passthrough: Passthrough,
}

/** `terminal.cmd_passthrough`: which cmd combos go to the shell (as meta, ESC + key) instead of the app. */
#[derive(Clone, Debug, PartialEq)]
enum Passthrough {
    None,
    All,
    Only(Vec<String>),
}

impl Passthrough {
    fn of(v: Option<&Value>) -> Self {
        match v {
            Some(Value::Bool(true)) => Self::All,
            Some(Value::Array(list)) => Self::Only(list.iter().filter_map(Value::as_str).map(normalize_combo).collect()),
            _ => Self::None,
        }
    }

    fn takes(&self, combo: &str) -> bool {
        match self {
            Self::None => false,
            Self::All => true,
            Self::Only(list) => list.iter().any(|c| c == combo),
        }
    }
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
        let links = x.get("links").cloned().unwrap_or(Value::Null);
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
            cursor_thickness: cnum("thickness", 2.).max(0.5),
            blink_ms: cursor.get("blink_ms").and_then(Value::as_u64).unwrap_or(0),
            bold_weight: FontWeight(x.get("bold_weight").and_then(Value::as_f64).unwrap_or(700.) as f32),
            bold_is_bright: x.get("bold_is_bright").and_then(Value::as_bool).unwrap_or(false),
            dim_alpha: x.get("dim_alpha").and_then(Value::as_f64).unwrap_or(0.66) as f32,
            links: links.get("enabled").and_then(Value::as_bool).unwrap_or(true),
            link_modifier: text(links.get("modifier"), "cmd"),
            link_color: links.get("color").and_then(Value::as_str).map(parse_color),
            cmd_passthrough: Passthrough::of(x.get("cmd_passthrough")),
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
    /** Scrolled back into tmux's history (copy mode) by the wheel. */
    tmux_scrolled: Cell<bool>,
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
    blink_task: Option<Task<()>>,
    blink_epoch: std::time::Instant,
    /** Whether it had the typing when last drawn (inspect.json's `focused`). */
    focused_last: Cell<bool>,
    /** The `terminal` settings, and a node's own font/padding props drawn over them (`set_overrides`). */
    base: TerminalSettings,
    overrides: Map<String, Value>,
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
            tmux_scrolled: Cell::new(false),
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
            blink_task: None,
            blink_epoch: std::time::Instant::now(),
            focused_last: Cell::new(false),
            base: ts.clone(),
            overrides: Map::new(),
        }
    }

    /** Picks up edited `terminal` settings (keys, colors, palette, cursor, wheel, selection) without reopening. */
    pub fn apply_settings(&mut self, ts: &TerminalSettings) {
        self.set_keys(&ts.keys);
        self.fg = parse_color(&ts.foreground);
        self.bg = parse_color(&ts.background);
        self.look = Look::from(ts);
        self.base = ts.clone();
        self.apply_metrics();
    }

    /** A `terminal` node's `font_size`, `font_family`, `line_height` and `padding` props, over the settings. */
    pub fn set_overrides(&mut self, props: &Map<String, Value>, theme: &Map<String, Value>, cx: &mut Context<Self>) {
        let mut o: Map<String, Value> = ["font_size", "font_family", "line_height", "padding"].iter().filter_map(|k| props.get(*k).map(|v| (k.to_string(), v.clone()))).collect();
        if let Some(Value::String(f)) = o.get("font_family") {
            if let Some(t) = f.strip_prefix('@').and_then(|t| theme.get(t)).cloned() {
                o.insert("font_family".into(), t);
            }
        }
        if o != self.overrides {
            self.overrides = o;
            self.apply_metrics();
            cx.notify();
        }
    }

    fn apply_metrics(&mut self) {
        let (b, o) = (&self.base, &self.overrides);
        let num = |k: &str| o.get(k).and_then(Value::as_f64).map(|x| x as f32);
        self.font = font(o.get("font_family").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| b.font_family.clone()));
        self.font_size = px(num("font_size").unwrap_or(b.font_size));
        self.line_height = num("line_height").unwrap_or(b.line_height);
        self.padding = match o.get("padding") {
            Some(Value::Number(n)) => [n.as_f64().unwrap_or(0.) as f32; 2],
            Some(Value::Array(a)) if a.len() == 2 => [a[0].as_f64().unwrap_or(0.) as f32, a[1].as_f64().unwrap_or(0.) as f32],
            _ => b.padding,
        };
    }

    /** Whether the cursor shows this frame (`terminal.cursor.blink_ms`; it blinks only while focused). */
    fn blink(&mut self, focused: bool, cx: &mut Context<Self>) -> bool {
        let ms = self.look.blink_ms;
        if ms == 0 || !focused {
            return true;
        }
        let t = self.blink_epoch.elapsed().as_millis() as u64;
        if self.blink_task.is_none() {
            let wait = std::time::Duration::from_millis(ms - t % ms);
            self.blink_task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(wait).await;
                this.update(cx, |t, cx| {
                    t.blink_task = None;
                    cx.notify();
                })
                .ok();
            }));
        }
        (t / ms) % 2 == 0
    }

    /** Pastes text into the shell, bracketed when the program asked for it. */
    pub fn paste(&mut self, text: &str) {
        self.to_bottom();
        if self.term.mode().contains(TermMode::BRACKETED_PASTE) {
            self.write(format!("\x1b[200~{text}\x1b[201~").as_bytes());
        } else {
            self.write(text.as_bytes());
        }
    }

    /** Selects the whole history (`all`), the cells from..to (grid lines: 0 is the screen's top, negative
    is history), or clears the selection. */
    pub fn select_text(&mut self, all: bool, from: Option<(i32, usize)>, to: Option<(i32, usize)>) {
        let clamp = |(l, c): (i32, usize)| GridPoint::new(Line(l.clamp(self.term.topmost_line().0, self.term.bottommost_line().0)), Column(c.min(self.term.columns().saturating_sub(1))));
        let span = if all {
            Some((GridPoint::new(self.term.topmost_line(), Column(0)), GridPoint::new(self.term.bottommost_line(), self.term.last_column())))
        } else {
            from.map(|f| (clamp(f), clamp(to.unwrap_or(f))))
        };
        self.term.selection = span.map(|(a, b)| {
            let mut sel = Selection::new(SelectionType::Simple, a, Side::Left);
            sel.update(b, Side::Right);
            sel
        });
    }

    /** The screen as text, newest `n` rows (what an agent reads in inspect.json). */
    pub fn screen_lines(&self, n: usize) -> Vec<String> {
        let rows = self.rows();
        let skip = rows.len().saturating_sub(n);
        rows.into_iter().skip(skip).map(|(t, _)| t.trim_end().to_string()).collect()
    }

    /** Types text into the shell as if pasted (bracketed when the program asked for it). */
    pub fn type_text(&mut self, text: &str) {
        if text.contains('\n') {
            self.paste(text);
        } else {
            self.to_bottom();
            self.write(text.as_bytes());
        }
    }

    /** Presses a key combo ("enter", "ctrl-c", "alt-left", "up"): `terminal.keys` first, then the usual bytes. */
    pub fn press(&mut self, combo: &str) -> bool {
        let combo = normalize_combo(combo);
        match self.keys.get(&combo).cloned() {
            Some(Value::String(text)) => {
                self.write(text.as_bytes());
                return true;
            }
            Some(Value::Object(o)) if key_action(&o).is_some() => {
                queue_action(key_action(&o).unwrap_or(Value::Null));
                return true;
            }
            _ => {}
        }
        let parts: Vec<&str> = combo.split('-').collect();
        let Some((key, mods)) = parts.split_last() else { return false };
        let m = Mods { ctrl: mods.contains(&"ctrl"), alt: mods.contains(&"alt"), shift: mods.contains(&"shift") };
        let ch = (key.chars().count() == 1).then(|| if m.shift { key.to_uppercase() } else { key.to_string() });
        match key_bytes(key, m, ch.as_deref()) {
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
        let (origin, cw, lh) = self.geom.get();
        json!({
            // Where the grid is drawn, in window points: cell (col, line) is at x + col*cell_w, y + line*cell_h.
            "grid": {"x": f32::from(origin.x), "y": f32::from(origin.y), "cell_w": f32::from(cw), "cell_h": f32::from(lh)},
            "host": self.host, "session": self.opened.clone().or(self.session.clone()), "term_id": self.term_id,
            "cwd": self.cwd, "command": self.command, "cols": self.size.0, "rows": self.size.1,
            "cursor": {"line": cursor.line.0, "column": cursor.column.0},
            "focused": self.focused_last.get(),
            "scrolled_back": self.term.grid().display_offset(),
            "mouse_reporting": self.term.mode().intersects(TermMode::MOUSE_MODE),
            "selection": self.selection_text(),
            "links": self.screen_links(),
            "font": {"family": self.font.family.to_string(), "size": f32::from(self.font_size), "line_height": self.line_height, "padding": self.padding},
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
        if self.look.links && modifier_held(&e.modifiers, &self.look.link_modifier) {
            let (at, _) = self.cell_at(e.position);
            if let Some(link) = self.term.grid()[at].hyperlink() {
                cx.open_url(link.uri());
                return;
            }
        }
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
            "local" | "tmux" => false,
            _ => mode.intersects(TermMode::MOUSE_MODE),
        };
        // A tmux shell draws on the alternate screen, so its history is tmux's: scroll that (copy mode)
        // without turning on tmux's mouse mode, which would take drags away from selection here.
        let tmux_session = self.opened.clone().filter(|s| s.starts_with("sh-") || s.starts_with("chat-"));
        let via_tmux = !to_app && tmux_session.is_some() && (self.look.wheel == "tmux" || mode.contains(TermMode::ALT_SCREEN));
        if via_tmux {
            if let Some(session) = tmux_session {
                if lines > 0 || self.tmux_scrolled.get() {
                    self.tmux_scrolled.set(true);
                    self.remote.send(Call::TermScroll { session, lines });
                }
            }
        } else if to_app {
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

    pub fn is_focused(&self, window: &Window) -> bool {
        self.focus.is_focused(window)
    }

    /** Lets go of the terminal but leaves its shell running (its tmux session), to attach to again later. */
    pub fn detach(&self) {
        if let Some(id) = self.term_id.filter(|id| *id != 0) {
            self.remote.send(Call::TermClose { term_id: id });
        }
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
        // Typing while scrolled back in tmux: leave copy mode first, so the keys reach the shell.
        if self.tmux_scrolled.replace(false) {
            if let Some(session) = self.opened.clone() {
                self.remote.send(Call::TermScroll { session, lines: 0 });
            }
        }
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

    fn send_key(&mut self, bytes: &[u8]) {
        self.blink_epoch = std::time::Instant::now();
        self.term.selection = None;
        self.to_bottom();
        self.write(bytes);
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
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
        self.blink_epoch = std::time::Instant::now();
        if self.look.select && combo == self.look.copy_key {
            if let Some(text) = self.selection_text() {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                cx.stop_propagation();
                return;
            }
        }
        match self.keys.get(&combo).cloned() {
            Some(Value::String(text)) => {
                self.write(text.as_bytes());
                cx.stop_propagation();
                return;
            }
            Some(Value::Object(o)) if o.get("paste").and_then(Value::as_bool) == Some(true) => {
                if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
                    self.paste(&text);
                }
                cx.stop_propagation();
                return;
            }
            Some(Value::Object(o)) if key_action(&o).is_some() => {
                queue_action(key_action(&o).unwrap_or(Value::Null));
                window.refresh();
                cx.stop_propagation();
                return;
            }
            Some(Value::Null) => return,
            _ => {}
        }
        // Cmd combos belong to the app unless `terminal.cmd_passthrough` sends them on, as meta.
        let passthrough = m.platform && self.look.cmd_passthrough.takes(&combo);
        if m.platform && !passthrough {
            return;
        }
        let mods = Mods { ctrl: m.control, alt: m.alt || passthrough, shift: m.shift };
        let Some(bytes) = key_bytes(k.key.as_str(), mods, k.key_char.as_deref().or((passthrough && k.key.chars().count() == 1).then_some(k.key.as_str()))) else { return };
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

    /** A cell's text color: bold base colors turn bright with `bold_is_bright`, dim fades, hidden vanishes. */
    fn cell_fg(&self, c: TermColor, look: &CellLook) -> Hsla {
        let c = match c {
            TermColor::Named(n) if self.look.bold_is_bright && look.bold && (n as usize) < 8 => TermColor::Indexed(n as u8 + 8),
            TermColor::Indexed(i) if self.look.bold_is_bright && look.bold && i < 8 => TermColor::Indexed(i + 8),
            other => other,
        };
        let mut fg = self.color(c);
        if look.dim {
            fg.a *= self.look.dim_alpha;
        }
        if look.hidden {
            fg.a = 0.;
        }
        fg
    }

    /** Each screen row as text plus style runs; a run changes wherever the cell style or link does. A wide
    character's spacer cell becomes a space, so text columns stay lined up with the grid (and the cursor). */
    fn rows(&self) -> Vec<(String, Vec<Run>)> {
        let content = self.term.renderable_content();
        let rows = self.term.screen_lines();
        let cols = self.term.columns();
        let mut lines: Vec<(String, Vec<Run>)> = vec![(String::new(), vec![]); rows];
        let offset = content.display_offset as i32;
        for indexed in content.display_iter {
            let row = (indexed.point.line.0 + offset) as usize;
            if row >= rows || indexed.point.column.0 >= cols {
                continue;
            }
            let cell = indexed.cell;
            let mut look = cell_look(cell.flags);
            let link = if self.look.links { cell.hyperlink().map(|h| h.uri().to_string()) } else { None };
            if link.is_some() && look.underline == Underline::None {
                look.underline = Underline::Single;
            }
            let (mut fg, mut bg) = (self.cell_fg(cell.fg, &look), self.color(cell.bg));
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if let (Some(_), Some(c)) = (&link, self.look.link_color) {
                fg = c;
            }
            let bg = (bg != self.bg).then_some(bg);
            let (text, runs) = &mut lines[row];
            let c = if cell.c == '\0' || cell.flags.contains(Flags::WIDE_CHAR_SPACER) { ' ' } else { cell.c };
            let len = c.len_utf8();
            text.push(c);
            match runs.last_mut() {
                Some(last) if last.fg == fg && last.bg == bg && last.look == look && last.link == link => {
                    last.len += len;
                    last.cells += 1;
                }
                _ => runs.push(Run { len, cells: 1, fg, bg, look, link }),
            }
        }
        lines
    }

    /** OSC-8 links on screen: where each starts, its text and target. */
    fn screen_links(&self) -> Vec<Value> {
        let mut out = vec![];
        for (line, (text, runs)) in self.rows().iter().enumerate() {
            let (mut col, mut byte) = (0, 0);
            let mut last: Option<(String, usize, usize, usize)> = None;
            for r in runs {
                match (&r.link, &mut last) {
                    (Some(uri), Some((u, _, _, end))) if u == uri => *end = byte + r.len,
                    (link, _) => {
                        if let Some((uri, column, from, to)) = last.take() {
                            out.push(json!({"line": line, "column": column, "uri": uri, "text": text.get(from..to).unwrap_or("").trim_end()}));
                        }
                        last = link.as_ref().map(|u| (u.clone(), col, byte, byte + r.len));
                    }
                }
                col += r.cells;
                byte += r.len;
            }
            if let Some((uri, column, from, to)) = last {
                out.push(json!({"line": line, "column": column, "uri": uri, "text": text.get(from..to).unwrap_or("").trim_end()}));
            }
        }
        out
    }
}

/** How a cell's attributes draw. */
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct CellLook {
    bold: bool,
    italic: bool,
    underline: Underline,
    strike: bool,
    dim: bool,
    hidden: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Underline {
    #[default]
    None,
    Single,
    Double,
    Curl,
}

fn cell_look(f: Flags) -> CellLook {
    let underline = if f.contains(Flags::UNDERCURL) {
        Underline::Curl
    } else if f.contains(Flags::DOUBLE_UNDERLINE) {
        Underline::Double
    } else if f.intersects(Flags::UNDERLINE | Flags::DOTTED_UNDERLINE | Flags::DASHED_UNDERLINE) {
        Underline::Single
    } else {
        Underline::None
    };
    CellLook {
        bold: f.contains(Flags::BOLD),
        italic: f.contains(Flags::ITALIC),
        underline,
        strike: f.contains(Flags::STRIKEOUT),
        dim: f.contains(Flags::DIM),
        hidden: f.contains(Flags::HIDDEN),
    }
}

/** A run of same-looking cells in a row: its bytes, cells, colors, attributes and link. */
#[derive(Clone, PartialEq)]
struct Run {
    len: usize,
    cells: usize,
    fg: Hsla,
    bg: Option<Hsla>,
    look: CellLook,
    link: Option<String>,
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
        self.focused_last.set(focused);
        let blink_on = self.blink(focused, cx);
        let entity = cx.entity();
        div()
            .id("terminal")
            .track_focus(&self.focus)
            .key_context("Terminal")
            .on_action(cx.listener(|t, _: &SendTab, _, _| t.send_key(b"\t")))
            .on_action(cx.listener(|t, _: &SendBackTab, _, _| t.send_key(b"\x1b[Z")))
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
                            for r in runs.iter() {
                                let n = r.cells as f32;
                                if let Some(bg) = r.bg {
                                    window.paint_quad(fill(Bounds::new(point(x, y), size(cell_width * n, line_height)), bg));
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
                                .filter_map(|r| {
                                    if left == 0 {
                                        return None;
                                    }
                                    let l = r.len.min(left);
                                    left -= l;
                                    Some(text_run(r, l, &font, look.bold_weight))
                                })
                                .collect();
                            let line = window.text_system().shape_line(trimmed.to_string().into(), font_size, &text_runs, Some(cell_width));
                            let _ = line.paint(point(origin.x, y), line_height, TextAlign::Left, None, window, cx);
                        }
                        let cx_ = origin.x + cell_width * cursor.column.0 as f32;
                        let cy = origin.y + line_height * (cursor.line.0 + offset).max(0) as f32;
                        let mut c = look.cursor_color.unwrap_or(fg);
                        c.a = if !blink_on { 0. } else if focused { look.cursor_focused } else { look.cursor_unfocused };
                        use alacritty_terminal::vte::ansi::CursorShape as Shape;
                        let shape = match look.cursor_shape.as_str() {
                            "bar" => Shape::Beam,
                            "underline" => Shape::Underline,
                            "hollow" => Shape::HollowBlock,
                            "app" => app_shape,
                            _ => Shape::Block,
                        };
                        let thin = px(look.cursor_thickness);
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

fn text_run(r: &Run, len: usize, font: &Font, bold_weight: FontWeight) -> TextRun {
    let mut f = font.clone();
    if r.look.bold {
        f.weight = bold_weight;
    }
    if r.look.italic {
        f.style = FontStyle::Italic;
    }
    let underline = match r.look.underline {
        Underline::None => None,
        u => Some(UnderlineStyle { thickness: px(if u == Underline::Double { 2. } else { 1. }), color: Some(r.fg), wavy: u == Underline::Curl }),
    };
    let strikethrough = r.look.strike.then(|| StrikethroughStyle { thickness: px(1.), color: Some(r.fg) });
    TextRun { len, font: f, color: r.fg, background_color: None, underline, strikethrough }
}

#[derive(Clone, Copy, Default)]
struct Mods {
    ctrl: bool,
    alt: bool,
    shift: bool,
}

impl Mods {
    /** xterm's modifier parameter: 1 + shift + 2 alt + 4 ctrl (1 means none). */
    fn param(self) -> u8 {
        1 + self.shift as u8 + 2 * self.alt as u8 + 4 * self.ctrl as u8
    }
}

/** The bytes a key sends to the shell, as xterm does; None for keys that send nothing. */
fn key_bytes(key: &str, m: Mods, key_char: Option<&str>) -> Option<Vec<u8>> {
    let p = m.param();
    let esc = |b: &[u8]| if m.alt { [b"\x1b".as_slice(), b].concat() } else { b.to_vec() };
    // Cursor keys: CSI letter, or CSI 1;m letter with modifiers.
    let csi_letter = |c: char| if p == 1 { format!("\x1b[{c}").into_bytes() } else { format!("\x1b[1;{p}{c}").into_bytes() };
    let tilde = |n: u8| if p == 1 { format!("\x1b[{n}~").into_bytes() } else { format!("\x1b[{n};{p}~").into_bytes() };
    Some(match key {
        "enter" => esc(b"\r"),
        "backspace" => esc(b"\x7f"),
        "tab" if m.shift => b"\x1b[Z".to_vec(),
        "tab" => esc(b"\t"),
        "escape" => esc(b"\x1b"),
        "up" => csi_letter('A'),
        "down" => csi_letter('B'),
        "right" => csi_letter('C'),
        "left" => csi_letter('D'),
        "home" => csi_letter('H'),
        "end" => csi_letter('F'),
        "insert" => tilde(2),
        "delete" => tilde(3),
        "pageup" => tilde(5),
        "pagedown" => tilde(6),
        "f1" | "f2" | "f3" | "f4" => {
            let c = (b'P' + key[1..].parse::<u8>().ok()? - 1) as char;
            if p == 1 { format!("\x1bO{c}").into_bytes() } else { format!("\x1b[1;{p}{c}").into_bytes() }
        }
        k if k.len() >= 2 && k.starts_with('f') && k[1..].parse::<u8>().is_ok_and(|n| (5..=12).contains(&n)) => {
            let n = k[1..].parse::<u8>().ok()?;
            tilde([15, 17, 18, 19, 20, 21, 23, 24][(n - 5) as usize])
        }
        "space" if m.ctrl => esc(&[0]),
        "space" => esc(b" "),
        k if m.ctrl && k.chars().count() == 1 => esc(&[ctrl_byte(k.as_bytes()[0])?]),
        _ => match key_char {
            Some(s) => esc(s.as_bytes()),
            None => return None,
        },
    })
}

/** The control code ctrl + a key sends: letters, and @ [ \\ ] ^ _ / (and their digit-row twins). */
fn ctrl_byte(c: u8) -> Option<u8> {
    match c.to_ascii_lowercase() {
        c @ b'a'..=b'z' => Some(c - b'a' + 1),
        b'@' | b'2' => Some(0),
        b'[' | b'3' => Some(0x1b),
        b'\\' | b'4' => Some(0x1c),
        b']' | b'5' => Some(0x1d),
        b'^' | b'6' => Some(0x1e),
        b'_' | b'/' | b'-' | b'7' => Some(0x1f),
        b'?' | b'8' => Some(0x7f),
        _ => None,
    }
}

/** A `terminal.keys` value that runs an app action: `{"do": ...}` itself, or `{"action": ...}`. */
fn key_action(o: &Map<String, Value>) -> Option<Value> {
    o.get("action").cloned().or_else(|| o.contains_key("do").then(|| Value::Object(o.clone())))
}

thread_local! {
    static ACTIONS_QUEUED: std::cell::RefCell<Vec<Value>> = const { std::cell::RefCell::new(vec![]) };
}

fn queue_action(a: Value) {
    ACTIONS_QUEUED.with(|q| q.borrow_mut().push(a));
}

/** App actions terminal keys asked for since the last call; the workspace runs them. */
pub(crate) fn take_actions() -> Vec<Value> {
    ACTIONS_QUEUED.with(|q| std::mem::take(&mut *q.borrow_mut()))
}

fn modifier_held(m: &Modifiers, name: &str) -> bool {
    match name {
        "none" => true,
        "ctrl" => m.control,
        "alt" => m.alt,
        "shift" => m.shift,
        _ => m.platform,
    }
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
        use super::{key_bytes, Mods};
        let none = Mods::default();
        let ctrl = Mods { ctrl: true, ..none };
        let alt = Mods { alt: true, ..none };
        let shift = Mods { shift: true, ..none };
        assert_eq!(key_bytes("c", ctrl, Some("c")), Some(vec![3]));
        assert_eq!(key_bytes("up", none, None), Some(b"\x1b[A".to_vec()));
        assert_eq!(key_bytes("b", alt, Some("b")), Some(b"\x1bb".to_vec()));
        assert_eq!(key_bytes("enter", none, None), Some(b"\r".to_vec()));
        assert_eq!(key_bytes("left", ctrl, None), Some(b"\x1b[1;5D".to_vec()));
        assert_eq!(key_bytes("end", shift, None), Some(b"\x1b[1;2F".to_vec()));
        assert_eq!(key_bytes("pageup", alt, None), Some(b"\x1b[5;3~".to_vec()));
        assert_eq!(key_bytes("tab", shift, None), Some(b"\x1b[Z".to_vec()));
        assert_eq!(key_bytes("f1", none, None), Some(b"\x1bOP".to_vec()));
        assert_eq!(key_bytes("f4", shift, None), Some(b"\x1b[1;2S".to_vec()));
        assert_eq!(key_bytes("f5", none, None), Some(b"\x1b[15~".to_vec()));
        assert_eq!(key_bytes("f12", ctrl, None), Some(b"\x1b[24;5~".to_vec()));
        assert_eq!(key_bytes("f13", none, None), None);
        assert_eq!(key_bytes("space", ctrl, None), Some(vec![0]));
        assert_eq!(key_bytes("[", ctrl, Some("[")), Some(vec![0x1b]));
        assert_eq!(key_bytes("\\", ctrl, Some("\\")), Some(vec![0x1c]));
        assert_eq!(key_bytes("/", ctrl, Some("/")), Some(vec![0x1f]));
        assert_eq!(key_bytes("a", Mods { ctrl: true, alt: true, shift: false }, Some("a")), Some(vec![0x1b, 1]));
        assert_eq!(key_bytes(",", ctrl, Some(",")), None);
    }

    #[test]
    fn cell_flags_map_to_looks() {
        use super::{cell_look, Underline};
        use alacritty_terminal::term::cell::Flags;
        let l = cell_look(Flags::BOLD | Flags::ITALIC | Flags::STRIKEOUT);
        assert!(l.bold && l.italic && l.strike && !l.dim && !l.hidden);
        assert_eq!(l.underline, Underline::None);
        assert_eq!(cell_look(Flags::UNDERLINE).underline, Underline::Single);
        assert_eq!(cell_look(Flags::DASHED_UNDERLINE).underline, Underline::Single);
        assert_eq!(cell_look(Flags::DOUBLE_UNDERLINE).underline, Underline::Double);
        assert_eq!(cell_look(Flags::UNDERCURL).underline, Underline::Curl);
        assert!(cell_look(Flags::DIM | Flags::HIDDEN).dim && cell_look(Flags::HIDDEN).hidden);
    }

    #[test]
    fn passthrough_and_key_actions() {
        use super::{key_action, Passthrough};
        use serde_json::json;
        assert!(!Passthrough::of(None).takes("cmd-k"));
        assert!(Passthrough::of(Some(&json!(true))).takes("cmd-k"));
        let only = Passthrough::of(Some(&json!(["Cmd+K"])));
        assert!(only.takes("cmd-k") && !only.takes("cmd-j"));
        let o = |v: serde_json::Value| v.as_object().cloned().unwrap_or_default();
        assert_eq!(key_action(&o(json!({"do": "notify", "text": "x"}))), Some(json!({"do": "notify", "text": "x"})));
        assert_eq!(key_action(&o(json!({"action": "palette.open"}))), Some(json!("palette.open")));
        assert_eq!(key_action(&o(json!({"paste": true}))), None);
    }
}

/** Actions this module handles (named in SETTINGS.md with the rest). */
pub(crate) const ACTIONS: &[(&str, &str)] = &[
    ("terminal.type", "{text?, keys?: [\"enter\", \"ctrl-c\", \"up\", ...], path?, id?}: type into the worktree's active terminal tab (or tab `id`); text first, then each key"),
    ("terminal.scroll", "{lines?, to?: \"bottom\", path?, id?}: scroll a terminal's own history (lines > 0 goes back)"),
    ("terminal.copy", "{path?, id?}: copy a terminal's selection to the clipboard"),
    ("terminal.select_text", "{all? | from: {line, column}, to?: {line, column}, copy?, path?, id?}: select a terminal's text (lines as inspect.json's `cursor`: 0 is the screen's top, negative is history); neither clears it; copy also copies it"),
    ("terminal.paste", "{text?, path?, id?}: paste text (default: the clipboard) into a terminal, bracketed when the program asked for it"),
];

/** The terminal view for a tab: `path` (default: the selected worktree) and tab `id` (default: its active tab). */
fn find(ws: &crate::workspace::Workspace, a: &Map<String, Value>) -> Option<Entity<TerminalView>> {
    let path = a.get("path").and_then(Value::as_str).map(str::to_string).or_else(|| ws.selected.clone())?;
    let id = a.get("id").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| ws.term_active(&path));
    ws.terminals.get(&crate::workspace::Workspace::term_key(&path, &id)).cloned()
}

/** Runs `name` if it's one of this module's actions; false otherwise. */
pub(crate) fn dispatch(ws: &mut crate::workspace::Workspace, name: &str, a: &Map<String, Value>, _window: &mut Window, cx: &mut Context<crate::workspace::Workspace>) -> bool {
    if !matches!(name, "terminal.type" | "terminal.scroll" | "terminal.copy" | "terminal.select_text" | "terminal.paste") {
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
        "terminal.select_text" => {
            let at = |k: &str| {
                let p = a.get(k)?;
                Some((p.get("line").and_then(Value::as_i64).unwrap_or(0) as i32, p.get("column").and_then(Value::as_u64).unwrap_or(0) as usize))
            };
            let all = a.get("all").and_then(Value::as_bool) == Some(true);
            let text = t.update(cx, |t, cx| {
                t.select_text(all, at("from"), at("to"));
                cx.notify();
                t.selection_text()
            });
            if let Some(text) = text.filter(|_| a.get("copy").and_then(Value::as_bool) == Some(true)) {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
        }
        "terminal.paste" => {
            let text = a.get("text").and_then(Value::as_str).map(str::to_string).or_else(|| cx.read_from_clipboard().and_then(|c| c.text()));
            if let Some(text) = text {
                t.update(cx, |t, cx| {
                    t.paste(&text);
                    cx.notify();
                });
            }
        }
        _ => {
            if let Some(text) = t.read(cx).selection_text() {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
        }
    }
    true
}

/** What inspect.json shows about this module's views: every open terminal, by view key (`PATH` for a
worktree's first shell, `PATH#ID` for a tab or tile, `screen:CHAT` for an agent's screen), each with its
`path` and `id`. */
pub(crate) fn inspect(ws: &crate::workspace::Workspace, cx: &App) -> Value {
    let n = ws.settings.num("inspect.terminal_lines", 40.).max(0.) as usize;
    let entry = |k: &str, t: &Entity<TerminalView>| {
        let mut v = t.read(cx).describe(n);
        let (path, id) = match (k.strip_prefix("screen:"), k.split_once('#')) {
            (Some(chat), _) => (chat.to_string(), Value::Null),
            (None, Some((p, id))) => (p.to_string(), Value::String(id.to_string())),
            (None, None) => (k.to_string(), Value::String("1".into())),
        };
        if let Value::Object(o) = &mut v {
            o.insert("path".into(), Value::String(path));
            o.insert("id".into(), id);
        }
        v
    };
    Value::Object(ws.terminals.iter().map(|(k, t)| (k.clone(), entry(k, t))).collect())
}
