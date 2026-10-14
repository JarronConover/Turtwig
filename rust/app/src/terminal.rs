/*! A native terminal: bytes from the box's PTY go through alacritty_terminal's emulator and the grid is
painted with GPUI text runs. Keystrokes go back as bytes; the PTY follows this view's size. */

use crate::remote::Remote;
use crate::settings::{parse_color, Settings};
use agent_box_protocol::Call;
use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{Color as TermColor, NamedColor, Processor, StdSyncHandler};
use base64::Engine as _;
use gpui_kit::*;

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
    pub term_id: Option<u64>,
    pub session: Option<String>,
    pub cwd: Option<String>,
    size: (u16, u16),
    focus: FocusHandle,
    font: Font,
    font_size: Pixels,
    line_height: f32,
    padding: [f32; 2],
    fg: Hsla,
    bg: Hsla,
}

impl TerminalView {
    pub fn new(remote: Remote, session: Option<String>, cwd: Option<String>, settings: &Settings, cx: &mut Context<Self>) -> Self {
        let size = Size { cols: 100, rows: 30 };
        Self {
            term: Term::new(Config::default(), &size, VoidListener),
            parser: Processor::new(),
            remote,
            term_id: None,
            session,
            cwd,
            size: (100, 30),
            focus: cx.focus_handle(),
            font: font(settings.theme.mono_font_family.clone()),
            font_size: px(settings.terminal.font_size),
            line_height: settings.terminal.line_height,
            padding: settings.terminal.padding,
            fg: parse_color("#d6d6d8"),
            bg: parse_color(&settings.theme.card),
        }
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    /** Opens the PTY on the box once the first layout has a real size. */
    fn open(&mut self, cx: &mut Context<Self>) {
        if self.term_id.is_some() {
            return;
        }
        self.term_id = Some(0);
        let remote = self.remote.clone();
        let call = Call::TermOpen { session: self.session.clone(), cwd: self.cwd.clone(), cols: self.size.0, rows: self.size.1 };
        cx.spawn(async move |this, cx| {
            if let Ok(v) = remote.call(call).await {
                let id = v["term_id"].as_u64();
                this.update(cx, |t, cx| {
                    t.term_id = id;
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    pub fn output(&mut self, data: &str, cx: &mut Context<Self>) {
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) {
            self.parser.advance(&mut self.term, &bytes);
            cx.notify();
        }
    }

    fn write(&self, bytes: &[u8]) {
        if let Some(id) = self.term_id.filter(|id| *id != 0) {
            self.remote.send(Call::TermInput { term_id: id, data: base64::engine::general_purpose::STANDARD.encode(bytes) });
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

    fn on_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let k = &event.keystroke;
        let m = &k.modifiers;
        if m.platform {
            return;
        }
        let bytes: Vec<u8> = match k.key.as_str() {
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
            key if m.control && key.len() == 1 => {
                let c = key.as_bytes()[0].to_ascii_lowercase();
                if c.is_ascii_lowercase() { vec![c - b'a' + 1] } else { return }
            }
            _ => match &k.key_char {
                Some(s) if m.alt => [b"\x1b".as_slice(), s.as_bytes()].concat(),
                Some(s) => s.as_bytes().to_vec(),
                None => return,
            },
        };
        self.write(&bytes);
        cx.stop_propagation();
    }

    fn color(&self, c: TermColor) -> Hsla {
        const PALETTE: [u32; 16] = [
            0x1e1e1f, 0xe5534b, 0x3fb950, 0xd29922, 0x488efc, 0xbc8cff, 0x39c5cf, 0xd6d6d8, 0x6e7681, 0xff7b72,
            0x56d364, 0xe3b341, 0x79c0ff, 0xd2a8ff, 0x56d4dd, 0xffffff,
        ];
        match c {
            TermColor::Named(NamedColor::Foreground) => self.fg,
            TermColor::Named(NamedColor::Background) => self.bg,
            TermColor::Named(n) if (n as usize) < 16 => rgb(PALETTE[n as usize]).into(),
            TermColor::Named(_) => self.fg,
            TermColor::Spec(rgbc) => rgb(((rgbc.r as u32) << 16) | ((rgbc.g as u32) << 8) | rgbc.b as u32).into(),
            TermColor::Indexed(i) if i < 16 => rgb(PALETTE[i as usize]).into(),
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
    fn rows(&self) -> Vec<(String, Vec<(usize, Hsla, Option<Hsla>, bool)>)> {
        let content = self.term.renderable_content();
        let rows = self.term.screen_lines();
        let cols = self.term.columns();
        let mut lines: Vec<(String, Vec<(usize, Hsla, Option<Hsla>, bool)>)> = vec![(String::new(), vec![]); rows];
        let offset = content.display_offset as i32;
        for indexed in content.display_iter {
            let row = (indexed.point.line.0 + offset) as usize;
            if row >= rows || indexed.point.column.0 >= cols {
                continue;
            }
            let cell = indexed.cell;
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            let (mut fg, mut bg) = (self.color(cell.fg), self.color(cell.bg));
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            let bg = (bg != self.bg).then_some(bg);
            let bold = cell.flags.contains(Flags::BOLD);
            let (text, runs) = &mut lines[row];
            let c = if cell.c == '\0' { ' ' } else { cell.c };
            let len = c.len_utf8();
            text.push(c);
            match runs.last_mut() {
                Some(last) if last.1 == fg && last.2 == bg && last.3 == bold => last.0 += len,
                _ => runs.push((len, fg, bg, bold)),
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
        let cursor = self.term.renderable_content().cursor.point;
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
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, _| this.focus.focus(window)))
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
                        for (i, (text, runs)) in rows.iter().enumerate() {
                            let y = origin.y + line_height * i as f32;
                            let mut x = origin.x;
                            for (len, _, bg, _) in runs.iter() {
                                let n = *len as f32;
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
                                .filter_map(|(len, color, _, bold)| {
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
                        let cy = origin.y + line_height * cursor.line.0.max(0) as f32;
                        let mut c = fg;
                        c.a = if focused { 0.75 } else { 0.3 };
                        window.paint_quad(fill(Bounds::new(point(cx_, cy), size(cell_width, line_height)), c));
                    },
                )
                .size_full(),
            )
    }
}
