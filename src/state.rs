use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AgentState {
    Working,
    #[default]
    Waiting,
    Blocked,
    Ended,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Task,
    Pen,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Repo {
    pub root: String,
    #[serde(rename = "ref")]
    pub ref_: String,
}

// Legacy files predate agent tracking, hence the field defaults.
#[derive(Debug, Clone, Deserialize)]
pub struct AgentRecord {
    pub session: String,
    pub cwd: String,
    #[serde(default)]
    pub task: Option<String>,
    #[serde(default)]
    pub kind: Kind,
    #[serde(default)]
    pub agent: AgentState,
    #[serde(default)]
    pub state_at: i64,
    #[serde(default)]
    pub zellij_session: Option<String>,
    #[serde(default)]
    pub zellij_pane: Option<String>,
    #[serde(default)]
    pub repos: Vec<Repo>,
}

pub fn read_dir(dir: &Path) -> Vec<AgentRecord> {
    let Ok(entries) = std::fs::read_dir(dir) else { return vec![] };
    entries
        .flatten()
        .filter(|e| {
            let n = e.file_name();
            let n = n.to_string_lossy();
            n.ends_with(".json") && !n.starts_with("board-")
        })
        .filter_map(|e| serde_json::from_slice(&std::fs::read(e.path()).ok()?).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("cb-state-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn read_dir_parses_records() {
        let d = tmp("parse");
        std::fs::write(
            d.join("s1.json"),
            r#"{"session":"s1","cwd":"/x","task":"t1","repos":[{"root":"/r","ref":"main"}],"dirty":true,
"agent":"blocked","state_at":42,"zellij_session":"z","zellij_pane":"terminal_3","kind":"pen","extra":1}"#,
        )
        .unwrap();
        let r = read_dir(&d);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].agent, AgentState::Blocked);
        assert_eq!(r[0].kind, Kind::Pen);
        assert_eq!(r[0].state_at, 42);
        assert_eq!(r[0].task.as_deref(), Some("t1"));
        assert_eq!(r[0].zellij_pane.as_deref(), Some("terminal_3"));
        assert_eq!(r[0].repos[0].ref_, "main");
    }

    #[test]
    fn bad_state_file_is_skipped() {
        let d = tmp("bad");
        std::fs::write(d.join("ok.json"), r#"{"session":"a","cwd":"/x"}"#).unwrap();
        std::fs::write(d.join("trunc.json"), r#"{"session":"#).unwrap();
        std::fs::write(d.join("nocwd.json"), r#"{"session":"b"}"#).unwrap();
        std::fs::write(d.join("board-abc.json"), r#"{"session":"c","cwd":"/x"}"#).unwrap();
        std::fs::write(d.join("ok.json.lock"), r#"{"session":"d","cwd":"/x"}"#).unwrap();
        assert_eq!(read_dir(&d).len(), 1);
    }

    #[test]
    fn legacy_record_gets_defaults() {
        let d = tmp("legacy");
        std::fs::write(d.join("old.json"), r#"{"session":"o","cwd":"/x","task":"t"}"#).unwrap();
        let r = read_dir(&d);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].agent, AgentState::Waiting);
        assert_eq!(r[0].kind, Kind::Task);
        assert_eq!(r[0].state_at, 0);
    }
}
