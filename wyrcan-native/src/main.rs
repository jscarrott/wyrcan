//! wyrcan (spike): the board's own terminal workspace, no zellij.
//!
//! - `wyrcan [--session S]`: attach, starting the session's server first if it isn't running.
//!   Closing the window only detaches; the server and its panes keep running.
//! - `wyrcan pipe [--plugin _] --name CMD [--args pane=terminal_N] [-- PAYLOAD]`: what the board calls
//!   (same syntax as the zellij plugin, so the board runs unchanged). Extra commands: `dump`, `kill`.
//! - `wyrcan --server S`: the server itself (started for you).
mod pane;
mod proto;
mod server;

use anyhow::{bail, Context, Result};
use ratatui::crossterm::{cursor, event, execute, terminal};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::time::Duration;

fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut session = std::env::var("ZELLIJ_SESSION_NAME").ok().filter(|_| inside()).unwrap_or_else(|| "board".into());
    if let Some(i) = args.iter().position(|a| a == "--session") {
        session = args.get(i + 1).context("--session needs a name")?.clone();
        args.drain(i..i + 2);
    }
    match args.first().map(String::as_str) {
        Some("--server") => server::run(args.get(1).context("--server needs a session name")?),
        Some("pipe") => std::process::exit(pipe(&session, &args[1..])?),
        Some(c @ ("dump" | "kill")) => std::process::exit(call(&session, c, BTreeMap::new(), None)?),
        None => attach(&session),
        Some(other) => bail!("unknown command {other}"),
    }
}

fn inside() -> bool {
    std::env::var_os("WYRCAN").is_some() && std::env::var_os("ZELLIJ").is_some()
}

/// `pipe --name N [--args k=v,...] [-- payload]`; `--plugin` is accepted and ignored.
fn pipe(session: &str, a: &[String]) -> Result<i32> {
    let (opts, payload) = match a.iter().position(|x| x == "--") {
        Some(i) => (&a[..i], Some(a[i + 1..].join(" "))),
        None => (a, None),
    };
    let get = |k: &str| opts.iter().position(|x| x == k).and_then(|i| opts.get(i + 1));
    let name = get("--name").context("pipe needs --name")?.clone();
    let args = get("--args")
        .map(|s| s.split(',').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect())
        .unwrap_or_default();
    call(session, &name, args, payload)
}

/// Sends one command, prints the reply as it arrives; exit 1 if it was an error.
fn call(session: &str, name: &str, args: BTreeMap<String, String>, payload: Option<String>) -> Result<i32> {
    let mut s = UnixStream::connect(proto::socket(session)).with_context(|| format!("no wyrcan session {session}"))?;
    let hello = proto::Hello::Pipe { name: name.into(), args, payload };
    s.write_all(format!("{}\n", serde_json::to_string(&hello)?).as_bytes())?;
    let mut code = 0;
    let mut out = std::io::stdout();
    let mut buf = [0u8; 8192];
    let mut first = true;
    loop {
        let n = s.read(&mut buf)?;
        if n == 0 {
            return Ok(code);
        }
        if first && buf[..n].starts_with(b"error:") {
            code = 1;
        }
        first = false;
        out.write_all(&buf[..n])?;
        out.flush()?;
    }
}

fn start_server(session: &str) -> Result<()> {
    let path = proto::socket(session);
    std::fs::create_dir_all(path.parent().unwrap())?;
    let log = std::fs::File::create(path.with_extension("log"))?;
    let mut cmd = std::process::Command::new(std::env::current_exe()?);
    cmd.args(["--server", session]).stdin(std::process::Stdio::null()).stdout(log.try_clone()?).stderr(log);
    // Its own session: closing the window (SIGHUP) must not take the panes with it.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn()?;
    for _ in 0..50 {
        if UnixStream::connect(&path).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    bail!("the wyrcan server didn't start; see {}", path.with_extension("log").display())
}

const MODES_ON: &str = "\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[?2004h";
const MODES_OFF: &str = "\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?2004l\x1b[0 q";

fn attach(session: &str) -> Result<()> {
    if inside() {
        bail!("already inside wyrcan");
    }
    let path = proto::socket(session);
    if UnixStream::connect(&path).is_err() {
        start_server(session)?;
    }
    let mut s = UnixStream::connect(&path)?;
    let (cols, rows) = terminal::size()?;
    s.write_all(format!("{}\n", serde_json::to_string(&proto::Hello::Attach { cols, rows })?).as_bytes())?;

    terminal::enable_raw_mode()?;
    let mut out = std::io::stdout();
    execute!(out, terminal::EnterAlternateScreen)?;
    out.write_all(MODES_ON.as_bytes())?;
    out.flush()?;

    let mut tx = s.try_clone()?;
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut buf = [0u8; 4096];
        while let Ok(n) = stdin.read(&mut buf) {
            if n == 0 || proto::write_input(&mut tx, &buf[..n]).is_err() {
                break;
            }
        }
    });
    let mut rs = s.try_clone()?;
    std::thread::spawn(move || {
        let mut last = (cols, rows);
        loop {
            std::thread::sleep(Duration::from_millis(200));
            if let Ok(now) = terminal::size() {
                if now != last && proto::write_resize(&mut rs, now.0, now.1).is_err() {
                    break;
                }
                last = now;
            }
        }
    });
    // Server output straight to the terminal until it hangs up (detach, takeover, or the session ended).
    let _ = std::io::copy(&mut s, &mut out);

    out.write_all(MODES_OFF.as_bytes())?;
    execute!(out, terminal::LeaveAlternateScreen, cursor::Show, event::DisableMouseCapture)?;
    terminal::disable_raw_mode()?;
    let alive = UnixStream::connect(&path).is_ok();
    println!("{}", if alive { format!("detached from {session}; run `wyrcan` to come back") } else { format!("session {session} ended") });
    // The stdin thread is blocked on a read; don't wait for it.
    std::process::exit(0)
}
