//! One terminal pane: a child process on a PTY, emulated by alacritty_terminal, drawn into a ratatui buffer.
use crate::server::Ev;
use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, TermMode};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::{Color as AColor, CursorShape, NamedColor, Rgb};
use alacritty_terminal::Term;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier};
use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::sync::Arc;

/// Forwards the emulator's events to the server loop, tagged with the pane id.
#[derive(Clone)]
pub struct Proxy {
    id: u32,
    tx: Sender<Ev>,
}

impl EventListener for Proxy {
    fn send_event(&self, e: Event) {
        let ev = match e {
            Event::Wakeup => Ev::Wakeup,
            // Replies to queries (cursor position, device attributes) go back to the child.
            Event::PtyWrite(s) => Ev::PtyWrite(self.id, s.into_bytes()),
            // shortcut: we can't see the real terminal's colours from the server; answer as a dark theme.
            Event::ColorRequest(_, fmt) => Ev::PtyWrite(self.id, fmt(Rgb { r: 0, g: 0, b: 0 }).into_bytes()),
            Event::ChildExit(status) => Ev::Exited(self.id, status.code()),
            _ => return,
        };
        let _ = self.tx.send(ev);
    }
}

pub struct Pane {
    pub id: u32,
    pub name: String,
    pub term: Arc<FairMutex<Term<Proxy>>>,
    io: EventLoopSender,
    pub exited: Option<Option<i32>>,
    size: (u16, u16),
}

fn window(cols: u16, rows: u16) -> WindowSize {
    WindowSize { num_lines: rows.max(1), num_cols: cols.max(1), cell_width: 1, cell_height: 1 }
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
        let (prog, args) = argv.split_first().ok_or_else(|| anyhow::anyhow!("empty argv"))?;
        let opts = tty::Options {
            shell: Some(tty::Shell::new(prog.clone(), args.to_vec())),
            working_directory: cwd.map(Into::into),
            drain_on_exit: true,
            env,
        };
        let pty = tty::new(&opts, window(cols, rows), id as u64)?;
        let proxy = Proxy { id, tx };
        let size = TermSize::new(cols.max(1) as usize, rows.max(1) as usize);
        let term = Arc::new(FairMutex::new(Term::new(Config::default(), &size, proxy.clone())));
        let lp = EventLoop::new(term.clone(), proxy, pty, true, false)?;
        let io = lp.channel();
        lp.spawn();
        Ok(Pane { id, name: name.to_string(), term, io, exited: None, size: (cols, rows) })
    }

    pub fn write(&self, bytes: Vec<u8>) {
        let _ = self.io.send(Msg::Input(bytes.into()));
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        if self.size == (cols, rows) || cols == 0 || rows == 0 {
            return;
        }
        self.size = (cols, rows);
        self.term.lock().resize(TermSize::new(cols as usize, rows as usize));
        let _ = self.io.send(Msg::Resize(window(cols, rows)));
    }

    pub fn kill(&self) {
        let _ = self.io.send(Msg::Shutdown);
    }

    pub fn mode(&self) -> TermMode {
        *self.term.lock().mode()
    }

    pub fn scroll(&self, lines: i32) {
        self.term.lock().scroll_display(Scroll::Delta(lines));
    }

    pub fn scroll_to_bottom(&self) {
        self.term.lock().scroll_display(Scroll::Bottom);
    }

    /// Draws the visible screen into `area`; returns where the cursor is, if it should show.
    pub fn draw(&self, buf: &mut Buffer, area: Rect) -> Option<(Position, CursorShape)> {
        let term = self.term.lock();
        let content = term.renderable_content();
        let offset = content.display_offset as i32;
        for ic in content.display_iter {
            let (line, col) = (ic.point.line.0 + offset, ic.point.column.0);
            if line < 0 || line as u16 >= area.height || col as u16 >= area.width {
                continue;
            }
            let cell = &*ic;
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            let Some(out) = buf.cell_mut((area.x + col as u16, area.y + line as u16)) else { continue };
            let mut sym = String::from(if cell.flags.contains(Flags::HIDDEN) { ' ' } else { cell.c });
            if let Some(z) = cell.zerowidth() {
                sym.extend(z);
            }
            out.set_symbol(&sym);
            out.set_fg(color(cell.fg));
            out.set_bg(color(cell.bg));
            out.modifier = modifiers(cell.flags, cell.fg);
        }
        let cur = content.cursor;
        let line = cur.point.line.0 + offset;
        let show = content.mode.contains(TermMode::SHOW_CURSOR) && cur.shape != CursorShape::Hidden && offset == 0;
        (show && line >= 0 && (line as u16) < area.height && (cur.point.column.0 as u16) < area.width)
            .then(|| (Position::new(area.x + cur.point.column.0 as u16, area.y + line as u16), cur.shape))
    }

    /// The screen's text, for `dump`.
    pub fn text(&self) -> String {
        let term = self.term.lock();
        let g = term.grid();
        (0..g.screen_lines() as i32)
            .map(|l| {
                let row = &g[alacritty_terminal::index::Line(l)];
                (0..g.columns()).map(|c| row[alacritty_terminal::index::Column(c)].c).collect::<String>().trim_end().to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn modifiers(f: Flags, fg: AColor) -> Modifier {
    let mut m = Modifier::empty();
    for (flag, md) in [
        (Flags::BOLD, Modifier::BOLD),
        (Flags::ITALIC, Modifier::ITALIC),
        (Flags::DIM, Modifier::DIM),
        (Flags::INVERSE, Modifier::REVERSED),
        (Flags::STRIKEOUT, Modifier::CROSSED_OUT),
    ] {
        if f.contains(flag) {
            m |= md;
        }
    }
    if f.intersects(Flags::ALL_UNDERLINES) {
        m |= Modifier::UNDERLINED;
    }
    if matches!(fg, AColor::Named(n) if n as usize >= NamedColor::DimBlack as usize && n as usize <= NamedColor::DimWhite as usize) {
        m |= Modifier::DIM;
    }
    m
}

/// Named colours stay palette colours, so panes follow the user's terminal theme.
fn color(c: AColor) -> Color {
    use NamedColor as N;
    match c {
        AColor::Spec(Rgb { r, g, b }) => Color::Rgb(r, g, b),
        AColor::Indexed(i) => Color::Indexed(i),
        AColor::Named(n) => match n {
            N::Black | N::DimBlack => Color::Black,
            N::Red | N::DimRed => Color::Red,
            N::Green | N::DimGreen => Color::Green,
            N::Yellow | N::DimYellow => Color::Yellow,
            N::Blue | N::DimBlue => Color::Blue,
            N::Magenta | N::DimMagenta => Color::Magenta,
            N::Cyan | N::DimCyan => Color::Cyan,
            N::White | N::DimWhite => Color::Gray,
            N::BrightBlack => Color::DarkGray,
            N::BrightRed => Color::LightRed,
            N::BrightGreen => Color::LightGreen,
            N::BrightYellow => Color::LightYellow,
            N::BrightBlue => Color::LightBlue,
            N::BrightMagenta => Color::LightMagenta,
            N::BrightCyan => Color::LightCyan,
            N::BrightWhite => Color::White,
            _ => Color::Reset,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_map_to_the_palette() {
        assert_eq!(color(AColor::Named(NamedColor::Foreground)), Color::Reset);
        assert_eq!(color(AColor::Named(NamedColor::BrightBlue)), Color::LightBlue);
        assert_eq!(color(AColor::Indexed(208)), Color::Indexed(208));
        assert_eq!(color(AColor::Spec(Rgb { r: 1, g: 2, b: 3 })), Color::Rgb(1, 2, 3));
        assert!(modifiers(Flags::BOLD | Flags::UNDERLINE, AColor::Named(NamedColor::Foreground)).contains(Modifier::BOLD | Modifier::UNDERLINED));
    }
}
