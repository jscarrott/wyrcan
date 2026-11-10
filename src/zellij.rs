use crate::state::{AgentRecord, AgentState};
use crate::model::Session;
use anyhow::{bail, Context, Result};

#[derive(Debug, PartialEq)]
pub enum Jump {
    Focus(String),
    Resume { session: String, cwd: String },
    Elsewhere,
    NoAgent,
}

pub fn current_session() -> Option<String> {
    std::env::var_os("ZELLIJ")?;
    std::env::var("ZELLIJ_SESSION_NAME").ok()
}

pub fn jump_target(agent: Option<&AgentRecord>, current: Option<&str>) -> Jump {
    let Some(a) = agent else { return Jump::NoAgent };
    if a.agent == AgentState::Ended {
        return Jump::Resume { session: a.session.clone(), cwd: a.cwd.clone() };
    }
    match (&a.zellij_pane, &a.zellij_session) {
        (Some(p), Some(s)) if Some(s.as_str()) == current => Jump::Focus(p.clone()),
        _ => Jump::Elsewhere,
    }
}

/// The pane is started by the zellij server, not us, so the Claude dir is passed explicitly.
pub fn new_tab_cmd(name: &str, cwd: &str, claude_dir: &str, claude_args: &[&str]) -> Vec<String> {
    let dir = format!("CLAUDE_CONFIG_DIR={claude_dir}");
    let mut v: Vec<String> = ["zellij", "action", "new-tab", "-n", name, "-c", cwd, "--", "env", &dir, "claude"]
        .into_iter()
        .map(String::from)
        .collect();
    v.extend(claude_args.iter().map(|s| s.to_string()));
    v
}

pub fn focus_cmd(pane: &str) -> Vec<String> {
    ["zellij", "action", "focus-pane-id", pane].map(String::from).to_vec()
}

pub fn back_cmd() -> Vec<String> {
    ["zellij", "action", "go-to-previous-tab"].map(String::from).to_vec()
}

pub fn run(cmd: &[String]) -> Result<()> {
    let (prog, args) = cmd.split_first().context("empty command")?;
    let out = std::process::Command::new(prog).args(args).output().with_context(|| format!("running {prog}"))?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

fn spawn_claude_in(current: Option<&str>, name: &str, cwd: &str, claude_dir: &str, args: &[&str]) -> Result<()> {
    if current.is_none() {
        bail!("not inside zellij");
    }
    run(&new_tab_cmd(name, cwd, claude_dir, args))?;
    run(&back_cmd())
}

pub fn spawn_claude(name: &str, cwd: &str, claude_dir: &str, args: &[&str]) -> Result<()> {
    spawn_claude_in(current_session().as_deref(), name, cwd, claude_dir, args)
}

// The items below are used by Task 6 (the workstream board); the old TUI above still uses the older ones.

/// shortcut: unverified against a live zellij client; Task 10's run-through must check Alt b.
#[allow(dead_code)] // used by Task 6
pub const RETURN_KEYBIND: &str = r#"shared_except "locked" {
    bind "Alt b" { ToggleFocusFullscreen; MoveFocus "Left"; }
}"#;

#[allow(dead_code)] // used by Task 6
pub trait Runner {
    /// Runs the command and returns its stdout.
    fn run(&self, cmd: &[String]) -> Result<String>;
}

#[allow(dead_code)] // used by Task 6
pub struct ShellRunner;

impl Runner for ShellRunner {
    fn run(&self, cmd: &[String]) -> Result<String> {
        let (prog, args) = cmd.split_first().context("empty command")?;
        let out = std::process::Command::new(prog).args(args).output().with_context(|| format!("running {prog}"))?;
        if !out.status.success() {
            bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

#[allow(dead_code)] // used by Task 6
pub struct Zellij {
    pub session: Option<String>,
    pub board_pane: Option<String>,
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

#[allow(dead_code)] // used by Task 6
impl Zellij {
    pub fn from_env() -> Zellij {
        let inside = std::env::var_os("ZELLIJ").is_some();
        let var = |k| std::env::var(k).ok().filter(|_| inside);
        Zellij { session: var("ZELLIJ_SESSION_NAME"), board_pane: var("ZELLIJ_PANE_ID").map(|n| format!("terminal_{n}")) }
    }

    fn board(&self) -> Result<&str> {
        match (&self.session, &self.board_pane) {
            (Some(_), Some(b)) => Ok(b),
            _ => bail!("not inside zellij"),
        }
    }

    fn act(&self, r: &dyn Runner, parts: &[&str]) -> Result<String> {
        self.board()?;
        r.run(&[&["zellij", "action"][..], parts].concat().iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    pub fn live_panes(&self, r: &dyn Runner) -> Result<Vec<String>> {
        let out = self.act(r, &["list-panes"])?;
        Ok(out
            .lines()
            .filter_map(|l| l.split_whitespace().next())
            .filter(|id| id.starts_with("terminal_"))
            .map(String::from)
            .collect())
    }

    /// A session's pane, only if it's in this zellij session and still exists.
    pub fn pane_of<'a>(&self, s: &'a Session, live: &[String]) -> Option<&'a str> {
        let pane = s.zellij_pane.as_deref()?;
        (s.zellij_session.is_some() && s.zellij_session == self.session && live.iter().any(|l| l == pane)).then_some(pane)
    }

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

    fn open_with(&self, r: &dyn Runner, name: &str, cwd: &str, cmd: &[String], stack: &[String]) -> Result<String> {
        let board = self.board()?.to_string();
        let name: String = name.chars().take(30).collect();
        let mut run = argv(&["zellij", "run", "--name", &name, "--cwd", cwd, "--no-focus"]);
        if stack.is_empty() {
            run.extend(argv(&["--direction", "right"]));
        }
        run.push("--".into());
        run.extend_from_slice(cmd);
        let id = r.run(&run)?.trim().to_string();
        if !stack.is_empty() {
            let mut sp = argv(&["zellij", "action", "stack-panes", "--"]);
            sp.extend(stack.iter().cloned());
            sp.push(id.clone());
            r.run(&sp)?;
        }
        self.act(r, &["focus-pane-id", &board])?;
        Ok(id)
    }

    pub fn show(&self, r: &dyn Runner, pane: &str) -> Result<()> {
        self.enter(r, pane)?;
        self.act(r, &["focus-pane-id", self.board()?]).map(drop)
    }

    pub fn enter(&self, r: &dyn Runner, pane: &str) -> Result<()> {
        self.act(r, &["focus-pane-id", pane]).map(drop)
    }

    pub fn fullscreen(&self, r: &dyn Runner, pane: &str) -> Result<()> {
        self.enter(r, pane)?;
        self.act(r, &["toggle-fullscreen", "--pane-id", pane]).map(drop)
    }

    pub fn toggle_board_fullscreen(&self, r: &dyn Runner) -> Result<()> {
        self.act(r, &["toggle-fullscreen", "--pane-id", self.board()?]).map(drop)
    }

    pub fn close(&self, r: &dyn Runner, pane: &str) -> Result<()> {
        self.act(r, &["close-pane", "--pane-id", pane]).map(drop)
    }

    pub fn type_line(&self, r: &dyn Runner, pane: &str, text: &str) -> Result<()> {
        self.act(r, &["write-chars", "--pane-id", pane, text])?;
        self.act(r, &["write", "--pane-id", pane, "13"]).map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Kind;

    fn rec(agent: AgentState, session: Option<&str>, pane: Option<&str>) -> AgentRecord {
        AgentRecord {
            session: "sid".into(),
            cwd: "/w".into(),
            task: None,
            kind: Kind::Task,
            agent,
            state_at: 0,
            zellij_session: session.map(Into::into),
            zellij_pane: pane.map(Into::into),
            repos: vec![],
        }
    }

    #[test]
    fn jump_focus_same_session() {
        let a = rec(AgentState::Working, Some("s"), Some("terminal_3"));
        assert_eq!(jump_target(Some(&a), Some("s")), Jump::Focus("terminal_3".into()));
    }

    #[test]
    fn jump_elsewhere_other_session() {
        let a = rec(AgentState::Working, Some("s"), Some("terminal_3"));
        assert_eq!(jump_target(Some(&a), Some("o")), Jump::Elsewhere);
        assert_eq!(jump_target(Some(&a), None), Jump::Elsewhere);
    }

    #[test]
    fn jump_elsewhere_no_pane() {
        let a = rec(AgentState::Waiting, Some("s"), None);
        assert_eq!(jump_target(Some(&a), Some("s")), Jump::Elsewhere);
    }

    #[test]
    fn jump_resume_when_ended() {
        let a = rec(AgentState::Ended, Some("s"), Some("terminal_3"));
        assert_eq!(jump_target(Some(&a), Some("s")), Jump::Resume { session: "sid".into(), cwd: "/w".into() });
    }

    #[test]
    fn jump_no_agent() {
        assert_eq!(jump_target(None, Some("s")), Jump::NoAgent);
    }

    #[test]
    fn new_tab_cmd_shape() {
        let want: Vec<String> =
            ["zellij", "action", "new-tab", "-n", "next", "-c", "/home/x", "--", "env", "CLAUDE_CONFIG_DIR=/c", "claude", "/next"]
                .map(String::from)
                .to_vec();
        assert_eq!(new_tab_cmd("next", "/home/x", "/c", &["/next"]), want);
    }

    #[test]
    fn outside_zellij_is_refused() {
        let e = spawn_claude_in(None, "n", "/", "/c", &[]).unwrap_err();
        assert!(e.to_string().contains("not inside zellij"));
    }

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

    fn zj() -> Zellij {
        Zellij { session: Some("s".into()), board_pane: Some("terminal_0".into()) }
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
    fn open_first_pane_goes_right_and_refocuses_board() {
        let r = Rec(Default::default(), "terminal_1\n".into());
        let id = zj().open(&r, "t", "/w", "/c", &["/next"], &[]).unwrap();
        assert_eq!(id, "terminal_1");
        assert_eq!(
            r.calls(),
            [
                "zellij run --name t --cwd /w --no-focus --direction right -- env CLAUDE_CONFIG_DIR=/c claude /next",
                "zellij action focus-pane-id terminal_0",
            ]
        );
    }

    #[test]
    fn open_later_pane_joins_the_stack() {
        let r = Rec(Default::default(), "terminal_5".into());
        let stack = ["terminal_1".to_string(), "terminal_2".to_string()];
        zj().open(&r, &"n".repeat(40), "/w", "/c", &[], &stack).unwrap();
        let c = r.calls();
        assert_eq!(c[0], format!("zellij run --name {} --cwd /w --no-focus -- env CLAUDE_CONFIG_DIR=/c claude", "n".repeat(30)));
        assert_eq!(c[1], "zellij action stack-panes -- terminal_1 terminal_2 terminal_5");
        assert_eq!(c[2], "zellij action focus-pane-id terminal_0");
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
    fn live_panes_keeps_terminals_only() {
        let out = "PANE_ID  TYPE  TITLE\nplugin_1  plugin  status-bar\nterminal_0  terminal  Pane #1\nterminal_1  terminal  a\n";
        let r = Rec(Default::default(), out.into());
        assert_eq!(zj().live_panes(&r).unwrap(), ["terminal_0", "terminal_1"]);
    }

    #[test]
    fn stack_outside_zellij_is_refused() {
        let r = Rec::default();
        let z = Zellij { session: None, board_pane: None };
        assert!(z.show(&r, "terminal_1").unwrap_err().to_string().contains("not inside zellij"));
        assert!(z.live_panes(&r).is_err());
        assert!(r.calls().is_empty());
    }

    #[test]
    fn type_line_sends_text_then_enter() {
        let r = Rec::default();
        zj().type_line(&r, "terminal_1", "/wrap").unwrap();
        assert_eq!(
            r.calls(),
            ["zellij action write-chars --pane-id terminal_1 /wrap", "zellij action write --pane-id terminal_1 13"]
        );
    }

    /// Hands-on check, run by a person inside zellij:
    /// cargo test stack_spike -- --ignored --nocapture
    #[test]
    #[ignore]
    fn stack_spike() {
        let z = Zellij::from_env();
        if z.session.is_none() || z.board_pane.is_none() {
            println!("stack_spike: not inside zellij, nothing done");
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
        println!("spike-b is fullscreen; press Alt b (RETURN_KEYBIND) to get back to the board.");
        println!("Clean up: close the spike-a / spike-b panes ({stack:?}) yourself.");
    }
}
