//! What goes over the session socket.
//!
//! A connection starts with one JSON line (`Hello`). An attached client then sends frames
//! (`[0, len u32, bytes]` input, `[1, cols u16, rows u16]` resize) and reads raw terminal output.
//! A pipe call reads reply lines until the server closes the connection.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::path::PathBuf;

#[derive(Serialize, Deserialize)]
pub enum Hello {
    Attach { cols: u16, rows: u16 },
    Pipe { name: String, args: BTreeMap<String, String>, payload: Option<String> },
}

pub fn socket(session: &str) -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("wyrcan-native").join(format!("{session}.sock"))
}

/// One line, read a byte at a time so nothing after it is consumed.
pub fn read_line(mut r: impl Read) -> io::Result<String> {
    let mut out = vec![];
    let mut b = [0u8];
    while r.read(&mut b)? == 1 && b[0] != b'\n' {
        out.push(b[0]);
        if out.len() > 1 << 20 {
            return Err(io::Error::other("line too long"));
        }
    }
    String::from_utf8(out).map_err(io::Error::other)
}

pub enum Frame {
    Input(Vec<u8>),
    Resize(u16, u16),
}

pub fn write_input(w: &mut impl Write, b: &[u8]) -> io::Result<()> {
    let mut f = vec![0];
    f.extend((b.len() as u32).to_le_bytes());
    f.extend(b);
    w.write_all(&f)
}

pub fn write_resize(w: &mut impl Write, cols: u16, rows: u16) -> io::Result<()> {
    let mut f = vec![1];
    f.extend(cols.to_le_bytes());
    f.extend(rows.to_le_bytes());
    w.write_all(&f)
}

pub fn read_frame(r: &mut impl Read) -> io::Result<Frame> {
    let mut t = [0u8];
    r.read_exact(&mut t)?;
    match t[0] {
        0 => {
            let mut n = [0u8; 4];
            r.read_exact(&mut n)?;
            let mut b = vec![0; u32::from_le_bytes(n) as usize];
            r.read_exact(&mut b)?;
            Ok(Frame::Input(b))
        }
        1 => {
            let mut s = [0u8; 4];
            r.read_exact(&mut s)?;
            Ok(Frame::Resize(u16::from_le_bytes([s[0], s[1]]), u16::from_le_bytes([s[2], s[3]])))
        }
        _ => Err(io::Error::other("bad frame")),
    }
}

/// An SGR mouse report (`ESC [ < b ; x ; y M|m`), with 0-based screen coordinates.
#[derive(Debug, PartialEq, Clone, Copy)]
pub struct Mouse {
    pub button: u16,
    pub x: u16,
    pub y: u16,
    pub release: bool,
    pub press: bool,
    /// -1 up, 1 down, 0 not a wheel event.
    pub wheel: i32,
}

impl Mouse {
    /// The same report with pane-relative coordinates.
    pub fn encode(&self, x: u16, y: u16) -> Vec<u8> {
        format!("\x1b[<{};{};{}{}", self.button, x + 1, y + 1, if self.release { 'm' } else { 'M' }).into_bytes()
    }
}

/// Every report in `b`, or None if `b` holds anything else.
pub fn parse_mouse(b: &[u8]) -> Option<Vec<Mouse>> {
    let s = std::str::from_utf8(b).ok()?;
    let mut out = vec![];
    for part in s.split("\x1b[<").skip(1) {
        let end = part.find(['M', 'm'])?;
        let mut n = part[..end].split(';').map(|v| v.parse::<u16>().ok());
        let (button, x, y) = (n.next()??, n.next()??, n.next()??);
        let release = &part[end..end + 1] == "m";
        let wheel = match button & !0b11100 {
            64 => -1,
            65 => 1,
            _ => 0,
        };
        let press = !release && wheel == 0 && button & 32 == 0;
        out.push(Mouse { button, x: x.saturating_sub(1), y: y.saturating_sub(1), release, press, wheel });
    }
    (!out.is_empty() && s.starts_with("\x1b[<")).then_some(out)
}

/// Bracketed-paste markers, for a pane that didn't ask for them.
pub fn strip_paste_marks(b: &[u8]) -> Vec<u8> {
    let s = String::from_utf8_lossy(b);
    if !s.contains("\x1b[200~") {
        return b.to_vec();
    }
    s.replace("\x1b[200~", "").replace("\x1b[201~", "").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_reports_parse_and_reencode() {
        let m = parse_mouse(b"\x1b[<0;12;5M").unwrap();
        assert_eq!(m, [Mouse { button: 0, x: 11, y: 4, release: false, press: true, wheel: 0 }]);
        assert_eq!(m[0].encode(1, 2), b"\x1b[<0;2;3M");
        assert_eq!(parse_mouse(b"\x1b[<65;1;1M").unwrap()[0].wheel, 1);
        assert!(!parse_mouse(b"\x1b[<32;3;3M").unwrap()[0].press, "drag is not a press");
        assert_eq!(parse_mouse(b"hello"), None);
        assert_eq!(parse_mouse(b"x\x1b[<0;1;1M"), None);
    }

    #[test]
    fn frames_round_trip() {
        let mut v = vec![];
        write_input(&mut v, b"hi").unwrap();
        write_resize(&mut v, 80, 24).unwrap();
        let mut r = v.as_slice();
        assert!(matches!(read_frame(&mut r).unwrap(), Frame::Input(b) if b == b"hi"));
        assert!(matches!(read_frame(&mut r).unwrap(), Frame::Resize(80, 24)));
        assert_eq!(read_line(&b"{\"a\":1}\nrest"[..]).unwrap(), "{\"a\":1}");
    }

    #[test]
    fn paste_marks_strip() {
        assert_eq!(strip_paste_marks(b"\x1b[200~hi\x1b[201~"), b"hi");
        assert_eq!(strip_paste_marks(b"plain"), b"plain");
    }
}
