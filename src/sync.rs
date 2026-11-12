//! Optional two-way sync with a Todoist project. The board owns stages; Todoist owns new
//! tasks (Up next) and queue order. Nothing here runs unless `todoist = true`.
use crate::config::Config;
use crate::model::Stage;
use crate::store::{NewTask, Store};
use crate::todoist;
use crate::todoist::Task as Remote_;
use std::collections::{HashMap, HashSet};
use anyhow::Result;
use std::sync::mpsc::Sender;
use std::time::Duration;

pub struct Sections {
    pub up_next: String,
    pub in_progress: String,
    pub needs_review: String,
    pub parked: String,
}

/// The one project the sync talks to; `todoist::Client` is the real one.
pub trait Remote {
    fn tasks(&self) -> Result<Vec<Remote_>>;
    fn create_task(&self, title: &str, description: &str, labels: &[String], section_id: Option<&str>) -> Result<String>;
    fn move_task(&self, id: &str, section_id: &str) -> Result<()>;
    fn complete(&self, id: &str) -> Result<()>;
    fn add_comment(&self, id: &str, text: &str) -> Result<()>;
}

#[derive(Debug, Default, PartialEq)]
pub struct SyncReport {
    pub imported: usize,
    pub pushed: usize,
    /// Tasks with a todoist_id that is gone remotely (completed or deleted there): not recreated.
    pub missing: usize,
}

fn section_for<'a>(stage: Stage, s: &'a Sections) -> Option<&'a str> {
    match stage {
        Stage::Queue => Some(&s.up_next),
        Stage::Active => Some(&s.in_progress),
        Stage::Review => Some(&s.needs_review),
        Stage::Parked | Stage::Shelved => Some(&s.parked),
        Stage::Done | Stage::Dropped => None,
    }
}

/// Stops at the first remote error; `synced_stage` only moves after the matching call
/// succeeded, so the next run retries. Every store write is its own statement: no
/// transaction is open during a network call.
pub fn sync_once(store: &Store, remote: &dyn Remote, sec: &Sections) -> Result<SyncReport> {
    let mut rep = SyncReport::default();
    let mut rtasks = remote.tasks()?;
    rtasks.sort_by_key(|t| t.child_order);
    let local = store.tasks()?;
    let first_run = local.iter().all(|t| t.todoist_id.is_none());
    let known: HashSet<&str> = local.iter().filter_map(|t| t.todoist_id.as_deref()).collect();

    for r in rtasks.iter().filter(|r| !known.contains(r.id.as_str())) {
        let s = r.section_id.as_deref();
        let (stage, synced) = if s == Some(&sec.up_next) {
            (Stage::Queue, Some("queue"))
        } else if first_run && (s == Some(&sec.in_progress) || s == Some(&sec.needs_review)) {
            (Stage::Parked, None) // not through the gate: resume and wrap up under the cap
        } else {
            continue;
        };
        let t = NewTask { title: &r.content, description: &r.description, dir: None, labels: &r.labels, todoist_id: Some(&r.id) };
        store.add_imported(t, stage, synced)?;
        rep.imported += 1;
    }

    // Synced queue tasks follow Todoist's order; local-only ones keep theirs, after.
    let order: HashMap<&str, i64> =
        rtasks.iter().filter(|r| r.section_id.as_deref() == Some(&sec.up_next)).map(|r| (r.id.as_str(), r.child_order)).collect();
    let local = store.tasks()?;
    let (mut synced, rest): (Vec<_>, Vec<_>) = local
        .iter()
        .filter(|t| t.stage == Stage::Queue)
        .partition(|t| t.todoist_id.as_deref().is_some_and(|id| order.contains_key(id)));
    synced.sort_by_key(|t| order[t.todoist_id.as_deref().unwrap_or_default()]);
    for (i, t) in synced.iter().chain(&rest).enumerate() {
        if t.position != (i + 1) as f64 {
            store.set_position(t.id, (i + 1) as f64)?;
        }
    }

    let remote_section: HashMap<&str, Option<&str>> = rtasks.iter().map(|r| (r.id.as_str(), r.section_id.as_deref())).collect();
    for t in store.tasks()?.into_iter().filter(|t| t.kind != "review") {
        let want = section_for(t.stage, sec);
        let behind = t.synced_stage.as_deref() != Some(t.stage.as_str());
        let (rid, in_place) = match t.todoist_id.clone() {
            None if want.is_none() => continue,
            None => {
                let id = remote.create_task(&t.title, &t.description, &t.labels, want)?;
                store.set_todoist_id(t.id, &id)?;
                (id, true)
            }
            Some(id) => match remote_section.get(id.as_str()) {
                None => {
                    rep.missing += usize::from(behind);
                    continue;
                }
                // The remote section is not trusted: the board's stage wins.
                Some(&cur) if !behind && cur == want => continue,
                Some(&cur) => (id, cur == want),
            },
        };
        match want {
            None => remote.complete(&rid)?,
            Some(s) if !in_place => remote.move_task(&rid, s)?,
            Some(_) => {}
        }
        if t.stage == Stage::Review && t.synced_stage.as_deref() != Some("review") {
            remote.add_comment(&rid, &review_comment(store, &t)?)?;
        }
        store.set_synced_stage(t.id, t.stage)?;
        rep.pushed += 1;
    }
    Ok(rep)
}

fn review_comment(store: &Store, t: &crate::model::Task) -> Result<String> {
    let mut lines = vec!["In review".to_string()];
    lines.extend(store.prs(t.id)?.into_iter().filter_map(|p| p.url).map(|u| format!("PR: {u}")));
    lines.extend(t.explainer.as_ref().map(|e| format!("Explainer: {e}")));
    Ok(lines.join("\n"))
}

/// Starts the 30 s sync thread; false (and nothing read or started) unless `todoist = true`.
/// Errors go to the footer as `(0, Some(text))`, only when the text changes.
pub fn spawn(cfg: &Config, tx: Sender<(i64, Option<String>)>) -> bool {
    if !cfg.todoist {
        return false;
    }
    std::thread::spawn(move || {
        let cfg = Config::from_env();
        let mut conn: Option<(todoist::Client, Sections)> = None;
        let mut last: Option<String> = None;
        loop {
            let r = (|| -> Result<()> {
                let store = Store::open(&cfg.db_path())?;
                if conn.is_none() {
                    conn = Some(todoist::connect(&cfg)?);
                }
                let (c, s) = conn.as_ref().expect("connected above");
                sync_once(&store, c, s).map(drop)
            })();
            let msg = r.err().map(|e| format!("Todoist sync: {e:#}"));
            if msg != last {
                let _ = tx.send((0, msg.clone()));
                last = msg;
            }
            std::thread::sleep(Duration::from_secs(30));
        }
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Stage::*;
    use std::cell::RefCell;

    const U: &str = "U";
    const I: &str = "I";
    const N: &str = "N";
    const P: &str = "P";

    fn sections() -> Sections {
        Sections { up_next: U.into(), in_progress: I.into(), needs_review: N.into(), parked: P.into() }
    }

    #[derive(Default)]
    struct Fake {
        tasks: RefCell<Vec<Remote_>>,
        calls: RefCell<Vec<String>>,
        fail_on: RefCell<Option<&'static str>>,
    }

    impl Fake {
        fn with(tasks: &[(&str, &str, i64)]) -> Fake {
            let f = Fake::default();
            for (id, sec, order) in tasks {
                f.tasks.borrow_mut().push(Remote_ {
                    id: id.to_string(),
                    content: format!("title {id}"),
                    description: format!("desc {id}"),
                    labels: vec!["l".into()],
                    section_id: Some(sec.to_string()),
                    child_order: *order,
                });
            }
            f
        }
        fn log(&self, what: &'static str, line: String) -> Result<()> {
            if *self.fail_on.borrow() == Some(what) {
                anyhow::bail!("boom");
            }
            self.calls.borrow_mut().push(line);
            Ok(())
        }
        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
        fn section_of(&self, id: &str) -> Option<String> {
            self.tasks.borrow().iter().find(|t| t.id == id).and_then(|t| t.section_id.clone())
        }
    }

    impl Remote for Fake {
        fn tasks(&self) -> Result<Vec<Remote_>> {
            Ok(self.tasks.borrow().clone())
        }
        fn create_task(&self, title: &str, description: &str, labels: &[String], section: Option<&str>) -> Result<String> {
            let id = format!("new{}", self.tasks.borrow().len());
            self.log("create", format!("create {id} {title:?} {description:?} {labels:?} {section:?}"))?;
            let order = self.tasks.borrow().len() as i64;
            self.tasks.borrow_mut().push(Remote_ {
                id: id.clone(),
                content: title.into(),
                description: description.into(),
                labels: labels.to_vec(),
                section_id: section.map(String::from),
                child_order: order,
            });
            Ok(id)
        }
        fn move_task(&self, id: &str, section: &str) -> Result<()> {
            self.log("move", format!("move {id} {section}"))?;
            self.tasks.borrow_mut().iter_mut().find(|t| t.id == id).unwrap().section_id = Some(section.into());
            Ok(())
        }
        fn complete(&self, id: &str) -> Result<()> {
            self.log("complete", format!("complete {id}"))?;
            self.tasks.borrow_mut().retain(|t| t.id != id);
            Ok(())
        }
        fn add_comment(&self, id: &str, text: &str) -> Result<()> {
            self.log("comment", format!("comment {id} {text}"))
        }
    }

    fn by_remote(s: &Store, rid: &str) -> crate::model::Task {
        s.tasks().unwrap().into_iter().find(|t| t.todoist_id.as_deref() == Some(rid)).unwrap()
    }

    fn queue(s: &Store) -> Vec<String> {
        s.tasks().unwrap().into_iter().filter(|t| t.stage == Queue).map(|t| t.title).collect()
    }

    /// A board that has synced before: one Active task linked to remote "a".
    fn synced_board() -> (Store, Fake) {
        let s = Store::open_in_memory();
        s.add_imported(NewTask { title: "A", description: "", dir: None, labels: &[], todoist_id: Some("a") }, Active, Some("active")).unwrap();
        (s, Fake::with(&[("a", I, 1)]))
    }

    #[test]
    fn pull_adds_new_up_next_tasks() {
        let (s, f) = synced_board();
        f.tasks.borrow_mut().push(Remote_ { section_id: Some(U.into()), id: "n".into(), content: "From phone".into(), description: "d".into(), labels: vec!["x".into()], child_order: 2 });
        f.tasks.borrow_mut().push(Remote_ { section_id: Some(I.into()), id: "z".into(), content: "Stray".into(), description: "".into(), labels: vec![], child_order: 3 });
        let r = sync_once(&s, &f, &sections()).unwrap();
        assert_eq!(r.imported, 1);
        let t = by_remote(&s, "n");
        assert_eq!((t.title.as_str(), t.description.as_str(), t.labels.clone(), t.stage), ("From phone", "d", vec!["x".to_string()], Queue));
        assert_eq!(t.synced_stage.as_deref(), Some("queue"));
        assert_eq!(s.tasks().unwrap().len(), 2, "a stray in another section is ignored after the first run");
        assert!(f.calls().is_empty(), "{:?}", f.calls());
        sync_once(&s, &f, &sections()).unwrap();
        assert_eq!(s.tasks().unwrap().len(), 2, "no duplicate on the next run");
    }

    #[test]
    fn queue_order_follows_todoist() {
        let s = Store::open_in_memory();
        let f = Fake::with(&[("r1", U, 3), ("r2", U, 1), ("r3", U, 2)]);
        sync_once(&s, &f, &sections()).unwrap();
        assert_eq!(queue(&s), ["title r2", "title r3", "title r1"]);
        let local = s.add_task(NewTask { title: "local", description: "", dir: None, labels: &[], todoist_id: None }).unwrap();
        f.tasks.borrow_mut()[0].child_order = 0; // phone moves r1 to the top
        sync_once(&s, &f, &sections()).unwrap();
        assert_eq!(by_remote(&s, "r1").position < by_remote(&s, "r2").position, true);
        assert_eq!(queue(&s)[..3], ["title r1", "title r2", "title r3"]);
        // the local task was pushed in the same run, so it now has an id but is still last
        assert_eq!(queue(&s)[3], "local");
        assert!(s.task(local).unwrap().unwrap().todoist_id.is_some());
    }

    #[test]
    fn first_run_imports_in_progress_as_parked() {
        let s = Store::open_in_memory();
        let f = Fake::with(&[("a", I, 1), ("b", N, 2), ("c", U, 3), ("d", P, 4)]);
        let r = sync_once(&s, &f, &sections()).unwrap();
        assert_eq!(r.imported, 3);
        assert_eq!(by_remote(&s, "a").stage, Parked);
        assert_eq!(by_remote(&s, "b").stage, Parked);
        assert_eq!(by_remote(&s, "c").stage, Queue);
        // the push moved the two imports to "Parked" and recorded it
        assert_eq!(f.section_of("a").as_deref(), Some(P));
        assert_eq!(f.section_of("b").as_deref(), Some(P));
        assert_eq!(by_remote(&s, "a").synced_stage.as_deref(), Some("parked"));
        assert_eq!(s.tasks().unwrap().len(), 3);
    }

    #[test]
    fn later_remote_stage_change_is_ignored_and_corrected() {
        let (s, f) = synced_board();
        s.set_synced_stage(by_remote(&s, "a").id, Active).unwrap();
        f.tasks.borrow_mut()[0].section_id = Some(U.into()); // phone drags it back to Up next
        let r = sync_once(&s, &f, &sections()).unwrap();
        assert_eq!(r.imported, 0);
        assert_eq!(by_remote(&s, "a").stage, Active);
        assert_eq!(f.section_of("a").as_deref(), Some(I));
        assert_eq!(f.calls(), ["move a I"]);
        sync_once(&s, &f, &sections()).unwrap();
        assert_eq!(f.calls().len(), 1, "settled");
    }

    #[test]
    fn push_moves_and_completes() {
        let (s, f) = synced_board();
        let a = by_remote(&s, "a").id;
        s.conn.execute("UPDATE tasks SET stage='parked' WHERE id=?1", [a]).unwrap();
        let q = s.add_task(NewTask { title: "Fresh", description: "why", dir: None, labels: &["l".into()], todoist_id: None }).unwrap();
        let gone = s.add_imported(NewTask { title: "Old", description: "", dir: None, labels: &[], todoist_id: Some("o") }, Active, Some("active")).unwrap();
        f.tasks.borrow_mut().push(Remote_ { id: "o".into(), content: "Old".into(), description: "".into(), labels: vec![], section_id: Some(I.into()), child_order: 5 });
        s.conn.execute("UPDATE tasks SET stage='dropped' WHERE id=?1", [gone]).unwrap();
        let r = sync_once(&s, &f, &sections()).unwrap();
        assert_eq!(r.pushed, 3);
        assert_eq!(f.section_of("a").as_deref(), Some(P));
        assert_eq!(f.section_of("o"), None, "completed");
        let t = s.task(q).unwrap().unwrap();
        assert_eq!(f.section_of(t.todoist_id.as_deref().unwrap()).as_deref(), Some(U));
        assert!(f.calls().contains(&"create new1 \"Fresh\" \"why\" [\"l\"] Some(\"U\")".to_string()), "{:?}", f.calls());
        assert_eq!(s.task(gone).unwrap().unwrap().synced_stage.as_deref(), Some("dropped"));
        let n = f.calls().len();
        sync_once(&s, &f, &sections()).unwrap();
        assert_eq!(f.calls().len(), n, "second run changes nothing");
    }

    #[test]
    fn reviews_are_never_pushed_and_shelved_goes_to_parked() {
        let (s, f) = synced_board();
        let a = by_remote(&s, "a").id;
        s.conn.execute("UPDATE tasks SET stage='shelved' WHERE id=?1", [a]).unwrap();
        let mk = |stage: &str| {
            let id = s.add_task(NewTask { title: "PR", description: "", dir: None, labels: &[], todoist_id: None }).unwrap();
            s.set_kind(id, "review", Some(&format!("https://github.com/o/r/pull/{id}"))).unwrap();
            s.conn.execute("UPDATE tasks SET stage=?2 WHERE id=?1", rusqlite::params![id, stage]).unwrap();
            id
        };
        let (q, d) = (mk("queue"), mk("done"));
        let r = sync_once(&s, &f, &sections()).unwrap();
        assert_eq!(r.pushed, 1, "only the shelved task");
        assert_eq!(f.section_of("a").as_deref(), Some(P));
        assert_eq!(f.calls(), ["move a P"], "no create/move/complete for reviews");
        assert!(s.task(q).unwrap().unwrap().todoist_id.is_none() && s.task(d).unwrap().unwrap().todoist_id.is_none());
    }

    #[test]
    fn review_gets_one_comment() {
        let (s, f) = synced_board();
        let a = by_remote(&s, "a").id;
        s.set_pr(a, "/r/repo", Some("https://github.com/o/repo/pull/7"), None).unwrap();
        s.conn.execute("UPDATE tasks SET stage='review', explainer='/x/a.html' WHERE id=?1", [a]).unwrap();
        sync_once(&s, &f, &sections()).unwrap();
        sync_once(&s, &f, &sections()).unwrap();
        let comments: Vec<_> = f.calls().into_iter().filter(|c| c.starts_with("comment")).collect();
        assert_eq!(comments.len(), 1, "{comments:?}");
        assert!(comments[0].starts_with("comment a ") && comments[0].contains("pull/7") && comments[0].contains("/x/a.html"));
        assert_eq!(f.section_of("a").as_deref(), Some(N));
    }

    #[test]
    fn errors_stop_the_run_and_retry_next_time() {
        let (s, f) = synced_board();
        let a = by_remote(&s, "a").id;
        s.conn.execute("UPDATE tasks SET stage='parked' WHERE id=?1", [a]).unwrap();
        *f.fail_on.borrow_mut() = Some("move");
        assert!(sync_once(&s, &f, &sections()).is_err());
        assert_eq!(s.task(a).unwrap().unwrap().synced_stage.as_deref(), Some("active"));
        *f.fail_on.borrow_mut() = None;
        sync_once(&s, &f, &sections()).unwrap();
        assert_eq!(f.section_of("a").as_deref(), Some(P));
    }

    #[test]
    fn task_gone_remotely_is_left_alone_and_counted() {
        let (s, f) = synced_board();
        let a = by_remote(&s, "a").id;
        f.tasks.borrow_mut().clear(); // completed on the phone
        assert_eq!(sync_once(&s, &f, &sections()).unwrap(), SyncReport::default());
        s.conn.execute("UPDATE tasks SET stage='parked' WHERE id=?1", [a]).unwrap();
        let r = sync_once(&s, &f, &sections()).unwrap();
        assert_eq!((r.pushed, r.missing), (0, 1));
        assert!(f.calls().is_empty());
        assert_eq!(s.task(a).unwrap().unwrap().stage, Parked);
    }

    #[test]
    fn sync_disabled_makes_no_calls() {
        let mut cfg = crate::app::tests::cfg(3);
        cfg.todoist = false;
        cfg.token_file = "/nonexistent/never-read".into();
        let (tx, rx) = std::sync::mpsc::channel();
        assert!(!spawn(&cfg, tx));
        assert!(rx.recv().is_err(), "no thread holds the sender");
    }
}
