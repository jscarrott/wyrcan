//! Pane control through wyrcan's bundled plugin (`wyrcan/plugin`): one `wyrcan pipe` call per action,
//! plus a long-lived `events` pipe that reports the live panes whenever they change.
use crate::model::Session;
use anyhow::{bail, Context, Result};
use std::io::BufRead;
use std::process::{Command, Stdio};

pub trait Runner {
    /// Runs the command and returns its stdout.
    fn run(&self, cmd: &[String]) -> Result<String>;
}

pub struct ShellRunner;

impl Runner for ShellRunner {
    fn run(&self, cmd: &[String]) -> Result<String> {
        let (prog, args) = cmd.split_first().context("empty command")?;
        // A null stdin: `wyrcan pipe` would otherwise wait for more input on a piped stdin.
        let out = Command::new(prog).args(args).stdin(Stdio::null()).output().with_context(|| format!("running {prog}"))?;
        if !out.status.success() {
            // The plugin reports errors on stdout.
            let (e, o) = (String::from_utf8_lossy(&out.stderr), String::from_utf8_lossy(&out.stdout));
            bail!("{}", if e.trim().is_empty() { o.trim() } else { e.trim() });
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

pub struct Zellij {
    pub session: Option<String>,
    pub board_pane: Option<String>,
    /// The wyrcan binary the board runs in, from `$WYRCAN`.
    pub bin: String,
}

fn pipe_cmd(bin: &str, name: &str, pane: Option<&str>, payload: Option<&str>) -> Vec<String> {
    let mut v: Vec<String> = [bin, "pipe", "--plugin", "zellij:wyrcan", "--name", name].iter().map(|s| s.to_string()).collect();
    if let Some(p) = pane {
        v.extend(["--args".into(), format!("pane={p}")]);
    }
    if let Some(p) = payload {
        v.extend(["--".into(), p.into()]);
    }
    v
}

impl Zellij {
    /// Inside wyrcan only: stock zellij has no plugin to talk to.
    pub fn from_env() -> Zellij {
        let bin = std::env::var("WYRCAN").unwrap_or_default();
        let inside = std::env::var_os("ZELLIJ").is_some() && !bin.is_empty();
        let var = |k| std::env::var(k).ok().filter(|_| inside);
        Zellij { session: var("ZELLIJ_SESSION_NAME"), board_pane: var("ZELLIJ_PANE_ID").map(|n| format!("terminal_{n}")), bin }
    }

    /// The board's pane; errors "not inside wyrcan" when there's none.
    pub fn board(&self) -> Result<&str> {
        match (&self.session, &self.board_pane) {
            (Some(_), Some(b)) => Ok(b),
            _ => bail!("not inside wyrcan (start the board with `wyrcan`)"),
        }
    }

    fn pipe(&self, r: &dyn Runner, name: &str, pane: Option<&str>, payload: Option<&str>) -> Result<String> {
        self.board()?;
        Ok(r.run(&pipe_cmd(&self.bin, name, pane, payload))?.trim().to_string())
    }

    /// A session's pane, only if it's in this zellij session and still exists.
    pub fn pane_of<'a>(&self, s: &'a Session, live: &[String]) -> Option<&'a str> {
        let pane = s.zellij_pane.as_deref()?;
        (s.zellij_session.is_some() && s.zellij_session == self.session && live.iter().any(|l| l == pane)).then_some(pane)
    }

    /// Opens Claude in a pane beside the board (joining `stack` if any) without taking focus.
    pub fn open(
        &self,
        r: &dyn Runner,
        name: &str,
        cwd: &str,
        claude_dir: &str,
        claude_args: &[&str],
        stack: &[String],
    ) -> Result<String> {
        let mut cmd = vec!["env".to_string(), format!("CLAUDE_CONFIG_DIR={claude_dir}"), "claude".to_string()];
        cmd.extend(claude_args.iter().map(|s| s.to_string()));
        self.open_with(r, name, cwd, &cmd, stack)
    }

    fn open_with(&self, r: &dyn Runner, name: &str, cwd: &str, argv: &[String], stack: &[String]) -> Result<String> {
        let name: String = name.chars().take(30).collect();
        let payload = serde_json::json!({ "name": name, "cwd": cwd, "argv": argv, "stack": stack }).to_string();
        let id = self.pipe(r, "open", None, Some(&payload))?;
        if !id.starts_with("terminal_") {
            bail!("wyrcan opened no pane: {id}");
        }
        Ok(id)
    }

    pub fn show(&self, r: &dyn Runner, pane: &str) -> Result<()> {
        self.enter(r, pane)?;
        self.enter(r, self.board()?)
    }

    pub fn enter(&self, r: &dyn Runner, pane: &str) -> Result<()> {
        self.pipe(r, "focus", Some(pane), None).map(drop)
    }

    pub fn fullscreen(&self, r: &dyn Runner, pane: &str) -> Result<()> {
        self.enter(r, pane)?;
        self.pipe(r, "fullscreen", Some(pane), None).map(drop)
    }

    pub fn toggle_board_fullscreen(&self, r: &dyn Runner) -> Result<()> {
        self.pipe(r, "fullscreen", Some(self.board()?), None).map(drop)
    }

    pub fn close(&self, r: &dyn Runner, pane: &str) -> Result<()> {
        self.pipe(r, "close", Some(pane), None).map(drop)
    }

    /// Types `text` and Enter into the pane; focus doesn't move.
    pub fn type_line(&self, r: &dyn Runner, pane: &str, text: &str) -> Result<()> {
        self.pipe(r, "write", Some(pane), Some(text)).map(drop)
    }
}

/// `panes terminal_0 terminal_3` -> the live pane ids.
fn parse_panes(line: &str) -> Option<Vec<String>> {
    let mut words = line.split_whitespace();
    (words.next() == Some("panes")).then(|| words.map(String::from).collect())
}

/// Sends the live panes each time they change, for as long as the board runs.
/// A dropped stream (say, the plugin reloaded) is reopened after a second.
pub fn spawn_events(bin: String, tx: std::sync::mpsc::Sender<Vec<String>>) {
    std::thread::spawn(move || loop {
        let child = Command::new(&bin)
            .args(&pipe_cmd(&bin, "events", None, None)[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        if let Ok(mut child) = child {
            let out = child.stdout.take().expect("piped above");
            for line in std::io::BufReader::new(out).lines() {
                let Ok(line) = line else { break };
                if let Some(live) = parse_panes(&line) {
                    if tx.send(live).is_err() {
                        let _ = child.kill();
                        return;
                    }
                }
            }
            let _ = child.wait();
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AgentState as MAgent, Session, SessionKind};
    use std::cell::RefCell;

    #[derive(Default)]
    struct Rec(RefCell<Vec<Vec<String>>>, String);
    impl Runner for Rec {
        fn run(&self, cmd: &[String]) -> Result<String> {
            self.0.borrow_mut().push(cmd.to_vec());
            Ok(self.1.clone())
        }
    }
    impl Rec {
        fn calls(&self) -> Vec<String> {
            self.0.borrow().iter().map(|c| c.join(" ")).collect()
        }
    }

    const P: &str = "wyrcan pipe --plugin zellij:wyrcan --name";

    fn zj() -> Zellij {
        Zellij { session: Some("s".into()), board_pane: Some("terminal_0".into()), bin: "wyrcan".into() }
    }

    fn sess(zs: Option<&str>, pane: Option<&str>) -> Session {
        Session {
            id: "x".into(),
            task_id: None,
            kind: SessionKind::Task,
            cwd: "/".into(),
            state: MAgent::Working,
            state_at: 0,
            zellij_session: zs.map(Into::into),
            zellij_pane: pane.map(Into::into),
            edited: false,
            current: false,
        }
    }

    #[test]
    fn open_sends_argv_and_stack_as_json() {
        let r = Rec(Default::default(), "terminal_5\n".into());
        let stack = ["terminal_1".to_string(), "terminal_2".to_string()];
        let id = zj().open(&r, &"n".repeat(40), "/w", "/c", &["/next 3"], &stack).unwrap();
        assert_eq!(id, "terminal_5");
        let calls = r.0.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0][..7].join(" "), format!("{P} open --"));
        let v: serde_json::Value = serde_json::from_str(&calls[0][7]).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"name": "n".repeat(30), "cwd": "/w", "argv": ["env", "CLAUDE_CONFIG_DIR=/c", "claude", "/next 3"], "stack": stack})
        );
    }

    #[test]
    fn open_without_pane_id_fails() {
        let r = Rec(Default::default(), "error: zellij opened no pane\n".into());
        let e = zj().open(&r, "t", "/w", "/c", &[], &[]).unwrap_err();
        assert_eq!(e.to_string(), "wyrcan opened no pane: error: zellij opened no pane");
    }

    #[test]
    fn show_focuses_the_pane_then_the_board() {
        let r = Rec::default();
        zj().show(&r, "terminal_4").unwrap();
        assert_eq!(r.calls(), [format!("{P} focus --args pane=terminal_4"), format!("{P} focus --args pane=terminal_0")]);
    }

    #[test]
    fn type_line_writes_through_the_plugin() {
        let r = Rec::default();
        zj().type_line(&r, "terminal_1", "/wrap").unwrap();
        assert_eq!(r.calls(), [format!("{P} write --args pane=terminal_1 -- /wrap")]);
    }

    #[test]
    fn stale_pane_is_not_used() {
        let live = ["terminal_3".to_string()];
        let z = zj();
        assert_eq!(z.pane_of(&sess(Some("s"), Some("terminal_3")), &live), Some("terminal_3"));
        assert_eq!(z.pane_of(&sess(Some("o"), Some("terminal_3")), &live), None);
        assert_eq!(z.pane_of(&sess(Some("s"), Some("terminal_9")), &live), None);
        assert_eq!(z.pane_of(&sess(Some("s"), None), &live), None);
    }

    #[test]
    fn outside_wyrcan_is_refused() {
        let r = Rec::default();
        let z = Zellij { session: None, board_pane: None, bin: String::new() };
        assert!(z.show(&r, "terminal_1").unwrap_err().to_string().contains("not inside wyrcan"));
        assert!(r.calls().is_empty());
    }

    #[test]
    fn events_lines_parse() {
        assert_eq!(parse_panes("panes terminal_0 terminal_3"), Some(vec!["terminal_0".into(), "terminal_3".into()]));
        assert_eq!(parse_panes("panes"), Some(vec![]));
        assert_eq!(parse_panes("error: x"), None);
    }

    /// Hands-on check, run by a person in the board's pane inside wyrcan:
    /// cargo test stack_spike -- --ignored --nocapture
    #[test]
    #[ignore]
    fn stack_spike() {
        let z = Zellij::from_env();
        if z.board().is_err() {
            println!("stack_spike: not inside wyrcan, nothing done");
            return;
        }
        let r = ShellRunner;
        let mut stack = vec![];
        for n in ["spike-a", "spike-b"] {
            // open_with is what open() delegates to; it lets the spike run `cat` instead of claude.
            let id = z.open_with(&r, n, ".", &["cat".to_string()], &stack).unwrap();
            stack.push(id);
        }
        println!("opened {stack:?}");
        for p in &stack {
            z.show(&r, p).unwrap();
        }
        z.type_line(&r, &stack[0], "hello").unwrap();
        z.fullscreen(&r, &stack[1]).unwrap();
        println!("LOOK FOR: two stacked panes beside the board; 'hello' + Enter arrived in spike-a;");
        println!("spike-b is fullscreen; press Alt b to get back to the board.");
        println!("Clean up: close the spike-a / spike-b panes ({stack:?}) yourself.");
    }
}
