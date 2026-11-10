use crate::model::{Pr, PrState, Refusal, Stage, Task};
use crate::store::Store;

/// url → does the PR exist; production runs `gh pr view <url>`.
pub type PrVerifier<'a> = dyn Fn(&str) -> bool + 'a;

pub fn basename(root: &str) -> &str {
    root.trim_end_matches('/').rsplit('/').next().unwrap_or(root)
}

fn waived(p: &Pr) -> bool {
    p.waiver.as_deref().is_some_and(|w| !w.trim().is_empty())
}

/// What is missing before the task may enter Review; empty means ready.
pub fn evaluate(store: &Store, task: &Task, checks: &[String], verify_pr: &PrVerifier) -> Vec<String> {
    let mut missing = vec![];
    for check in checks {
        match check.as_str() {
            "pr" => {
                // A read failure must refuse, never pass.
                let (prs, roots) = match (store.prs(task.id), store.task_repos(task.id)) {
                    (Ok(p), Ok(r)) => (p, r),
                    (Err(e), _) | (_, Err(e)) => {
                        missing.push(format!("couldn't read PRs: {e}"));
                        continue;
                    }
                };
                for root in roots {
                    let name = basename(&root);
                    match prs.iter().find(|p| p.root == root) {
                        Some(p) if waived(p) => {}
                        Some(Pr { url: Some(u), .. }) => {
                            if !verify_pr(u) {
                                missing.push(format!("couldn't verify PR {u}"));
                            }
                        }
                        _ => missing.push(format!("no PR for {name}")),
                    }
                }
            }
            "explainer" => {
                if !task.explainer.as_deref().is_some_and(|p| std::path::Path::new(p).exists()) {
                    missing.push("no explainer".into());
                }
            }
            other => missing.push(format!("unknown check {other}")),
        }
    }
    missing
}

/// The worst state across repos; waived rows count as merged.
/// The one way into Review. `verify_pr` does network I/O, so it runs on every PR url before the
/// transaction opens; the gate inside only checks membership and the write lock is held for local work.
/// A PR recorded after the pre-check is not in the set and fails closed.
pub fn to_review(store: &Store, id: i64, max_active: usize, checks: &[String], verify_pr: &PrVerifier) -> anyhow::Result<Result<(), Refusal>> {
    let verified: Vec<String> = store.prs(id)?.into_iter().filter(|p| !waived(p)).filter_map(|p| p.url).filter(|u| verify_pr(u)).collect();
    store.transition(id, Stage::Review, max_active, &|s, t| evaluate(s, t, checks, &|u| verified.iter().any(|v| v == u)))
}

pub fn worst(prs: &[Pr]) -> Option<PrState> {
    prs.iter().map(|p| if waived(p) { PrState::Merged } else { p.state }).max_by_key(|s| s.badness())
}

pub fn ready_to_close(prs: &[Pr]) -> bool {
    worst(prs).is_none_or(|s| s == PrState::Merged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::AgentState;
    use crate::store::{NewTask, SessionUpdate};

    fn setup() -> (Store, i64) {
        let s = Store::open_in_memory();
        let id = s.add_task(NewTask { title: "t", description: "", dir: None, labels: &[], todoist_id: None }).unwrap();
        s.link_session("s1", id, "/a").unwrap();
        s.mark_edited("s1", "/x/a", "main").unwrap();
        (s, id)
    }
    fn checks() -> Vec<String> {
        vec!["pr".into(), "explainer".into()]
    }
    fn eval(s: &Store, id: i64, ok: bool) -> Vec<String> {
        evaluate(s, &s.task(id).unwrap().unwrap(), &checks(), &move |_| ok)
    }
    fn explainer(s: &Store, id: i64) {
        let p = std::env::temp_dir().join(format!("cb-gate-{}.html", std::process::id()));
        std::fs::write(&p, "x").unwrap();
        s.set_explainer(id, p.to_str().unwrap()).unwrap();
    }
    fn pr(state: PrState, waiver: Option<&str>) -> Pr {
        Pr { task_id: 1, root: "/r".into(), url: None, waiver: waiver.map(Into::into), state, reviewers: String::new() }
    }

    #[test]
    fn gate_passes_with_pr_and_explainer() {
        let (s, id) = setup();
        s.set_pr(id, "/x/a", Some("https://x/pr/1"), None).unwrap();
        explainer(&s, id);
        assert!(eval(&s, id, true).is_empty());
    }

    #[test]
    fn gate_names_each_missing_piece() {
        let (s, id) = setup();
        s.link_session("s2", id, "/b").unwrap();
        s.mark_edited("s2", "/x/b", "main").unwrap();
        s.set_pr(id, "/x/a", Some("https://x/pr/1"), None).unwrap();
        assert_eq!(eval(&s, id, true), ["no PR for b", "no explainer"]);
    }

    #[test]
    fn waiver_stands_in_for_a_pr() {
        let (s, id) = setup();
        s.set_pr(id, "/x/a", None, Some("docs only")).unwrap();
        explainer(&s, id);
        assert!(eval(&s, id, false).is_empty());
    }

    #[test]
    fn to_review_verifies_before_taking_the_write_lock() {
        let path = std::env::temp_dir().join(format!("cb-gate-lock-{}/board.db", std::process::id()));
        let s = Store::open(&path).unwrap();
        let id = s.add_task(NewTask { title: "t", description: "", dir: None, labels: &[], todoist_id: None }).unwrap();
        s.link_session("s1", id, "/a").unwrap();
        s.mark_edited("s1", "/x/a", "main").unwrap();
        s.set_pr(id, "/x/a", Some("https://x/pr/1"), None).unwrap();
        s.conn.execute("UPDATE tasks SET stage='active'", []).unwrap();
        // If the verifier ran inside BEGIN IMMEDIATE, this second connection's write would time out.
        let p2 = path.clone();
        let verify = move |_: &str| {
            let other = Store::open_with_timeout(&p2, 50).unwrap();
            other.set_pr(id, "/x/b", None, Some("probe")).is_ok()
        };
        assert_eq!(to_review(&s, id, 3, &["pr".to_string()], &verify).unwrap(), Ok(()));
        // An unverified PR still refuses.
        s.conn.execute("UPDATE tasks SET stage='active'", []).unwrap();
        let r = to_review(&s, id, 3, &["pr".to_string()], &|_| false).unwrap();
        assert!(matches!(r, Err(Refusal::Gate(m)) if m == ["couldn't verify PR https://x/pr/1"]));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn gate_refuses_unverifiable_pr() {
        let (s, id) = setup();
        s.set_pr(id, "/x/a", Some("https://x/pr/1"), None).unwrap();
        explainer(&s, id);
        assert_eq!(eval(&s, id, false), ["couldn't verify PR https://x/pr/1"]);
    }

    #[test]
    fn store_read_error_fails_closed() {
        let (s, id) = setup();
        s.conn.execute("DROP TABLE repos", []).unwrap();
        let r = eval(&s, id, true);
        assert!(r[0].starts_with("couldn't read PRs:"), "{r:?}");
    }

    #[test]
    fn unknown_check_fails_closed() {
        let (s, id) = setup();
        let r = evaluate(&s, &s.task(id).unwrap().unwrap(), &["prr".into()], &|_| true);
        assert_eq!(r, ["unknown check prr"]);
    }

    #[test]
    fn worst_state_across_repos() {
        let a = [pr(PrState::Approved, None), pr(PrState::ChangesRequested, None)];
        assert_eq!(worst(&a), Some(PrState::ChangesRequested));
        assert!(!ready_to_close(&a));
        let b = [pr(PrState::Merged, None), pr(PrState::Waiting, Some("no code"))];
        assert_eq!(worst(&b), Some(PrState::Merged));
        assert!(ready_to_close(&b));
        assert_eq!(worst(&[]), None);
    }

    #[test]
    fn review_to_done_needs_merge() {
        let (s, id) = setup();
        s.conn.execute("UPDATE tasks SET stage='review'", []).unwrap();
        s.set_pr(id, "/x/a", Some("u"), None).unwrap();
        s.set_pr_state(id, "/x/a", PrState::Waiting, "").unwrap();
        assert_eq!(s.transition(id, Stage::Done, 3, &|_, _| vec![]).unwrap(), Err(Refusal::NotMerged));
        assert!(s.set_pr_state(id, "/x/a", PrState::Merged, "").unwrap());
        assert!(!s.set_pr_state(id, "/x/a", PrState::Merged, "").unwrap());
        assert_eq!(s.transition(id, Stage::Done, 3, &|_, _| vec![]).unwrap(), Ok(()));
        assert_eq!(s.events(id).iter().flatten().filter(|e| e.kind == "pr").count(), 2);
    }

    #[test]
    fn state_at_only_moves_on_change() {
        let s = Store::open_in_memory();
        let u = |state| SessionUpdate { id: "s", cwd: "/c", state, zellij_session: None, zellij_pane: None };
        assert!(!s.touch_session(u(AgentState::Working), false).unwrap());
        assert!(s.touch_session(u(AgentState::Working), true).unwrap());
        s.conn.execute("UPDATE sessions SET state_at=5", []).unwrap();
        s.touch_session(u(AgentState::Working), false).unwrap();
        assert_eq!(s.session("s").unwrap().unwrap().state_at, 5);
        s.touch_session(u(AgentState::Waiting), false).unwrap();
        assert!(s.session("s").unwrap().unwrap().state_at > 5);
    }

    #[test]
    fn a_pane_belongs_to_its_latest_reporter() {
        let s = Store::open_in_memory();
        let u = |id, zs, zp| SessionUpdate { id, cwd: "/c", state: AgentState::Waiting, zellij_session: zs, zellij_pane: zp };
        s.touch_session(u("old", Some("z"), Some("terminal_2")), true).unwrap();
        s.touch_session(u("other", Some("y"), Some("terminal_2")), true).unwrap();
        s.touch_session(u("new", Some("z"), Some("terminal_2")), true).unwrap();
        let pane = |id| {
            let x = s.session(id).unwrap().unwrap();
            (x.zellij_session, x.zellij_pane)
        };
        assert_eq!(pane("old"), (None, None));
        assert_eq!(pane("other"), (Some("y".into()), Some("terminal_2".into())));
        assert_eq!(pane("new"), (Some("z".into()), Some("terminal_2".into())));
        // An event without zellij info doesn't take the pane back.
        s.touch_session(u("old", None, None), false).unwrap();
        assert_eq!(pane("new").1.as_deref(), Some("terminal_2"));
    }

    #[test]
    fn untracked_lists_only_edited_unlinked_live_sessions() {
        let s = Store::open_in_memory();
        for n in ["edited", "plain", "ended", "pen"] {
            s.touch_session(SessionUpdate { id: n, cwd: "/c", state: AgentState::Working, zellij_session: None, zellij_pane: None }, true).unwrap();
        }
        s.mark_edited("edited", "/r", "m").unwrap();
        s.mark_edited("ended", "/r", "m").unwrap();
        s.touch_session(SessionUpdate { id: "ended", cwd: "/c", state: AgentState::Ended, zellij_session: None, zellij_pane: None }, false).unwrap();
        s.register_pen("pen", "/c").unwrap();
        s.mark_edited("pen", "/r", "m").unwrap();
        let ids: Vec<_> = s.untracked().unwrap().into_iter().map(|x| x.id).collect();
        assert_eq!(ids, ["edited"]);
    }
}
