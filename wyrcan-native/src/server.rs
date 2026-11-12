//! The server: owns the panes, draws the screen for the attached client, answers `pipe` calls.
//! Everything runs on one loop fed by a channel; connection and PTY threads only send `Ev`s
//! (ghostty's terminals are `!Send`, so they never leave this thread).
use crate::pane::Pane;
use crate::proto::{self, Hello};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::{Terminal, TerminalOptions, Viewport};
use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

pub enum Ev {
    Output(u32, Vec<u8>),
    Exited(u32, Option<i32>),
    Attach(UnixStream, u16, u16),
    Input(u64, Event),
    Gone(u64),
    Pipe { name: String, args: BTreeMap<String, String>, payload: Option<String>, out: UnixStream },
}

struct Client {
    id: u64,
    /// Buffered: a frame is many small escape sequences; ratatui flushes once per draw.
    term: Terminal<CrosstermBackend<BufWriter<UnixStream>>>,
    size: (u16, u16),
    /// To hang up: the reader thread holds another handle, so dropping `term` alone leaves it open.
    sock: UnixStream,
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.sock.shutdown(std::net::Shutdown::Both);
    }
}

/// Where everything goes on screen.
#[derive(Default, Debug, PartialEq)]
struct Layout {
    panes: Vec<(u32, Rect)>,
    titles: Vec<(u32, Rect)>,
    placeholder: Option<Rect>,
    status: Rect,
}

struct Server {
    session: String,
    exe: String,
    tx: Sender<Ev>,
    panes: Vec<Pane>,
    next_id: u32,
    focus: u32,
    expanded: Option<u32>,
    fullscreen: Option<u32>,
    client: Option<Client>,
    next_client: u64,
    subscribers: Vec<UnixStream>,
    panes_line: String,
    dirty: bool,
    last_screen: Option<Buffer>,
    quit: bool,
}

const BOARD: u32 = 0;
const STATUS: &str = " wyrcan  Alt b board · Alt l agents · Alt j/k next/prev agent · Alt d detach ";

/// Splits `size` into the board (left), the agent stack (right) and a status line.
/// Every agent has a title row; the expanded one also gets the rest of the column.
fn layout(size: (u16, u16), agents: &[u32], expanded: Option<u32>, fullscreen: Option<u32>) -> Layout {
    let (w, h) = size;
    let body = Rect::new(0, 0, w, h.saturating_sub(1));
    let status = Rect::new(0, h.saturating_sub(1), w, 1.min(h));
    if let Some(f) = fullscreen {
        return Layout { panes: vec![(f, body)], status, ..Default::default() };
    }
    let left = w / 2;
    let right = Rect::new(left + 1, 0, w.saturating_sub(left + 1), body.height);
    let mut l = Layout { panes: vec![(BOARD, Rect::new(0, 0, left, body.height))], status, ..Default::default() };
    if agents.is_empty() {
        l.placeholder = Some(right);
        return l;
    }
    let open = expanded.filter(|e| agents.contains(e)).unwrap_or(*agents.last().unwrap());
    let body_rows = right.height.saturating_sub(agents.len() as u16);
    let mut y = right.y;
    for &a in agents {
        l.titles.push((a, Rect::new(right.x, y, right.width, 1)));
        y += 1;
        if a == open {
            l.panes.push((a, Rect::new(right.x, y, right.width, body_rows)));
            y += body_rows;
        }
    }
    l
}

/// What an Alt+key does, if it's one of wyrcan's.
#[derive(Debug, PartialEq)]
enum Hotkey {
    Board,
    Agents,
    Next,
    Prev,
    Detach,
}

fn hotkey(e: &Event) -> Option<Hotkey> {
    let Event::Key(k) = e else { return None };
    if k.kind == KeyEventKind::Release || k.modifiers != KeyModifiers::ALT {
        return None;
    }
    Some(match k.code {
        KeyCode::Char('b' | 'h') | KeyCode::Left => Hotkey::Board,
        KeyCode::Char('l') | KeyCode::Right => Hotkey::Agents,
        KeyCode::Char('j') | KeyCode::Down => Hotkey::Next,
        KeyCode::Char('k') | KeyCode::Up => Hotkey::Prev,
        KeyCode::Char('d') => Hotkey::Detach,
        _ => return None,
    })
}

impl Server {
    fn pane(&mut self, id: u32) -> Option<&mut Pane> {
        self.panes.iter_mut().find(|p| p.id == id)
    }

    fn agents(&self) -> Vec<u32> {
        self.panes.iter().map(|p| p.id).filter(|&i| i != BOARD).collect()
    }

    fn size(&self) -> (u16, u16) {
        self.client.as_ref().map_or((120, 40), |c| c.size)
    }

    fn layout(&self) -> Layout {
        layout(self.size(), &self.agents(), self.expanded, self.fullscreen)
    }

    /// Visible panes take their rect's size; collapsed agents keep the expanded agent's size, so they don't reflow.
    fn fit(&mut self) {
        let l = self.layout();
        let agent = l.panes.iter().find(|(id, _)| *id != BOARD).map(|(_, r)| (r.width, r.height));
        for p in &mut self.panes {
            if let Some((_, r)) = l.panes.iter().find(|(id, _)| *id == p.id) {
                p.resize(r.width, r.height);
            } else if let (true, Some((w, h))) = (p.id != BOARD, agent) {
                p.resize(w, h);
            }
        }
        self.dirty = true;
    }

    fn env(&self, id: u32) -> HashMap<String, String> {
        // The board and its hooks read zellij's variables; WYRCAN tells the board which binary to call.
        HashMap::from([
            ("ZELLIJ".into(), "0".into()),
            ("ZELLIJ_SESSION_NAME".into(), self.session.clone()),
            ("ZELLIJ_PANE_ID".into(), id.to_string()),
            ("WYRCAN".into(), self.exe.clone()),
            ("TERM".into(), "xterm-256color".into()),
            ("COLORTERM".into(), "truecolor".into()),
        ])
    }

    fn spawn(&mut self, name: &str, argv: &[String], cwd: Option<&str>) -> anyhow::Result<u32> {
        let id = self.next_id;
        let l = self.layout();
        let (w, h) = l.panes.iter().find(|(i, _)| *i != BOARD).map_or((self.size().0 / 2, self.size().1), |(_, r)| (r.width, r.height));
        let p = Pane::spawn(id, name, argv, cwd, self.env(id), (w.max(10), h.max(3)), self.tx.clone())?;
        self.next_id += 1;
        self.panes.push(p);
        Ok(id)
    }

    fn publish(&mut self) {
        let line = std::iter::once("panes".to_string()).chain(self.panes.iter().map(|p| format!("terminal_{}", p.id))).collect::<Vec<_>>().join(" ");
        if line != self.panes_line {
            self.panes_line = line;
            let msg = format!("{}\n", self.panes_line);
            self.subscribers.retain_mut(|s| s.write_all(msg.as_bytes()).is_ok());
        }
    }

    /// Composes the whole screen into a buffer, then hands it to ratatui, which sends only what changed.
    fn render(&mut self) {
        let Some((w, h)) = self.client.as_ref().map(|c| c.size) else { return };
        let l = self.layout();
        // A visible pane is mid-way through a synchronized redraw: wait for the rest (dirty stays set).
        if l.panes.iter().any(|(id, _)| self.panes.iter().any(|p| p.id == *id && p.held())) {
            return;
        }
        let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
        let mut cursor = None;
        let focus = self.focus;
        for &(id, r) in &l.panes {
            if let Some(p) = self.pane(id) {
                let cur = p.draw(&mut buf, r);
                if id == focus {
                    cursor = cur;
                }
            }
        }
        if let Some(&(_, b)) = l.panes.first().filter(|(id, _)| *id == BOARD && self.fullscreen.is_none()) {
            for y in 0..b.height {
                buf.set_string(b.right(), y, "│", Style::default().fg(Color::DarkGray));
            }
        }
        for &(id, r) in &l.titles {
            let Some(p) = self.panes.iter().find(|p| p.id == id) else { continue };
            let state = match p.exited {
                Some(Some(c)) => format!(" (exited {c})"),
                Some(None) => " (exited)".into(),
                None => String::new(),
            };
            let style = if id == focus { Style::default().fg(Color::Black).bg(Color::Cyan) } else { Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM) };
            buf.set_style(r, style);
            buf.set_stringn(r.x, r.y, format!(" ▸ {}{state}", p.name), r.width as usize, style);
        }
        if let Some(r) = l.placeholder {
            buf.set_stringn(r.x + 2, r.y + 1, "Agents open here.", r.width.saturating_sub(2) as usize, Style::default().add_modifier(Modifier::DIM));
        }
        let bar = Style::default().fg(Color::Black).bg(Color::DarkGray);
        buf.set_style(l.status, bar);
        buf.set_stringn(l.status.x, l.status.y, STATUS, l.status.width as usize, bar);

        let Some(c) = self.client.as_mut() else { return };
        // Synchronized output: the outer terminal shows the frame whole, never half-painted.
        let _ = c.term.backend_mut().write_all(b"\x1b[?2026h");
        let drawn = c.term.draw(|f| {
            f.buffer_mut().merge(&buf);
            if let Some((pos, _)) = cursor {
                f.set_cursor_position(pos);
            }
        });
        if drawn.is_err() {
            self.client = None;
            return;
        }
        // DECSCUSR: the focused pane's cursor shape.
        let _ = write!(c.term.backend_mut(), "\x1b[{} q\x1b[?2026l", cursor.map_or(2, |(_, s)| s));
        let _ = c.term.backend_mut().flush();
        self.last_screen = Some(buf);
        self.dirty = false;
    }

    fn hotkey(&mut self, h: Hotkey) {
        let agents = self.agents();
        match h {
            Hotkey::Board => {
                self.focus = BOARD;
                self.fullscreen = None;
            }
            Hotkey::Agents => {
                if let Some(e) = self.expanded.filter(|e| agents.contains(e)).or(agents.last().copied()) {
                    self.focus = e;
                    self.expanded = Some(e);
                }
            }
            Hotkey::Next | Hotkey::Prev => {
                let step = if h == Hotkey::Next { 1 } else { -1 };
                if let Some(cur) = self.expanded.and_then(|e| agents.iter().position(|&a| a == e)) {
                    let a = agents[(cur as isize + step).rem_euclid(agents.len() as isize) as usize];
                    self.expanded = Some(a);
                    self.focus = a;
                }
            }
            Hotkey::Detach => {
                self.client = None;
                return;
            }
        }
        self.fit();
    }

    fn input(&mut self, e: Event) {
        if let Some(h) = hotkey(&e) {
            return self.hotkey(h);
        }
        let focus = self.focus;
        match e {
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                if let Some(p) = self.pane(focus) {
                    p.key(k);
                }
            }
            Event::Mouse(m) => self.mouse(m),
            Event::Paste(s) => {
                if let Some(p) = self.pane(focus) {
                    p.paste(&s);
                }
            }
            Event::FocusGained | Event::FocusLost => {
                if let Some(p) = self.pane(focus) {
                    p.focus_event(matches!(e, Event::FocusGained));
                }
            }
            Event::Resize(w, h) => {
                if let Some(c) = self.client.as_mut() {
                    c.size = (w, h);
                    let _ = c.term.resize(Rect::new(0, 0, w, h));
                    let _ = c.term.clear();
                }
                self.fit();
            }
            _ => {}
        }
    }

    fn mouse(&mut self, m: crossterm::event::MouseEvent) {
        let l = self.layout();
        let at = Position::new(m.column, m.row);
        let press = matches!(m.kind, MouseEventKind::Down(MouseButton::Left));
        if let Some(&(id, _)) = l.titles.iter().find(|(_, r)| r.contains(at)) {
            if press {
                self.expanded = Some(id);
                self.focus = id;
                self.fit();
            }
            return;
        }
        let Some(&(id, r)) = l.panes.iter().find(|(_, r)| r.contains(at)) else { return };
        if press && id != self.focus {
            self.focus = id;
            self.dirty = true;
        }
        let Some(p) = self.pane(id) else { return };
        if p.wants_mouse() {
            p.mouse_event(m, m.column - r.x, m.row - r.y);
        } else if let MouseEventKind::ScrollUp | MouseEventKind::ScrollDown = m.kind {
            p.scroll(if m.kind == MouseEventKind::ScrollUp { -3 } else { 3 });
            self.dirty = true;
        }
    }

    fn pipe(&mut self, name: &str, args: &BTreeMap<String, String>, payload: Option<String>, mut out: UnixStream) {
        if name == "events" {
            let _ = out.write_all(format!("{}\n", self.panes_line).as_bytes());
            self.subscribers.push(out);
            return;
        }
        let text = match self.command(name, args, payload) {
            Ok(s) => format!("{s}\n"),
            Err(e) => format!("error: {e}\n"),
        };
        let _ = out.write_all(text.as_bytes());
    }

    fn pane_arg(&self, args: &BTreeMap<String, String>) -> anyhow::Result<u32> {
        let id: u32 = args.get("pane").and_then(|p| p.strip_prefix("terminal_")?.parse().ok()).ok_or_else(|| anyhow::anyhow!("needs --args pane=terminal_<n>"))?;
        self.panes.iter().any(|p| p.id == id).then_some(id).ok_or_else(|| anyhow::anyhow!("no pane terminal_{id}"))
    }

    fn command(&mut self, name: &str, args: &BTreeMap<String, String>, payload: Option<String>) -> anyhow::Result<String> {
        match name {
            "open" => {
                let v: serde_json::Value = serde_json::from_str(payload.as_deref().unwrap_or(""))?;
                let argv: Vec<String> = serde_json::from_value(v["argv"].clone())?;
                let id = self.spawn(v["name"].as_str().unwrap_or("agent"), &argv, v["cwd"].as_str())?;
                self.expanded = Some(id);
                self.fit();
                self.publish();
                return Ok(format!("terminal_{id}"));
            }
            "focus" => {
                let id = self.pane_arg(args)?;
                self.focus = id;
                if id != BOARD {
                    self.expanded = Some(id);
                }
                self.fit();
            }
            "fullscreen" => {
                let id = self.pane_arg(args)?;
                self.fullscreen = if self.fullscreen == Some(id) { None } else { Some(id) };
                self.fit();
            }
            "close" => {
                let id = self.pane_arg(args)?;
                self.close(id);
            }
            "write" => {
                let id = self.pane_arg(args)?;
                let mut b = payload.unwrap_or_default().into_bytes();
                b.push(b'\r');
                self.pane(id).expect("checked by pane_arg").write(&b);
            }
            "dump" => {
                if args.contains_key("pane") {
                    let id = self.pane_arg(args)?;
                    return Ok(self.pane(id).expect("checked").text());
                }
                let b = self.last_screen.as_ref().ok_or_else(|| anyhow::anyhow!("nothing drawn yet"))?;
                return Ok(screen_text(b));
            }
            "info" => {
                let live = self.panes.iter().filter(|p| p.exited.is_none()).count();
                let client = self.client.as_ref().map_or("detached".to_string(), |c| format!("attached {}x{}", c.size.0, c.size.1));
                return Ok(format!("{} panes ({live} running), {client}", self.panes.len()));
            }
            "kill" => self.quit = true,
            other => anyhow::bail!("unknown command {other}"),
        }
        Ok("ok".into())
    }

    fn close(&mut self, id: u32) {
        if let Some(i) = self.panes.iter().position(|p| p.id == id) {
            self.panes.remove(i).kill();
        }
        if self.fullscreen == Some(id) {
            self.fullscreen = None;
        }
        if self.expanded == Some(id) {
            self.expanded = self.agents().last().copied();
        }
        if self.focus == id {
            self.focus = BOARD;
        }
        self.fit();
        self.publish();
    }

    fn handle(&mut self, ev: Ev) {
        match ev {
            Ev::Output(id, b) => {
                if let Some(p) = self.pane(id) {
                    p.feed(&b);
                    // A collapsed agent's output changes nothing on screen until it's shown.
                    if self.layout().panes.iter().any(|(v, _)| *v == id) {
                        self.dirty = true;
                    }
                }
            }
            Ev::Exited(id, code) => {
                if let Some(p) = self.pane(id) {
                    p.exited = Some(code);
                }
                // Nothing left running: the session is over.
                if self.panes.iter().all(|p| p.exited.is_some()) {
                    self.quit = true;
                }
                self.dirty = true;
            }
            Ev::Attach(s, w, h) => {
                let id = self.next_client;
                self.next_client += 1;
                let opts = TerminalOptions { viewport: Viewport::Fixed(Rect::new(0, 0, w, h)) };
                let (Ok(reader), Ok(sock)) = (s.try_clone(), s.try_clone()) else { return };
                if let Ok(mut term) = Terminal::with_options(CrosstermBackend::new(BufWriter::with_capacity(256 * 1024, s)), opts) {
                    let _ = term.clear();
                    // One client at a time: a new attach takes over (the old one is hung up on drop).
                    self.client = Some(Client { id, term, size: (w, h), sock });
                    read_client(id, reader, self.tx.clone());
                    self.fit();
                }
            }
            Ev::Input(id, e) if self.client.as_ref().is_some_and(|c| c.id == id) => self.input(e),
            Ev::Input(..) => {}
            Ev::Gone(id) => {
                if self.client.as_ref().is_some_and(|c| c.id == id) {
                    self.client = None;
                }
            }
            Ev::Pipe { name, args, payload, out } => self.pipe(&name, &args, payload, out),
        }
    }
}

fn screen_text(b: &Buffer) -> String {
    (0..b.area.height)
        .map(|y| (0..b.area.width).map(|x| b[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// After the hello line, an attached client sends one crossterm event per line, as JSON.
fn read_client(id: u64, s: UnixStream, tx: Sender<Ev>) {
    std::thread::spawn(move || {
        for line in BufReader::new(s).lines() {
            let Ok(line) = line else { break };
            if let Ok(e) = serde_json::from_str::<Event>(&line)
                && tx.send(Ev::Input(id, e)).is_err() {
                    return;
                }
        }
        let _ = tx.send(Ev::Gone(id));
    });
}

fn accept(listener: UnixListener, tx: Sender<Ev>) {
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let tx = tx.clone();
            std::thread::spawn(move || {
                let Ok(line) = proto::read_line(&conn) else { return };
                let ev = match serde_json::from_str::<Hello>(&line) {
                    Ok(Hello::Attach { cols, rows }) => Ev::Attach(conn, cols, rows),
                    Ok(Hello::Pipe { name, args, payload }) => Ev::Pipe { name, args, payload, out: conn },
                    Err(_) => return,
                };
                let _ = tx.send(ev);
            });
        }
    });
}

pub fn run(session: &str) -> anyhow::Result<()> {
    let path = proto::socket(session);
    std::fs::create_dir_all(path.parent().unwrap())?;
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    let (tx, rx): (Sender<Ev>, Receiver<Ev>) = channel();
    accept(listener, tx.clone());
    let exe = std::env::current_exe()?.to_string_lossy().into_owned();
    let mut s = Server {
        session: session.to_string(),
        exe,
        tx: tx.clone(),
        panes: vec![],
        next_id: 0,
        focus: BOARD,
        expanded: None,
        fullscreen: None,
        client: None,
        next_client: 1,
        subscribers: vec![],
        panes_line: String::new(),
        dirty: true,
        last_screen: None,
        quit: false,
    };
    let board = std::env::var("WYRCAN_BOARD").unwrap_or_else(|_| "claude-board".into());
    s.spawn("board", &[board], None)?;
    s.publish();
    let frame = Duration::from_millis(16);
    let mut last = Instant::now();
    // WYRCAN_PROFILE=1: render cost to the log every 5 s.
    let profile = std::env::var_os("WYRCAN_PROFILE").is_some();
    let (mut frames, mut spent, mut worst, mut since) = (0u32, Duration::ZERO, Duration::ZERO, Instant::now());
    while !s.quit {
        // Idle: block. Dirty: wait only for the rest of this frame, then draw.
        let wait = if s.dirty { frame.saturating_sub(last.elapsed()) } else { Duration::from_secs(1) };
        match rx.recv_timeout(wait) {
            Ok(ev) => {
                s.handle(ev);
                // Take what's already queued, so one frame covers it (capped: a flooding pane mustn't starve drawing).
                for ev in rx.try_iter().take(256) {
                    s.handle(ev);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        // Coalesce: PTY output arrives per read; draw at most once a frame.
        if s.dirty && last.elapsed() >= frame {
            let t = Instant::now();
            s.render();
            last = Instant::now();
            frames += 1;
            spent += last - t;
            worst = worst.max(last - t);
        }
        if profile && since.elapsed() >= Duration::from_secs(5) {
            eprintln!("render: {frames} frames, avg {:?}, worst {:?}", spent / frames.max(1), worst);
            (frames, spent, worst, since) = (0, Duration::ZERO, Duration::ZERO, Instant::now());
        }
    }
    for p in &mut s.panes {
        p.kill();
    }
    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    #[test]
    fn layout_board_left_agents_stacked_right() {
        let l = layout((100, 31), &[1, 2, 3], Some(2), None);
        assert_eq!(l.panes, vec![(0, Rect::new(0, 0, 50, 30)), (2, Rect::new(51, 2, 49, 27))]);
        assert_eq!(l.titles.iter().map(|(id, r)| (*id, r.y)).collect::<Vec<_>>(), [(1, 0), (2, 1), (3, 29)]);
        assert_eq!(l.status, Rect::new(0, 30, 100, 1));
    }

    #[test]
    fn layout_without_agents_has_a_placeholder_and_fullscreen_takes_all() {
        assert_eq!(layout((80, 24), &[], None, None).placeholder, Some(Rect::new(41, 0, 39, 23)));
        assert_eq!(layout((80, 24), &[1], None, Some(1)).panes, vec![(1, Rect::new(0, 0, 80, 23))]);
    }

    #[test]
    fn only_bare_alt_keys_are_hotkeys() {
        let k = |c, m| Event::Key(KeyEvent::new(c, m));
        assert_eq!(hotkey(&k(KeyCode::Char('b'), KeyModifiers::ALT)), Some(Hotkey::Board));
        assert_eq!(hotkey(&k(KeyCode::Right, KeyModifiers::ALT)), Some(Hotkey::Agents));
        assert_eq!(hotkey(&k(KeyCode::Char('b'), KeyModifiers::NONE)), None);
        assert_eq!(hotkey(&k(KeyCode::Char('b'), KeyModifiers::ALT | KeyModifiers::SHIFT)), None);
    }
}
