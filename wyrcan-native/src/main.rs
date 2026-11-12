//! wyrcan (spike): the board's own terminal workspace, no zellij.
//!
//! - `wyrcan [--session S]`: attach, starting the session's server first if it isn't running
//!   (through systemd when the user unit is installed). Closing the window only detaches.
//! - `wyrcan ls`: sessions, with their panes and whether a client is attached.
//! - `wyrcan service install|uninstall`: the `wyrcan@.service` systemd user unit.
//! - `wyrcan pipe [--plugin _] --name CMD [--args pane=terminal_N] [-- PAYLOAD]`: what the board calls
//!   (same syntax as the zellij plugin, so the board runs unchanged). Also `wyrcan dump|kill`.
//! - `wyrcan --server S`: the server itself.
mod pane;
mod proto;
mod server;

use anyhow::{bail, Context, Result};
use crossterm::event::{
    self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste, EnableFocusChange, EnableMouseCapture,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::{cursor, execute, terminal};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
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
        Some(c @ ("dump" | "kill")) => std::process::exit(call(&session, c, BTreeMap::new(), None, &mut std::io::stdout())?),
        Some("ls") => ls(),
        Some("service") => service(args.get(1).map(String::as_str)),
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
    call(session, &name, args, payload, &mut std::io::stdout())
}

/// Sends one command and copies the reply to `out` as it arrives; exit 1 if it was an error.
fn call(session: &str, name: &str, args: BTreeMap<String, String>, payload: Option<String>, out: &mut dyn Write) -> Result<i32> {
    let mut s = UnixStream::connect(proto::socket(session)).with_context(|| format!("no wyrcan session {session}"))?;
    let hello = proto::Hello::Pipe { name: name.into(), args, payload };
    s.write_all(format!("{}\n", serde_json::to_string(&hello)?).as_bytes())?;
    let (mut code, mut first) = (0, true);
    let mut buf = [0u8; 8192];
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

fn ls() -> Result<()> {
    let mut any = false;
    for name in proto::sessions() {
        let mut info = vec![];
        if call(&name, "info", BTreeMap::new(), None, &mut info).is_ok() {
            println!("{name}\t{}", String::from_utf8_lossy(&info).trim());
            any = true;
        } else {
            // The server is gone and left its socket behind.
            let _ = std::fs::remove_file(proto::socket(&name));
        }
    }
    if !any {
        println!("no wyrcan sessions");
    }
    Ok(())
}

fn unit_path() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    Ok(base.context("no HOME")?.join("systemd/user/wyrcan@.service"))
}

/// The unit runs one session per instance (`wyrcan@board`). systemd starts it with a bare environment,
/// so PATH (claude-board, claude, git, gh) and the ssh agent socket are copied in at install time.
fn unit(exe: &str, path: &str, ssh: Option<&str>) -> String {
    let ssh = ssh.map(|s| format!("Environment=SSH_AUTH_SOCK={s}\n")).unwrap_or_default();
    format!(
        "# Written by `wyrcan service install`; run it again after moving wyrcan or changing PATH.\n\
         [Unit]\nDescription=wyrcan session %i (Claude Board workspace)\n\n\
         [Service]\nType=simple\nExecStart={exe} --server %i\nEnvironment=PATH={path}\n{ssh}\
         Restart=on-failure\nRestartSec=2\n\n\
         [Install]\nWantedBy=default.target\n"
    )
}

fn systemctl(args: &[&str]) -> Result<()> {
    let ok = std::process::Command::new("systemctl").arg("--user").args(args).status()?.success();
    if !ok {
        bail!("systemctl --user {} failed", args.join(" "));
    }
    Ok(())
}

fn service(cmd: Option<&str>) -> Result<()> {
    let path = unit_path()?;
    match cmd {
        Some("install") => {
            let exe = std::env::current_exe()?.to_string_lossy().into_owned();
            let text = unit(&exe, &std::env::var("PATH").unwrap_or_default(), std::env::var("SSH_AUTH_SOCK").ok().as_deref());
            std::fs::create_dir_all(path.parent().unwrap())?;
            std::fs::write(&path, text)?;
            systemctl(&["daemon-reload"])?;
            println!("wrote {}", path.display());
            println!("`wyrcan` now starts sessions through it. To start the board session at login:");
            println!("  systemctl --user enable --now wyrcan@board");
            println!("To keep sessions running after you log out:  loginctl enable-linger $USER");
        }
        Some("uninstall") => {
            let _ = std::fs::remove_file(&path);
            systemctl(&["daemon-reload"])?;
            println!("removed {}; running sessions keep running (stop them with `wyrcan --session S kill`)", path.display());
        }
        _ => bail!("usage: wyrcan service install|uninstall"),
    }
    Ok(())
}

fn start_server(session: &str) -> Result<()> {
    let path = proto::socket(session);
    std::fs::create_dir_all(path.parent().unwrap())?;
    if unit_path().is_ok_and(|u| u.exists()) {
        systemctl(&["start", &format!("wyrcan@{session}")])?;
    } else {
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
    }
    for _ in 0..100 {
        if UnixStream::connect(&path).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    bail!("the wyrcan server didn't start; see {} or `journalctl --user -u wyrcan@{session}`", path.with_extension("log").display())
}

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
    execute!(out, terminal::EnterAlternateScreen, EnableMouseCapture, EnableBracketedPaste, EnableFocusChange)?;
    // Kitty keyboard protocol: Shift+Enter, Ctrl+I vs Tab etc. arrive as distinct keys,
    // and each pane gets them in whatever encoding its program asked for.
    let kitty = terminal::supports_keyboard_enhancement().unwrap_or(false);
    if kitty {
        execute!(out, PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES))?;
    }

    let done = Arc::new(AtomicBool::new(false));
    let mut from = s.try_clone()?;
    let flag = done.clone();
    // Server output straight to the terminal until it hangs up (detach, takeover, or the session ended).
    // Flushed per chunk: stdout is line-buffered, and screen updates rarely contain a newline.
    let copier = std::thread::spawn(move || {
        let mut buf = vec![0u8; 64 * 1024];
        let mut out = std::io::stdout().lock();
        while let Ok(n) = from.read(&mut buf) {
            if n == 0 || out.write_all(&buf[..n]).and_then(|_| out.flush()).is_err() {
                break;
            }
        }
        flag.store(true, Ordering::Relaxed);
    });
    while !done.load(Ordering::Relaxed) {
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let e = event::read()?;
        if s.write_all(format!("{}\n", serde_json::to_string(&e)?).as_bytes()).is_err() {
            break;
        }
    }
    let _ = copier.join();

    if kitty {
        execute!(out, PopKeyboardEnhancementFlags)?;
    }
    execute!(out, DisableFocusChange, DisableBracketedPaste, DisableMouseCapture, terminal::LeaveAlternateScreen, cursor::Show)?;
    out.write_all(b"\x1b[0 q")?;
    terminal::disable_raw_mode()?;
    let alive = UnixStream::connect(&path).is_ok();
    println!("{}", if alive { format!("detached from {session}; run `wyrcan` to come back") } else { format!("session {session} ended") });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_carries_path_and_instance() {
        let u = unit("/bin/wyrcan", "/a:/b", Some("/run/ssh"));
        assert!(u.contains("ExecStart=/bin/wyrcan --server %i\n"));
        assert!(u.contains("Environment=PATH=/a:/b\n") && u.contains("Environment=SSH_AUTH_SOCK=/run/ssh\n"));
        assert!(!unit("/w", "/p", None).contains("SSH_AUTH_SOCK"));
    }
}
