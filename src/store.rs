use crate::gate;
use crate::model::{AgentState, Event, Pr, PrState, Refusal, Session, SessionKind, Stage, Task};
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::path::Path;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS tasks(
    id INTEGER PRIMARY KEY, title TEXT NOT NULL, description TEXT NOT NULL DEFAULT '',
    kind TEXT NOT NULL DEFAULT 'default', stage TEXT NOT NULL DEFAULT 'queue',
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
    id INTEGER PRIMARY KEY, task_id INTEGER NOT NULL, at INTEGER NOT NULL, kind TEXT NOT NULL, detail TEXT NOT NULL DEFAULT '');";

/// Switching a file to WAL needs a lock the busy timeout doesn't reliably wait for, so retry
/// within the caller's budget; failing that, carry on in the current journal mode.
fn enable_wal(conn: &Connection, ms: u64) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    loop {
        let mode: rusqlite::Result<String> = conn.query_row("PRAGMA journal_mode", [], |r| r.get(0));
        if mode.is_ok_and(|m| m == "wal") || conn.pragma_update(None, "journal_mode", "WAL").is_ok() {
            return;
        }
        if std::time::Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Brings any database (new, or created at user_version 1 or 2) to version 3.
fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA)?;
    let version = |c: &Connection| c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0));
    if version(conn)? >= 3 {
        return Ok(());
    }
    conn.execute_batch("BEGIN IMMEDIATE")?;
    // Another process may have migrated while we waited for the lock.
    let r = (|| -> Result<()> {
        if version(conn)? < 3 {
            // An older binary resets user_version on every open, so trust the schema, not the number.
            for (col, ty) in [("synced_stage", "TEXT"), ("link", "TEXT")] {
                let has: i64 = conn.query_row("SELECT count(*) FROM pragma_table_info('tasks') WHERE name=?1", [col], |r| r.get(0))?;
                if has == 0 {
                    conn.execute_batch(&format!("ALTER TABLE tasks ADD COLUMN {col} {ty}"))?;
                }
            }
            conn.execute_batch(
                "CREATE UNIQUE INDEX IF NOT EXISTS tasks_todoist_id ON tasks(todoist_id) WHERE todoist_id IS NOT NULL;
                 PRAGMA user_version = 3;",
            )?;
        }
        Ok(())
    })();
    conn.execute_batch(if r.is_ok() { "COMMIT" } else { "ROLLBACK" })?;
    r
}

pub struct NewTask<'a> {
    pub title: &'a str,
    pub description: &'a str,
    pub dir: Option<&'a str>,
    pub labels: &'a [String],
    pub todoist_id: Option<&'a str>,
}

pub struct SessionUpdate<'a> {
    pub id: &'a str,
    pub cwd: &'a str,
    pub state: AgentState,
    pub zellij_session: Option<&'a str>,
    pub zellij_pane: Option<&'a str>,
}

pub struct Store {
    pub(crate) conn: Connection,
}

pub(crate) fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

const TASK_COLS: &str = "id,title,description,kind,stage,position,dir,labels,explainer,todoist_id,created_at,stage_at,synced_stage,link";

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
        synced_stage: r.get(12)?,
        link: r.get(13)?,
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
        enable_wal(&conn, ms);
        migrate(&conn)?;
        Ok(Store { conn })
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Store {
        let conn = Connection::open_in_memory().expect("in-memory sqlite");
        migrate(&conn).expect("schema");
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

    /// The task's type (see `model::KINDS`) and the issue, PR or thread it tracks.
    pub fn set_kind(&self, id: i64, kind: &str, link: Option<&str>) -> Result<()> {
        self.conn.execute("UPDATE tasks SET kind=?1, link=?2 WHERE id=?3", params![kind, link, id])?;
        Ok(())
    }

    /// Only for undoing an add that was refused straight away (adopting at the cap).
    pub fn delete_task(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM tasks WHERE id=?1", [id])?;
        Ok(())
    }

    /// A task that came from Todoist: `stage` is where it lands, `synced` what Todoist is known to hold.
    pub fn add_imported(&self, t: NewTask, stage: Stage, synced: Option<&str>) -> Result<i64> {
        let id = self.add_task(t)?;
        self.conn.execute("UPDATE tasks SET stage=?1, synced_stage=?2 WHERE id=?3", params![stage.as_str(), synced, id])?;
        Ok(id)
    }

    pub fn set_todoist_id(&self, id: i64, todoist_id: &str) -> Result<()> {
        self.conn.execute("UPDATE tasks SET todoist_id=?1 WHERE id=?2", params![todoist_id, id])?;
        Ok(())
    }

    pub fn set_synced_stage(&self, id: i64, stage: Stage) -> Result<()> {
        self.conn.execute("UPDATE tasks SET synced_stage=?1 WHERE id=?2", params![stage.as_str(), id])?;
        Ok(())
    }

    pub fn set_position(&self, id: i64, position: f64) -> Result<()> {
        self.conn.execute("UPDATE tasks SET position=?1 WHERE id=?2", params![position, id])?;
        Ok(())
    }

    pub fn active_count(&self) -> Result<usize> {
        let n: i64 = self.conn.query_row("SELECT COUNT(*) FROM tasks WHERE stage='active' AND kind!='review'", [], |r| r.get(0))?;
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
        if !(matches!(
            (from, to),
            (Queue, Active | Dropped)
                | (Active, Review | Parked | Queue | Dropped)
                | (Parked, Active | Queue | Dropped)
                | (Review, Active | Done | Dropped)
                | (Active, Shelved)
                | (Shelved, Active | Dropped)
        ) || (task.kind == "review" && matches!((from, to), (Queue | Active | Parked, Done))))
        {
            return Ok(Err(Refusal::NotAllowed { from, to }));
        }
        if to == Active && task.kind != "review" && self.active_count()? >= max_active {
            return Ok(Err(Refusal::CapFull { max: max_active }));
        }
        if matches!((from, to), (Active, Review | Shelved)) {
            let missing = gate(self, &task);
            if !missing.is_empty() {
                return Ok(Err(Refusal::Gate(missing)));
            }
        }
        if to == Done && task.kind != "review" {
            if !gate::ready_to_close(&self.prs(id)?) {
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
        if (from, to) == (Shelved, Active) {
            self.conn.execute("UPDATE tasks SET kind='default' WHERE id=?1", [id])?;
        }
        self.add_event(id, "stage", &format!("{from} → {to}"))?;
        Ok(Ok(()))
    }

    fn session_row(r: &rusqlite::Row) -> rusqlite::Result<Session> {
        let (kind, state): (String, String) = (r.get(2)?, r.get(4)?);
        Ok(Session {
            id: r.get(0)?,
            task_id: r.get(1)?,
            kind: SessionKind::from_str(&kind).unwrap_or(SessionKind::Task),
            cwd: r.get(3)?,
            state: AgentState::from_str(&state).unwrap_or(AgentState::Working),
            state_at: r.get(5)?,
            zellij_session: r.get(6)?,
            zellij_pane: r.get(7)?,
            edited: r.get(8)?,
            current: r.get(9)?,
        })
    }

    fn query_sessions(&self, filter: &str, p: impl rusqlite::Params) -> Result<Vec<Session>> {
        let mut q = self.conn.prepare(&format!(
            "SELECT id,task_id,kind,cwd,state,state_at,zellij_session,zellij_pane,edited,current FROM sessions {filter} ORDER BY id"
        ))?;
        let rows = q.query_map(p, Self::session_row)?.collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn session(&self, id: &str) -> Result<Option<Session>> {
        Ok(self.query_sessions("WHERE id=?1", [id])?.pop())
    }

    pub fn current_session(&self, task_id: i64) -> Result<Option<Session>> {
        Ok(self.query_sessions("WHERE task_id=?1 AND current=1", [task_id])?.pop())
    }

    pub fn sessions(&self) -> Result<Vec<Session>> {
        self.query_sessions("", [])
    }

    /// False if the session is absent and `create` is not set. `state_at` moves only when the state does.
    /// `cwd` is set once: Bash `cd` moves the reported cwd, but `claude --resume` must run where the session began.
    pub fn touch_session(&self, u: SessionUpdate, create: bool) -> Result<bool> {
        let n = now();
        let changed = self.conn.execute(
            "UPDATE sessions SET cwd=CASE WHEN cwd='' THEN ?2 ELSE cwd END, state_at=CASE WHEN state!=?3 THEN ?4 ELSE state_at END, state=?3,
               zellij_session=COALESCE(?5,zellij_session), zellij_pane=COALESCE(?6,zellij_pane) WHERE id=?1",
            params![u.id, u.cwd, u.state.as_str(), n, u.zellij_session, u.zellij_pane],
        )?;
        if changed == 0 && create {
            self.conn.execute(
                "INSERT INTO sessions(id,cwd,state,state_at,zellij_session,zellij_pane) VALUES(?1,?2,?3,?4,?5,?6)",
                params![u.id, u.cwd, u.state.as_str(), n, u.zellij_session, u.zellij_pane],
            )?;
        }
        // A pane belongs to the session that last reported it: zellij reuses pane ids after a restart,
        // so an older row still naming this pane must not lead the board to it.
        if let (Some(zs), Some(zp)) = (u.zellij_session, u.zellij_pane) {
            self.conn.execute(
                "UPDATE sessions SET zellij_session=NULL, zellij_pane=NULL WHERE id!=?1 AND zellij_session=?2 AND zellij_pane=?3",
                params![u.id, zs, zp],
            )?;
        }
        Ok(changed > 0 || create)
    }

    /// The session becomes the task's current one; any other session of the task stops being current.
    pub fn link_session(&self, session: &str, task_id: i64, cwd: &str) -> Result<()> {
        self.conn.execute("UPDATE sessions SET current=0 WHERE task_id=?1", [task_id])?;
        self.conn.execute(
            "INSERT INTO sessions(id,task_id,cwd,state_at,current) VALUES(?1,?2,?3,?4,1)
             ON CONFLICT(id) DO UPDATE SET task_id=?2, kind='task', current=1",
            params![session, task_id, cwd, now()],
        )?;
        Ok(())
    }

    pub fn register_pen(&self, session: &str, cwd: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sessions(id,kind,cwd,state_at) VALUES(?1,'pen',?2,?3) ON CONFLICT(id) DO UPDATE SET kind='pen'",
            params![session, cwd, now()],
        )?;
        Ok(())
    }

    pub fn mark_edited(&self, session: &str, root: &str, git_ref: &str) -> Result<()> {
        self.conn.execute("UPDATE sessions SET edited=1 WHERE id=?1", [session])?;
        self.conn.execute(
            "INSERT INTO repos(session_id,root,git_ref) VALUES(?1,?2,?3)
             ON CONFLICT(session_id,root) DO UPDATE SET git_ref=?3",
            params![session, root, git_ref],
        )?;
        Ok(())
    }

    pub fn task_repos(&self, task_id: i64) -> Result<Vec<String>> {
        let mut q = self.conn.prepare(
            "SELECT DISTINCT r.root FROM repos r JOIN sessions s ON s.id=r.session_id WHERE s.task_id=?1 ORDER BY r.root",
        )?;
        let rows = q.query_map([task_id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// A different url or waiver is a different PR, so its known state starts over.
    pub fn set_pr(&self, task_id: i64, root: &str, url: Option<&str>, waiver: Option<&str>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO prs(task_id,root,url,waiver) VALUES(?1,?2,?3,?4)
             ON CONFLICT(task_id,root) DO UPDATE SET url=?3, waiver=?4,
               state=CASE WHEN url IS ?3 AND waiver IS ?4 THEN state ELSE 'unknown' END,
               reviewers=CASE WHEN url IS ?3 AND waiver IS ?4 THEN reviewers ELSE '' END",
            params![task_id, root, url, waiver],
        )?;
        Ok(())
    }

    pub fn set_pr_state(&self, task_id: i64, root: &str, state: PrState, reviewers: &str) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE prs SET state=?3, reviewers=?4 WHERE task_id=?1 AND root=?2 AND (state!=?3 OR reviewers!=?4)",
            params![task_id, root, state.as_str(), reviewers],
        )? > 0;
        if changed {
            self.add_event(task_id, "pr", &format!("{}: {state}", gate::basename(root)))?;
        }
        Ok(changed)
    }

    pub fn prs(&self, task_id: i64) -> Result<Vec<Pr>> {
        let mut q = self.conn.prepare("SELECT root,url,waiver,state,reviewers FROM prs WHERE task_id=?1 ORDER BY root")?;
        let rows = q
            .query_map([task_id], |r| {
                let state: String = r.get(3)?;
                Ok(Pr {
                    task_id,
                    root: r.get(0)?,
                    url: r.get(1)?,
                    waiver: r.get(2)?,
                    state: PrState::from_str(&state).unwrap_or(PrState::Unknown),
                    reviewers: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn set_explainer(&self, task_id: i64, path: &str) -> Result<()> {
        self.conn.execute("UPDATE tasks SET explainer=?1 WHERE id=?2", params![path, task_id])?;
        Ok(())
    }

    /// For a session whose SessionEnd never came; its next report brings it back.
    pub fn end_session(&self, id: &str) -> Result<()> {
        self.conn.execute("UPDATE sessions SET state='ended', state_at=?2 WHERE id=?1", params![id, now()])?;
        Ok(())
    }

    pub fn untracked(&self) -> Result<Vec<Session>> {
        self.query_sessions("WHERE kind='task' AND task_id IS NULL AND edited=1 AND state!='ended'", [])
    }

    pub fn add_event(&self, task_id: i64, kind: &str, detail: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO events(task_id,at,kind,detail) VALUES(?1,?2,?3,?4)",
            params![task_id, now(), kind, detail],
        )?;
        Ok(())
    }

    /// Adds an event, dropping the task's earlier events of the same kind.
    pub fn replace_event(&self, task_id: i64, kind: &str, detail: &str) -> Result<()> {
        self.conn.execute("DELETE FROM events WHERE task_id=?1 AND kind=?2", params![task_id, kind])?;
        self.add_event(task_id, kind, detail)
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
            (Active, Shelved), (Shelved, Active), (Shelved, Dropped),
        ];
        let s = Store::open_in_memory();
        for &from in Stage::ALL {
            for &to in Stage::ALL {
                let id = task_in(&s, from);
                // Done needs a merged or waived PR row.
                s.set_pr(id, "/a", None, Some("no code")).unwrap();
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
    fn experiments_shelve_through_the_gate_and_resume_as_default() {
        let s = Store::open_in_memory();
        let id = task_in(&s, Stage::Active);
        s.set_kind(id, "experiment", None).unwrap();
        assert!(matches!(s.transition(id, Stage::Shelved, 3, &|_, _| vec!["no explainer".into()]).unwrap(), Err(Refusal::Gate(_))));
        s.transition(id, Stage::Shelved, 3, &no_gate).unwrap().unwrap();
        assert_eq!(s.transition(id, Stage::Review, 3, &no_gate).unwrap(), Err(Refusal::NotAllowed { from: Stage::Shelved, to: Stage::Review }));
        s.transition(id, Stage::Active, 3, &no_gate).unwrap().unwrap();
        assert_eq!(s.task(id).unwrap().unwrap().kind, "default");
    }

    #[test]
    fn reviews_skip_the_cap_and_close_without_prs() {
        let s = Store::open_in_memory();
        let mine = task_in(&s, Stage::Active);
        let r = task_in(&s, Stage::Queue);
        s.set_kind(r, "review", Some("https://github.com/o/r/pull/1")).unwrap();
        s.transition(r, Stage::Active, 1, &no_gate).unwrap().unwrap(); // cap 1 already used by `mine`
        assert_eq!(s.active_count().unwrap(), 1);
        s.transition(r, Stage::Done, 1, &no_gate).unwrap().unwrap();
        assert_eq!(s.transition(mine, Stage::Done, 1, &no_gate).unwrap(), Err(Refusal::NotAllowed { from: Stage::Active, to: Stage::Done }));
    }

    #[test]
    fn migration_adds_link_and_keeps_old_rows() {
        let dir = std::env::temp_dir().join(format!("cb-link-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("board.db");
        {
            // A v2 database: no `link` column.
            let c = Connection::open(&path).unwrap();
            c.execute_batch(SCHEMA).unwrap();
            c.execute_batch("PRAGMA user_version = 2; INSERT INTO tasks(title,created_at,stage_at) VALUES('old',1,1)").unwrap();
        }
        let s = Store::open(&path).unwrap();
        let t = &s.tasks().unwrap()[0];
        assert_eq!((t.kind.as_str(), t.link.clone()), ("default", None));
        drop(s);
        Connection::open(&path).unwrap().execute_batch("PRAGMA user_version = 1").unwrap();
        let s = Store::open(&path).unwrap();
        assert_eq!((version(&s), s.tasks().unwrap()[0].link.clone()), (3, None));
        std::fs::remove_dir_all(&dir).unwrap();
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
    fn session_keeps_its_launch_cwd() {
        let s = Store::open_in_memory();
        let u = |cwd| SessionUpdate { id: "s", cwd, state: AgentState::Working, zellij_session: None, zellij_pane: None };
        s.touch_session(u(""), true).unwrap();
        s.touch_session(u("/w"), false).unwrap();
        s.touch_session(u("/w/sub"), false).unwrap();
        s.touch_session(u(""), false).unwrap();
        assert_eq!(s.session("s").unwrap().unwrap().cwd, "/w");
    }

    #[test]
    fn new_pr_value_resets_its_state() {
        let s = Store::open_in_memory();
        let st = |s: &Store| { let p = &s.prs(1).unwrap()[0]; (p.state, p.reviewers.clone()) };
        s.set_pr(1, "/a", Some("https://x/1"), None).unwrap();
        s.set_pr_state(1, "/a", PrState::Merged, "sam").unwrap();
        s.set_pr(1, "/a", Some("https://x/1"), None).unwrap();
        assert_eq!(st(&s), (PrState::Merged, "sam".into()));
        s.set_pr(1, "/a", Some("https://x/2"), None).unwrap();
        assert_eq!(st(&s), (PrState::Unknown, String::new()));
        s.set_pr_state(1, "/a", PrState::Merged, "sam").unwrap();
        s.set_pr(1, "/a", Some("https://x/2"), Some("no code")).unwrap();
        assert_eq!(st(&s), (PrState::Unknown, String::new()));
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

    fn v1_db(dir: &std::path::Path) -> std::path::PathBuf {
        let path = dir.join("board.db");
        let c = Connection::open(&path).unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c.execute_batch("PRAGMA user_version = 1; INSERT INTO tasks(title,created_at,stage_at,stage) VALUES('old',1,1,'review')").unwrap();
        path
    }

    fn version(s: &Store) -> i64 {
        s.conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn v1_database_upgrades_keeping_rows_and_twice_is_a_noop() {
        let dir = std::env::temp_dir().join(format!("cb-mig-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = v1_db(&dir);
        let s = Store::open(&path).unwrap();
        assert_eq!(version(&s), 3);
        let t = s.tasks().unwrap();
        assert_eq!((t.len(), t[0].title.as_str(), t[0].stage, t[0].synced_stage.clone()), (1, "old", Stage::Review, None));
        s.set_synced_stage(t[0].id, Stage::Review).unwrap();
        drop(s);
        let s = Store::open(&path).unwrap();
        assert_eq!((version(&s), s.tasks().unwrap()[0].synced_stage.as_deref()), (3, Some("review")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn fresh_database_is_v3_and_todoist_ids_are_unique() {
        let s = Store::open_in_memory();
        assert_eq!(version(&s), 3);
        let mk = |tid| s.add_task(NewTask { title: "x", description: "", dir: None, labels: &[], todoist_id: tid });
        mk(None).unwrap();
        mk(None).unwrap();
        mk(Some("a")).unwrap();
        assert!(mk(Some("a")).is_err());
    }

    #[test]
    fn v2_database_reset_to_v1_by_an_old_binary_still_opens() {
        let dir = std::env::temp_dir().join(format!("cb-reset-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("board.db");
        drop(Store::open(&path).unwrap());
        Connection::open(&path).unwrap().execute_batch("PRAGMA user_version = 1").unwrap();
        let s = Store::open(&path).unwrap();
        assert_eq!(version(&s), 3);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn concurrent_opens_of_a_v1_file_both_succeed() {
        let dir = std::env::temp_dir().join(format!("cb-race-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = v1_db(&dir);
        let hs: Vec<_> = (0..4).map(|_| { let p = path.clone(); std::thread::spawn(move || Store::open(&p).map(|s| version(&s))) }).collect();
        for h in hs {
            assert_eq!(h.join().unwrap().unwrap(), 3);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
