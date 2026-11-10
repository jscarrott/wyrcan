use crate::state::{AgentRecord, AgentState};
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

pub fn new_tab_cmd(name: &str, cwd: &str, claude_args: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = ["zellij", "action", "new-tab", "-n", name, "-c", cwd, "--", "claude"]
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

fn spawn_claude_in(current: Option<&str>, name: &str, cwd: &str, args: &[&str]) -> Result<()> {
    if current.is_none() {
        bail!("not inside zellij");
    }
    run(&new_tab_cmd(name, cwd, args))?;
    run(&back_cmd())
}

pub fn spawn_claude(name: &str, cwd: &str, args: &[&str]) -> Result<()> {
    spawn_claude_in(current_session().as_deref(), name, cwd, args)
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
        let want: Vec<String> = ["zellij", "action", "new-tab", "-n", "next", "-c", "/home/x", "--", "claude", "/next"]
            .map(String::from)
            .to_vec();
        assert_eq!(new_tab_cmd("next", "/home/x", &["/next"]), want);
    }

    #[test]
    fn outside_zellij_is_refused() {
        let e = spawn_claude_in(None, "n", "/", &[]).unwrap_err();
        assert!(e.to_string().contains("not inside zellij"));
    }
}
