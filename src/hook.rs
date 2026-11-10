//! `claude-board hook <event>`: Claude Code hook handler. Never fails, never prints.
use crate::config::Config;
use crate::model::{AgentState, SessionKind};
use crate::store::{SessionUpdate, Store};
use anyhow::Result;
use serde_json::Value;
use std::io::Read;
use std::path::Path;
use std::process::Command;

pub fn run(event: &str, cfg: &Config) {
    // Read all of stdin first, or the writer gets SIGPIPE.
    let mut buf = String::new();
    let _ = std::io::stdin().read_to_string(&mut buf);
    let Ok(input) = serde_json::from_str::<Value>(&buf) else { return };
    let Ok(store) = Store::open_with_timeout(&cfg.db_path(), 200) else { return };
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let (zs, zp) = (var("ZELLIJ_SESSION_NAME"), var("ZELLIJ_PANE_ID").map(|p| format!("terminal_{p}")));
    let _ = apply(&store, event, &input, (zs.as_deref(), zp.as_deref()));
}

fn out(cmd: &mut Command) -> Option<String> {
    let o = cmd.output().ok().filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// (root, "jj <change>" | "git <branch>") of the jj/git repo containing `file`.
fn repo_info(file: &Path) -> Option<(String, String)> {
    let mut d = file.parent()?;
    while !d.is_dir() {
        d = d.parent()?;
    }
    if let Some(root) = out(Command::new("jj").args(["root", "--ignore-working-copy"]).current_dir(d)) {
        let ch = out(Command::new("jj").args(["log", "--ignore-working-copy", "--no-graph", "-r", "@", "-T", "change_id.short()"]).current_dir(&root));
        return Some((root, format!("jj {}", ch.unwrap_or_default())));
    }
    let root = out(Command::new("git").arg("-C").arg(d).args(["rev-parse", "--show-toplevel"]))?;
    let br = out(Command::new("git").arg("-C").arg(&root).args(["branch", "--show-current"]));
    Some((root, format!("git {}", br.unwrap_or_default())))
}

fn tail(s: &str, max: usize) -> &str {
    let mut i = s.len().saturating_sub(max);
    while !s.is_char_boundary(i) {
        i += 1;
    }
    &s[i..]
}

pub fn apply(store: &Store, event: &str, input: &Value, zellij: (Option<&str>, Option<&str>)) -> Result<()> {
    let Some(id) = input["session_id"].as_str().filter(|s| !s.is_empty()) else { return Ok(()) };
    let cwd = input["cwd"].as_str().unwrap_or("");
    let state = match event {
        "working" | "edit" => AgentState::Working,
        "waiting" => AgentState::Waiting,
        "blocked" => AgentState::Blocked,
        "ended" => AgentState::Ended,
        _ => return Ok(()),
    };
    let upd = SessionUpdate { id, cwd, state, zellij_session: zellij.0, zellij_pane: zellij.1 };
    if event == "edit" {
        if store.session(id)?.is_some_and(|s| s.kind == SessionKind::Pen) {
            return Ok(());
        }
        let ti = &input["tool_input"];
        let Some(file) = ti["file_path"].as_str().or(ti["notebook_path"].as_str()) else { return Ok(()) };
        let Some((root, git_ref)) = repo_info(Path::new(file)) else { return Ok(()) };
        store.touch_session(upd, true)?;
        return store.mark_edited(id, &root, &git_ref);
    }
    if !store.touch_session(upd, false)? {
        return Ok(());
    }
    if event == "waiting" {
        let msg = input["last_assistant_message"].as_str().unwrap_or("");
        if let (Some(task), false) = (store.session(id)?.and_then(|s| s.task_id), msg.is_empty()) {
            // Every Stop writes one; keeping only the newest stops them burying stage and PR events.
            store.replace_event(task, "turn", tail(msg, 1500))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn edit(store: &Store, sid: &str, file: &Path) {
        let i = json!({"session_id": sid, "cwd": "/w", "tool_input": {"file_path": file}});
        apply(store, "edit", &i, (Some("zs"), Some("terminal_4"))).unwrap();
    }

    /// A throwaway git repo with one file in it.
    fn repo(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("cb-hook-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        Command::new("git").arg("init").arg("-q").arg(&d).status().unwrap();
        d.join("f.rs")
    }

    #[test]
    fn first_edit_creates_untracked_session_with_repo() {
        let s = Store::open_in_memory();
        let f = repo("a");
        edit(&s, "s1", &f);
        let sess = s.session("s1").unwrap().unwrap();
        assert!(sess.edited && sess.task_id.is_none());
        assert_eq!(sess.zellij_pane.as_deref(), Some("terminal_4"));
        assert_eq!(s.untracked().unwrap().len(), 1);
        let root: String = s.conn.query_row("SELECT root FROM repos WHERE session_id='s1'", [], |r| r.get(0)).unwrap();
        assert_eq!(Path::new(&root).canonicalize().unwrap(), f.parent().unwrap().canonicalize().unwrap());
        std::fs::remove_dir_all(f.parent().unwrap()).unwrap();
    }

    #[test]
    fn second_repo_is_added_once() {
        let s = Store::open_in_memory();
        let (a, b) = (repo("b"), repo("c"));
        for f in [&a, &b, &a, &b] {
            edit(&s, "s1", f);
        }
        let n: i64 = s.conn.query_row("SELECT COUNT(*) FROM repos", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2);
        for f in [a, b] {
            std::fs::remove_dir_all(f.parent().unwrap()).unwrap();
        }
    }

    #[test]
    fn state_events_ignore_unknown_sessions() {
        let s = Store::open_in_memory();
        for e in ["working", "waiting", "blocked", "ended"] {
            apply(&s, e, &json!({"session_id": "x", "cwd": "/w"}), (None, None)).unwrap();
        }
        assert!(s.sessions().unwrap().is_empty());
    }

    #[test]
    fn state_events_update_existing_session() {
        let s = Store::open_in_memory();
        s.register_pen("p", "/w").unwrap();
        apply(&s, "blocked", &json!({"session_id": "p", "cwd": "/w"}), (None, None)).unwrap();
        assert_eq!(s.session("p").unwrap().unwrap().state, AgentState::Blocked);
    }

    #[test]
    fn stop_records_summary_for_linked_session() {
        let s = Store::open_in_memory();
        let t = s.add_task(crate::store::NewTask { title: "t", description: "", dir: None, labels: &[], todoist_id: None }).unwrap();
        s.link_session("lk", t, "/w").unwrap();
        s.touch_session(SessionUpdate { id: "free", cwd: "/w", state: AgentState::Working, zellij_session: None, zellij_pane: None }, true).unwrap();
        let long = "é".repeat(2000); // 4000 bytes
        for sid in ["lk", "free"] {
            apply(&s, "waiting", &json!({"session_id": sid, "cwd": "/w", "last_assistant_message": long}), (None, None)).unwrap();
        }
        let ev = s.events(t).unwrap();
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].kind, "turn");
        assert!(ev[0].detail.len() <= 1500 && !ev[0].detail.is_empty());
        let n: i64 = s.conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn only_the_latest_turn_summary_is_kept() {
        let s = Store::open_in_memory();
        let t = s.add_task(crate::store::NewTask { title: "t", description: "", dir: None, labels: &[], todoist_id: None }).unwrap();
        s.link_session("lk", t, "/w").unwrap();
        s.add_event(t, "summary", "wrap-up").unwrap();
        for m in ["first", "second"] {
            apply(&s, "waiting", &json!({"session_id": "lk", "cwd": "/w", "last_assistant_message": m}), (None, None)).unwrap();
        }
        let ev: Vec<(String, String)> = s.events(t).unwrap().into_iter().map(|e| (e.kind, e.detail)).collect();
        assert_eq!(ev, [("summary".into(), "wrap-up".into()), ("turn".into(), "second".into())]);
    }

    #[test]
    fn pen_session_ignores_edit() {
        let s = Store::open_in_memory();
        s.register_pen("p", "/w").unwrap();
        let f = repo("d");
        edit(&s, "p", &f);
        assert!(!s.session("p").unwrap().unwrap().edited);
        let n: i64 = s.conn.query_row("SELECT COUNT(*) FROM repos", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        std::fs::remove_dir_all(f.parent().unwrap()).unwrap();
    }
}
