//! One terminal pane: a child process on a PTY, emulated by libghostty-vt, drawn into a ratatui buffer.
//!
//! ghostty handles are `!Send`, so a `Pane` lives on the server loop's thread. A reader thread sends the
//! child's output there (`Ev::Output`), and a waiter thread reports its exit (`Ev::Exited`).
use crate::server::Ev;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ghostty_vt::render::{CellIterator, CursorVisualStyle, RowIterator};
use ghostty_vt::screen::CellWide;
use ghostty_vt::style::StyleColor;
use ghostty_vt::terminal::{Event as GEvent, Mode, ScrollViewport};
use ghostty_vt::{key, mouse, RenderState, Terminal, TerminalOptions};
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::mpsc::Sender;

pub struct Pane {
    pub id: u32,
    pub name: String,
    term: Terminal,
    render: RenderState,
    rows: RowIterator,
    cells: CellIterator,
    keys: key::Encoder,
    mouse: mouse::Encoder,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    pub exited: Option<Option<i32>>,
    size: (u16, u16),
}

fn pty_size(cols: u16, rows: u16) -> PtySize {
    PtySize { rows: rows.max(1), cols: cols.max(1), pixel_width: 0, pixel_height: 0 }
}

impl Pane {
    pub fn spawn(
        id: u32,
        name: &str,
        argv: &[String],
        cwd: Option<&str>,
        env: HashMap<String, String>,
        (cols, rows): (u16, u16),
        tx: Sender<Ev>,
    ) -> anyhow::Result<Pane> {
        let (cols, rows) = (cols.max(1), rows.max(1));
        let pair = native_pty_system().openpty(pty_size(cols, rows))?;
        let mut cmd = CommandBuilder::new(argv.first().ok_or_else(|| anyhow::anyhow!("empty argv"))?);
        cmd.args(&argv[1..]);
        if let Some(d) = cwd {
            cmd.cwd(d);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);
        let killer = child.clone_killer();
        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let out = tx.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 64 * 1024];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 || out.send(Ev::Output(id, buf[..n].to_vec())).is_err() {
                    break;
                }
            }
        });
        std::thread::spawn(move || {
            let code = child.wait().ok().map(|s| s.exit_code() as i32);
            let _ = tx.send(Ev::Exited(id, code));
        });
        let mut term = Terminal::new(TerminalOptions { cols, rows, ..Default::default() })?;
        term.resize(cols, rows, 1, 1)?;
        Ok(Pane {
            id,
            name: name.to_string(),
            term,
            render: RenderState::new()?,
            rows: RowIterator::new()?,
            cells: CellIterator::new()?,
            keys: key::Encoder::new()?,
            mouse: mouse::Encoder::new()?,
            master: pair.master,
            writer,
            killer,
            exited: None,
            size: (cols, rows),
        })
    }

    /// Child output: through the emulator; its replies (cursor reports, mode queries) go back to the child.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.term.vt_write(bytes);
        let replies: Vec<Vec<u8>> = self.term.events().filter_map(|e| if let GEvent::PtyWrite(b) = e { Some(b) } else { None }).collect();
        for r in replies {
            self.write(&r);
        }
    }

    pub fn write(&mut self, bytes: &[u8]) {
        let _ = self.writer.write_all(bytes).and_then(|_| self.writer.flush());
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        if self.size == (cols, rows) || cols == 0 || rows == 0 {
            return;
        }
        self.size = (cols, rows);
        let _ = self.term.resize(cols, rows, 1, 1);
        let _ = self.master.resize(pty_size(cols, rows));
    }

    pub fn kill(&mut self) {
        let _ = self.killer.kill();
    }

    fn mode(&self, m: Mode) -> bool {
        self.term.mode(m).unwrap_or(false)
    }

    pub fn wants_mouse(&self) -> bool {
        self.term.is_mouse_tracking().unwrap_or(false)
    }

    /// A key, encoded the way the program in the pane asked for (legacy, modifyOtherKeys or kitty).
    pub fn key(&mut self, k: KeyEvent) {
        let Some(bytes) = self.encode_key(k) else { return };
        self.term.scroll_viewport(ScrollViewport::Bottom);
        self.write(&bytes);
    }

    fn encode_key(&mut self, k: KeyEvent) -> Option<Vec<u8>> {
        let action = match k.kind {
            KeyEventKind::Press => key::Action::Press,
            KeyEventKind::Repeat => key::Action::Repeat,
            KeyEventKind::Release => key::Action::Release,
        };
        let mut ev = key::Event::new().ok()?;
        ev.set_action(action).set_key(key_of(k.code)).set_mods(mods(k.modifiers));
        if let KeyCode::Char(c) = k.code {
            // Ctrl+letter has no text of its own; the encoder derives the control byte from the key.
            if !k.modifiers.contains(KeyModifiers::CONTROL) {
                ev.set_utf8(Some(c.to_string()));
            }
            ev.set_unshifted_codepoint(c.to_ascii_lowercase());
        }
        let mut out = vec![];
        self.keys.set_options_from_terminal(&self.term);
        self.keys.encode_to_vec(&ev, &mut out).ok()?;
        Some(out)
    }

    /// A mouse event at pane-relative cell (x, y), for a program that tracks the mouse.
    pub fn mouse_event(&mut self, m: MouseEvent, x: u16, y: u16) {
        let (action, button) = match m.kind {
            MouseEventKind::Down(b) => (mouse::Action::Press, Some(button(b))),
            MouseEventKind::Up(b) => (mouse::Action::Release, Some(button(b))),
            MouseEventKind::Drag(b) => (mouse::Action::Motion, Some(button(b))),
            MouseEventKind::Moved => (mouse::Action::Motion, None),
            MouseEventKind::ScrollUp => (mouse::Action::Press, Some(mouse::Button::Four)),
            MouseEventKind::ScrollDown => (mouse::Action::Press, Some(mouse::Button::Five)),
            _ => return,
        };
        let Ok(mut ev) = mouse::Event::new() else { return };
        ev.set_action(action).set_button(button).set_mods(mods(m.modifiers)).set_position(mouse::Position { x: x as f32, y: y as f32 });
        let size = mouse::EncoderSize {
            screen_width: self.size.0 as u32,
            screen_height: self.size.1 as u32,
            cell_width: 1,
            cell_height: 1,
            padding_top: 0,
            padding_bottom: 0,
            padding_right: 0,
            padding_left: 0,
        };
        self.mouse.set_options_from_terminal(&self.term).set_size(size);
        let mut out = vec![];
        if self.mouse.encode_to_vec(&ev, &mut out).is_ok() && !out.is_empty() {
            self.write(&out);
        }
    }

    pub fn scroll(&mut self, lines: isize) {
        self.term.scroll_viewport(ScrollViewport::Delta(lines));
    }

    pub fn paste(&mut self, text: &str) {
        let mut data = text.as_bytes().to_vec();
        let bracketed = self.mode(Mode::BRACKETED_PASTE);
        let mut buf = vec![0u8; data.len() * 2 + 16];
        if let Ok(n) = ghostty_vt::paste::encode(&mut data, bracketed, &mut buf) {
            self.term.scroll_viewport(ScrollViewport::Bottom);
            self.write(&buf[..n]);
        }
    }

    pub fn focus_event(&mut self, gained: bool) {
        if self.mode(Mode::FOCUS_EVENT) {
            self.write(if gained { b"\x1b[I" } else { b"\x1b[O" });
        }
    }

    /// Draws the viewport into `area`; returns the cursor position and DECSCUSR shape if it should show.
    pub fn draw(&mut self, buf: &mut Buffer, area: Rect) -> Option<(Position, u8)> {
        let snap = self.render.update(&self.term).ok()?;
        let mut rows = self.rows.update(&snap).ok()?;
        let mut y = 0u16;
        let mut text = String::new();
        while let Some(row) = rows.next() {
            if y >= area.height {
                break;
            }
            let Ok(mut cells) = self.cells.update(row) else { break };
            let mut x = 0u16;
            while let Some(c) = cells.next() {
                if x >= area.width {
                    break;
                }
                let raw = c.raw_cell().ok();
                let wide = raw.and_then(|r| r.wide().ok());
                if !matches!(wide, Some(CellWide::SpacerTail | CellWide::SpacerHead))
                    && let Some(out) = buf.cell_mut((area.x + x, area.y + y)) {
                        text.clear();
                        let _ = c.graphemes_utf8(&mut text);
                        out.set_symbol(if text.is_empty() { " " } else { &text });
                        if let Ok(s) = c.style() {
                            out.set_fg(color(s.fg_color));
                            out.set_bg(color(s.bg_color));
                            out.modifier = style_mods(&s);
                        }
                    }
                x += 1;
            }
            y += 1;
        }
        let cur = snap.cursor_viewport().ok().flatten().filter(|_| snap.cursor_visible().unwrap_or(false));
        let shape = match snap.cursor_visual_style() {
            Ok(CursorVisualStyle::Bar) => 6,
            Ok(CursorVisualStyle::Underline) => 4,
            _ => 2,
        };
        let _ = snap.clean();
        cur.filter(|c| c.x < area.width && c.y < area.height).map(|c| (Position::new(area.x + c.x, area.y + c.y), shape))
    }

    /// The screen's text, for `dump`.
    pub fn text(&mut self) -> String {
        let mut b = Buffer::empty(Rect::new(0, 0, self.size.0, self.size.1));
        let area = b.area;
        self.draw(&mut b, area);
        (0..area.height)
            .map(|y| (0..area.width).map(|x| b[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn style_mods(s: &ghostty_vt::style::Style) -> Modifier {
    let mut m = Modifier::empty();
    for (on, md) in [
        (s.bold, Modifier::BOLD),
        (s.italic, Modifier::ITALIC),
        (s.faint, Modifier::DIM),
        (s.inverse, Modifier::REVERSED),
        (s.strikethrough, Modifier::CROSSED_OUT),
        (s.blink, Modifier::SLOW_BLINK),
        (s.invisible, Modifier::HIDDEN),
        (s.underline != ghostty_vt::style::Underline::None, Modifier::UNDERLINED),
    ] {
        if on {
            m |= md;
        }
    }
    m
}

/// Palette colours stay palette colours, so panes follow the outer terminal's theme.
fn color(c: StyleColor) -> Color {
    match c {
        StyleColor::None => Color::Reset,
        StyleColor::Rgb(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
        StyleColor::Palette(i) => {
            const NAMED: [Color; 16] = [
                Color::Black,
                Color::Red,
                Color::Green,
                Color::Yellow,
                Color::Blue,
                Color::Magenta,
                Color::Cyan,
                Color::Gray,
                Color::DarkGray,
                Color::LightRed,
                Color::LightGreen,
                Color::LightYellow,
                Color::LightBlue,
                Color::LightMagenta,
                Color::LightCyan,
                Color::White,
            ];
            let i = i.0;
            NAMED.get(i as usize).copied().unwrap_or(Color::Indexed(i))
        }
    }
}

fn mods(m: KeyModifiers) -> key::Mods {
    let mut out = key::Mods::empty();
    for (c, g) in [
        (KeyModifiers::SHIFT, key::Mods::SHIFT),
        (KeyModifiers::ALT, key::Mods::ALT),
        (KeyModifiers::CONTROL, key::Mods::CTRL),
        (KeyModifiers::SUPER, key::Mods::SUPER),
    ] {
        if m.contains(c) {
            out |= g;
        }
    }
    out
}

fn button(b: MouseButton) -> mouse::Button {
    match b {
        MouseButton::Left => mouse::Button::Left,
        MouseButton::Right => mouse::Button::Right,
        MouseButton::Middle => mouse::Button::Middle,
    }
}

/// The physical key for a crossterm key code; text keys outside the US layout go by their UTF-8 alone.
fn key_of(code: KeyCode) -> key::Key {
    use key::Key as K;
    match code {
        KeyCode::Backspace => K::Backspace,
        KeyCode::Enter => K::Enter,
        KeyCode::Left => K::ArrowLeft,
        KeyCode::Right => K::ArrowRight,
        KeyCode::Up => K::ArrowUp,
        KeyCode::Down => K::ArrowDown,
        KeyCode::Home => K::Home,
        KeyCode::End => K::End,
        KeyCode::PageUp => K::PageUp,
        KeyCode::PageDown => K::PageDown,
        KeyCode::Tab | KeyCode::BackTab => K::Tab,
        KeyCode::Delete => K::Delete,
        KeyCode::Insert => K::Insert,
        KeyCode::Esc => K::Escape,
        KeyCode::F(n) => [K::F1, K::F2, K::F3, K::F4, K::F5, K::F6, K::F7, K::F8, K::F9, K::F10, K::F11, K::F12]
            .get(n as usize - 1)
            .copied()
            .unwrap_or(K::Unidentified),
        KeyCode::Char(c) => match c.to_ascii_lowercase() {
            c @ 'a'..='z' => [
                K::A, K::B, K::C, K::D, K::E, K::F, K::G, K::H, K::I, K::J, K::K, K::L, K::M, K::N, K::O, K::P, K::Q, K::R, K::S, K::T,
                K::U, K::V, K::W, K::X, K::Y, K::Z,
            ][(c as u8 - b'a') as usize],
            c @ '0'..='9' => [K::Digit0, K::Digit1, K::Digit2, K::Digit3, K::Digit4, K::Digit5, K::Digit6, K::Digit7, K::Digit8, K::Digit9]
                [(c as u8 - b'0') as usize],
            ' ' => K::Space,
            '-' => K::Minus,
            '=' => K::Equal,
            '[' => K::BracketLeft,
            ']' => K::BracketRight,
            '\\' => K::Backslash,
            ';' => K::Semicolon,
            '\'' => K::Quote,
            ',' => K::Comma,
            '.' => K::Period,
            '/' => K::Slash,
            '`' => K::Backquote,
            _ => K::Unidentified,
        },
        _ => K::Unidentified,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane() -> Pane {
        let (tx, _rx) = std::sync::mpsc::channel();
        Pane::spawn(1, "t", &["cat".into()], None, HashMap::new(), (20, 4), tx).unwrap()
    }

    #[test]
    fn colours_map_to_the_palette() {
        assert_eq!(color(StyleColor::None), Color::Reset);
        assert_eq!(color(StyleColor::Palette(ghostty_vt::style::PaletteIndex(12))), Color::LightBlue);
        assert_eq!(color(StyleColor::Palette(ghostty_vt::style::PaletteIndex(208))), Color::Indexed(208));
    }

    #[test]
    fn output_renders_and_keys_follow_the_program_mode() {
        let mut p = pane();
        p.feed(b"hello\r\n\x1b[31mred\x1b[0m");
        assert_eq!(p.text().lines().take(2).collect::<Vec<_>>(), ["hello", "red"]);
        let shift_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT);
        // Legacy mode: Shift+Enter still differs from Enter (xterm's modifyOtherKeys form).
        assert_eq!(p.encode_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).unwrap(), b"\r");
        assert_eq!(p.encode_key(shift_enter).unwrap(), b"\x1b[27;2;13~");
        // After the program asks for the kitty keyboard protocol (CSI > 1 u), it is told apart.
        p.feed(b"\x1b[>1u");
        assert_eq!(p.encode_key(shift_enter).unwrap(), b"\x1b[13;2u");
        assert_eq!(p.encode_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)).unwrap(), b"\x1b[99;5u");
        p.kill();
    }
}
