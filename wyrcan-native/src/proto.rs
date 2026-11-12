//! What goes over a session's socket.
//!
//! A connection starts with one JSON line (`Hello`). An attached client then sends crossterm
//! events, one JSON line each, and reads raw terminal output. A pipe call reads reply lines
//! until the server closes the connection.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{self, Read};
use std::path::PathBuf;

#[derive(Serialize, Deserialize)]
pub enum Hello {
    Attach { cols: u16, rows: u16 },
    Pipe { name: String, args: BTreeMap<String, String>, payload: Option<String> },
}

pub fn dir() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("wyrcan-native")
}

pub fn socket(session: &str) -> PathBuf {
    dir().join(format!("{session}.sock"))
}

/// Session names with a socket file, sorted; the caller checks which are alive.
pub fn sessions() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.strip_suffix(".sock").map(String::from))
        .collect();
    v.sort();
    v
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_line_leaves_the_rest() {
        let mut r: &[u8] = b"{\"a\":1}\nrest";
        assert_eq!(read_line(&mut r).unwrap(), "{\"a\":1}");
        assert_eq!(r, b"rest");
    }
}
