#![allow(dead_code)] // later workstream tasks use the rest of the store
use crate::model::{Event, Refusal, Stage, Task};
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::path::Path;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS tasks(
    id INTEGER PRIMARY KEY, title TEXT NOT NULL, description TEXT NOT NULL DEFAULT '',
    kind TEXT NOT NULL DEFAULT 'task', stage TEXT NOT NULL DEFAULT 'queue',
    position REAL NOT NULL DEFAULT 0, dir TEXT, labels TEXT NOT NULL DEFAULT '[]',
    explainer TEXT, todoist_id TEXT, created_at INTEGER NOT NULL, stage_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS sessions(
    id TEXT PRIMARY KEY, task_id INTEGER, kind TEXT NOT NULL DEFAULT 'task', cwd TEXT NOT NULL DEFAULT '',
    state TEXT NOT NULL DEFAULT 'working', state_at INTEGER NOT NULL DEFAULT 0,
    zellij_session TEXT, zellij_pane TEXT, edited INTEGER NOT NULL DEFAULT 0, current INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS repos(
    session_id TEXT NOT NULL, root TEXT NOT NULL, git_ref TEXT NOT NULL DEFAULT '', UNIQUE(session_id, root));
CREATE TABLE IF NOT EXISTS prs(
    task_id INTEGER NOT NULL, root TEXT NOT NULL, url TEXT, waiver TEXT,
    state TEXT NOT NULL DEFAULT 'unknown', reviewers TEXT NOT NULL DEFAULT '', UNIQUE(task_id, root));
CREATE TABLE IF NOT EXISTS events(
    id INTEGER PRIMARY KEY, task_id INTEGER NOT NULL, at INTEGER NOT NULL, kind TEXT NOT NULL, detail TEXT NOT NULL DEFAULT '');
PRAGMA user_version = 1;";

pub struct NewTask<'a> {
    pub title: &'a str,
    pub description: &'a str,
    pub dir: Option<&'a str>,
    pub labels: &'a [String],
    pub todoist_id: Option<&'a str>,
}

pub struct Store {
    pub(crate) conn: Connection,
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

const TASK_COLS: &str = "id,title,description,kind,stage,position,dir,labels,explainer,todoist_id,created_at,stage_at";

fn task_row(r: &rusqlite::Row) -> rusqlite::Result<Task> {
    let stage: String = r.get(4)?;
    let labels: String = r.get(7)?;
    Ok(Task {
        id: r.get(0)?,
        title: r.get(1)?,
        description: r.get(2)?,
        kind: r.get(3)?,
        stage: Stage::from_str(&stage).unwrap_or(Stage::Queue),
        position: r.get(5)?,
        dir: r.get(6)?,
        labels: serde_json::from_str(&labels).unwrap_or_default(),
        explainer: r.get(8)?,
        todoist_id: r.get(9)?,
        created_at: r.get(10)?,
        stage_at: r.get(11)?,
    })
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        Self::open_with_timeout(path, 2000)
    }

    pub fn open_with_timeout(path: &Path, ms: u64) -> Result<Store> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_millis(ms))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Store { conn })
    }

    pub fn open_in_memory() -> Store {
        let conn = Connection::open_in_memory().expect("in-memory sqlite");
        conn.execute_batch(SCHEMA).expect("schema");
        Store { conn }
    }

    pub fn add_task(&self, t: NewTask) -> Result<i64> {
        let n = now();
        self.conn.execute(
            "INSERT INTO tasks(title,description,dir,labels,todoist_id,position,created_at,stage_at)
             VALUES(?1,?2,?3,?4,?5,(SELECT COALESCE(MAX(position),0)+1 FROM tasks),?6,?6)",
            params![t.title, t.description, t.dir, serde_json::to_string(t.labels)?, t.todoist_id, n],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn task(&self, id: i64) -> Result<Option<Task>> {
        Ok(self.conn.query_row(&format!("SELECT {TASK_COLS} FROM tasks WHERE id=?1"), [id], task_row).optional()?)
    }

    pub fn tasks(&self) -> Result<Vec<Task>> {
        let mut q = self.conn.prepare(&format!(
            "SELECT {TASK_COLS} FROM tasks ORDER BY CASE WHEN stage='queue' THEN position END, stage_at DESC, id"
        ))?;
        let rows = q.query_map([], task_row)?.collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn update_task(&self, id: i64, title: &str, description: &str, dir: Option<&str>, labels: &[String]) -> Result<()> {
        self.conn.execute(
            "UPDATE tasks SET title=?1, description=?2, dir=?3, labels=?4 WHERE id=?5",
            params![title, description, dir, serde_json::to_string(labels)?, id],
        )?;
        Ok(())
    }

    pub fn set_position(&self, id: i64, position: f64) -> Result<()> {
        self.conn.execute("UPDATE tasks SET position=?1 WHERE id=?2", params![position, id])?;
        Ok(())
    }

    pub fn active_count(&self) -> Result<usize> {
        let n: i64 = self.conn.query_row("SELECT COUNT(*) FROM tasks WHERE stage='active'", [], |r| r.get(0))?;
        Ok(n as usize)
    }

    /// The only place a stage changes: checks the table, the active cap and the review gate.
    pub fn transition(
        &self,
        id: i64,
        to: Stage,
        max_active: usize,
        gate: &dyn Fn(&Store, &Task) -> Vec<String>,
    ) -> Result<Result<(), Refusal>> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let r = self.transition_inner(id, to, max_active, gate);
        // A refusal changes nothing, so rolling back is as good as committing.
        self.conn.execute_batch(if matches!(r, Ok(Ok(()))) { "COMMIT" } else { "ROLLBACK" })?;
        r
    }

    fn transition_inner(
        &self,
        id: i64,
        to: Stage,
        max_active: usize,
        gate: &dyn Fn(&Store, &Task) -> Vec<String>,
    ) -> Result<Result<(), Refusal>> {
        use Stage::*;
        let Some(task) = self.task(id)? else { return Ok(Err(Refusal::NoSuchTask)) };
        let from = task.stage;
        if !matches!(
            (from, to),
            (Queue, Active | Dropped)
                | (Active, Review | Parked | Queue | Dropped)
                | (Parked, Active | Queue | Dropped)
                | (Review, Active | Done | Dropped)
        ) {
            return Ok(Err(Refusal::NotAllowed { from, to }));
        }
        if to == Active && self.active_count()? >= max_active {
            return Ok(Err(Refusal::CapFull { max: max_active }));
        }
        if (from, to) == (Active, Review) {
            let missing = gate(self, &task);
            if !missing.is_empty() {
                return Ok(Err(Refusal::Gate(missing)));
            }
        }
        if to == Done {
            let open: i64 = self.conn.query_row(
                "SELECT COUNT(*) FROM prs WHERE task_id=?1 AND state!='merged' AND waiver IS NULL",
                [id],
                |r| r.get(0),
            )?;
            if open > 0 {
                return Ok(Err(Refusal::NotMerged));
            }
        }
        let n = now();
        // Back to the queue means the bottom of it.
        self.conn.execute(
            "UPDATE tasks SET stage=?1, stage_at=?2,
               position=CASE WHEN ?1='queue' THEN (SELECT COALESCE(MAX(position),0)+1 FROM tasks) ELSE position END
             WHERE id=?3",
            params![to.as_str(), n, id],
        )?;
        self.add_event(id, "stage", &format!("{from} → {to}"))?;
        Ok(Ok(()))
    }

    pub fn add_event(&self, task_id: i64, kind: &str, detail: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO events(task_id,at,kind,detail) VALUES(?1,?2,?3,?4)",
            params![task_id, now(), kind, detail],
        )?;
        Ok(())
    }

    pub fn events(&self, task_id: i64) -> Result<Vec<Event>> {
        let mut q = self.conn.prepare("SELECT task_id,at,kind,detail FROM events WHERE task_id=?1 ORDER BY id")?;
        let rows = q
            .query_map([task_id], |r| Ok(Event { task_id: r.get(0)?, at: r.get(1)?, kind: r.get(2)?, detail: r.get(3)? }))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn export_json(&self) -> Result<Value> {
        let mut out = serde_json::Map::new();
        for table in ["tasks", "sessions", "repos", "prs", "events"] {
            let mut q = self.conn.prepare(&format!("SELECT * FROM {table}"))?;
            let cols: Vec<String> = q.column_names().iter().map(|c| c.to_string()).collect();
            let rows = q
                .query_map([], |r| {
                    let mut o = serde_json::Map::new();
                    for (i, c) in cols.iter().enumerate() {
                        use rusqlite::types::ValueRef::*;
                        let v = match r.get_ref(i)? {
                            Null => Value::Null,
                            Integer(n) => json!(n),
                            Real(f) => json!(f),
                            Text(t) | Blob(t) => {
                                let s = String::from_utf8_lossy(t).into_owned();
                                if c == "labels" { serde_json::from_str(&s).unwrap_or(Value::String(s)) } else { Value::String(s) }
                            }
                        };
                        o.insert(c.clone(), v);
                    }
                    Ok(Value::Object(o))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            out.insert(table.into(), Value::Array(rows));
        }
        Ok(Value::Object(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nt(title: &str) -> NewTask<'_> {
        NewTask { title, description: "", dir: None, labels: &[], todoist_id: None }
    }
    fn no_gate(_: &Store, _: &Task) -> Vec<String> {
        vec![]
    }
    /// A task forced into `stage` directly, bypassing the table.
    fn task_in(s: &Store, stage: Stage) -> i64 {
        let id = s.add_task(nt("t")).unwrap();
        s.conn.execute("UPDATE tasks SET stage=?1 WHERE id=?2", rusqlite::params![stage.as_str(), id]).unwrap();
        id
    }
    fn stage_of(s: &Store, id: i64) -> Stage {
        s.task(id).unwrap().unwrap().stage
    }

    #[test]
    fn transitions_follow_the_table() {
        use Stage::*;
        let allowed = [
            (Queue, Active), (Queue, Dropped),
            (Active, Review), (Active, Parked), (Active, Queue), (Active, Dropped),
            (Parked, Active), (Parked, Queue), (Parked, Dropped),
            (Review, Active), (Review, Done), (Review, Dropped),
        ];
        let s = Store::open_in_memory();
        for &from in Stage::ALL {
            for &to in Stage::ALL {
                let id = task_in(&s, from);
                let r = s.transition(id, to, 99, &no_gate).unwrap();
                if allowed.contains(&(from, to)) {
                    assert_eq!(r, Ok(()), "{from} -> {to}");
                } else {
                    assert_eq!(r, Err(Refusal::NotAllowed { from, to }), "{from} -> {to}");
                }
            }
        }
    }

    #[test]
    fn review_to_done_needs_merged_or_waived_prs() {
        let s = Store::open_in_memory();
        let id = task_in(&s, Stage::Review);
        s.conn.execute("INSERT INTO prs(task_id,root,state) VALUES(?1,'/a','waiting')", [id]).unwrap();
        assert_eq!(s.transition(id, Stage::Done, 3, &no_gate).unwrap(), Err(Refusal::NotMerged));
        s.conn.execute("UPDATE prs SET waiver='no code'", []).unwrap();
        assert_eq!(s.transition(id, Stage::Done, 3, &no_gate).unwrap(), Ok(()));
    }

    #[test]
    fn every_way_into_active_checks_the_cap() {
        let s = Store::open_in_memory();
        task_in(&s, Stage::Active);
        for from in [Stage::Queue, Stage::Parked, Stage::Review] {
            let id = task_in(&s, from);
            assert_eq!(s.transition(id, Stage::Active, 1, &no_gate).unwrap(), Err(Refusal::CapFull { max: 1 }));
            assert_eq!(stage_of(&s, id), from);
        }
    }

    #[test]
    fn last_slot_goes_to_exactly_one() {
        let dir = std::env::temp_dir().join(format!("cb-store-{}", std::process::id()));
        let path = dir.join("board.db");
        let (a, b) = {
            let s = Store::open(&path).unwrap();
            (s.add_task(nt("a")).unwrap(), s.add_task(nt("b")).unwrap())
        };
        let hs: Vec<_> = [a, b]
            .into_iter()
            .map(|id| {
                let p = path.clone();
                std::thread::spawn(move || Store::open(&p).unwrap().transition(id, Stage::Active, 1, &no_gate).unwrap())
            })
            .collect();
        let rs: Vec<_> = hs.into_iter().map(|h| h.join().unwrap()).collect();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(rs.iter().filter(|r| **r == Ok(())).count(), 1);
        assert_eq!(rs.iter().filter(|r| **r == Err(Refusal::CapFull { max: 1 })).count(), 1);
    }

    #[test]
    fn review_needs_the_gate() {
        let s = Store::open_in_memory();
        let id = task_in(&s, Stage::Active);
        let r = s.transition(id, Stage::Review, 3, &|_, _| vec!["no explainer".into()]).unwrap();
        assert_eq!(r, Err(Refusal::Gate(vec!["no explainer".into()])));
        assert_eq!(stage_of(&s, id), Stage::Active);
    }

    #[test]
    fn stage_change_is_logged() {
        let s = Store::open_in_memory();
        let id = s.add_task(nt("t")).unwrap();
        s.transition(id, Stage::Active, 3, &no_gate).unwrap().unwrap();
        let ev = s.events(id).unwrap();
        assert_eq!(ev.len(), 1);
        assert_eq!((ev[0].kind.as_str(), ev[0].detail.as_str()), ("stage", "queue → active"));
    }

    #[test]
    fn queue_order_and_requeue() {
        let s = Store::open_in_memory();
        let ids: Vec<i64> = ["a", "b", "c"].iter().map(|t| s.add_task(nt(t)).unwrap()).collect();
        let order = |s: &Store| s.tasks().unwrap().iter().filter(|t| t.stage == Stage::Queue).map(|t| t.id).collect::<Vec<_>>();
        assert_eq!(order(&s), ids);
        s.set_position(ids[2], 0.5).unwrap();
        assert_eq!(order(&s), vec![ids[2], ids[0], ids[1]]);
        s.transition(ids[0], Stage::Active, 3, &no_gate).unwrap().unwrap();
        s.transition(ids[0], Stage::Queue, 3, &no_gate).unwrap().unwrap();
        assert_eq!(order(&s), vec![ids[2], ids[1], ids[0]]);
    }

    #[test]
    fn refusal_messages() {
        assert_eq!(Refusal::CapFull { max: 3 }.to_string(), "All 3 active slots are in use. Park or finish a stream first.");
        assert_eq!(Refusal::Gate(vec!["a".into(), "b".into()]).to_string(), "Not ready for review: a; b");
        assert_eq!(Refusal::NotAllowed { from: Stage::Done, to: Stage::Active }.to_string(), "Can't move from done to active.");
        assert_eq!(Refusal::NotMerged.to_string(), "Not merged yet. Drop it if you want to abandon it.");
    }

    #[test]
    fn export_has_every_table() {
        let s = Store::open_in_memory();
        s.add_task(NewTask { title: "x", description: "d", dir: Some("/d"), labels: &["l".into()], todoist_id: None }).unwrap();
        let v = s.export_json().unwrap();
        for k in ["tasks", "sessions", "repos", "prs", "events"] {
            assert!(v[k].is_array(), "{k}");
        }
        assert_eq!(v["tasks"][0]["labels"][0], "l");
    }
}
