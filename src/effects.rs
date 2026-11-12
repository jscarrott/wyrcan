//! Carries out the app's effects: stage changes (always through `Store::transition`) and zellij panes.
use crate::app::{App, CardKey, Effect, Prompt};
use crate::config::Config;
use crate::gate::{self, PrVerifier};
use crate::model::{AgentState, Refusal, SessionKind, Stage, Task};
use crate::reviews;
use crate::store::{NewTask, Store};
use crate::zellij::{Runner, Zellij};
use anyhow::{bail, Context, Result};
use std::collections::{HashMap, HashSet};

pub struct Ctx<'a> {
    pub store: &'a Store,
    pub cfg: &'a Config,
    pub zj: &'a Zellij,
    pub run: &'a dyn Runner,
    pub verify: &'a PrVerifier<'a>,
    /// Where pen sessions start.
    pub cwd: &'a str,
}

/// What the board knows about zellij panes between effects.
pub struct Panes {
    /// The live panes, as wyrcan's events stream last reported them.
    live: Vec<String>,
    /// Panes opened for tasks whose session hasn't reported its pane yet.
    opened: HashMap<i64, String>,
    /// Board start: the tick only closes panes of streams that left Active after it,
    /// so a pane id reused after a zellij restart is never closed for a stale session.
    since: i64,
    /// (task, stage_at) the tick has already dealt with: it closes at most once per stage entry.
    settled: HashSet<(i64, i64)>,
}

impl Panes {
    pub fn new(since: i64) -> Panes {
        Panes { live: vec![], opened: HashMap::new(), since, settled: HashSet::new() }
    }

    pub fn set_live(&mut self, live: Vec<String>) {
        self.live = live;
    }
}

/// `Store::transition` with the real gate; the board's UI thread and its review worker both come through here.
pub fn transition(store: &Store, cfg: &Config, id: i64, to: Stage, verify: &PrVerifier) -> Result<Result<(), Refusal>> {
    if to == Stage::Review {
        let kind = store.task(id)?.context("No such task.")?.kind;
        return gate::to_review(store, id, cfg.max_active, &cfg.gate_checks(&kind), verify);
    }
    store.transition(id, to, cfg.max_active, &|s, t| gate::evaluate(s, t, &cfg.gate_checks(&t.kind), verify))
}

impl Ctx<'_> {
    fn task(&self, id: i64) -> Result<Task> {
        self.store.task(id)?.context("No such task.")
    }

    /// The one way the board changes a stage; a refusal goes to the footer verbatim.
    fn move_to(&self, id: i64, to: Stage, app: &mut App) -> Result<bool> {
        match transition(self.store, self.cfg, id, to, self.verify)? {
            Ok(()) => Ok(true),
            Err(r) => {
                app.message = Some(r.to_string());
                Ok(false)
            }
        }
    }

    /// Forgets an opened pane once another task's session reports it.
    /// Not once it leaves `live`: an events line sent just before the open could still be on its way.
    fn refresh(&self, p: &mut Panes) -> Result<()> {
        let sessions = self.store.sessions()?;
        let live = &p.live;
        p.opened.retain(|id, x| !sessions.iter().any(|s| s.task_id != Some(*id) && self.zj.pane_of(s, live) == Some(x.as_str())));
        Ok(())
    }

    /// The task's pane, if it's in the latest `live`: its current session's, else the one we opened for it.
    /// A reused pane id is caught once a Claude session in it reports (touch_session hands the pane over).
    // shortcut: a plain shell in a reused pane id still looks like the old session's pane; needs pane identity zellij doesn't give.
    fn pane(&self, p: &Panes, id: i64) -> Result<Option<String>> {
        let s = self.store.current_session(id)?;
        if let Some(x) = s.as_ref().and_then(|s| self.zj.pane_of(s, &p.live)) {
            return Ok(Some(x.to_string()));
        }
        Ok(p.opened.get(&id).filter(|x| p.live.contains(x)).cloned())
    }

    fn live_pane(&self, p: &mut Panes, id: i64) -> Result<String> {
        self.refresh(p)?;
        self.pane(p, id)?.context("No live session. Resume it first.")
    }

    /// Live panes of streams and pen sessions, which new panes join.
    fn stack(&self, p: &Panes) -> Result<Vec<String>> {
        let ours = self.store.sessions()?;
        let ours = ours.iter().filter(|s| s.task_id.is_some() || s.kind == SessionKind::Pen);
        let mut v: Vec<String> = ours.filter_map(|s| self.zj.pane_of(s, &p.live)).map(String::from).chain(p.opened.values().filter(|x| p.live.contains(x)).cloned()).collect();
        v.retain(|x| Some(x.as_str()) != self.zj.board_pane.as_deref());
        v.sort();
        v.dedup();
        Ok(v)
    }

    fn claude_dir(&self) -> String {
        self.cfg.claude_dir.to_string_lossy().into_owned()
    }
}

/// Runs effects in order; the first error goes to the footer and drops the rest. True means quit.
pub fn run_all(effects: Vec<Effect>, c: &Ctx, p: &mut Panes, app: &mut App) -> bool {
    for e in effects {
        if e == Effect::Quit {
            return true;
        }
        if let Err(err) = handle(e, c, p, app) {
            app.message = Some(format!("{err:#}"));
            break;
        }
    }
    false
}

pub fn handle(e: Effect, c: &Ctx, p: &mut Panes, app: &mut App) -> Result<()> {
    match e {
        Effect::Start(id) => open(c, p, app, id, false)?,
        Effect::Resume(id) => open(c, p, app, id, true)?,
        Effect::Unshelve(id) => open(c, p, app, id, false)?,
        Effect::SetDir { id, dir } => {
            let dir = crate::config::expand_home(&dir);
            is_dir(&dir)?;
            let t = c.task(id)?;
            c.store.update_task(id, &t.title, &t.description, Some(&dir), &t.labels)?;
        }
        Effect::SetReviewParent { id, repo, dir } => {
            let dir = crate::config::expand_home(&dir);
            if !std::path::Path::new(&dir).join(".jj").is_dir() {
                bail!("Not a jj repo: {dir}");
            }
            c.task(id)?;
            c.store.set_review_parent(&repo, &dir)?;
        }
        Effect::Park(id) => {
            // Parking works outside zellij too; there's just no pane to close.
            if c.move_to(id, Stage::Parked, app)? && c.zj.board().is_ok() {
                if let Ok(x) = c.live_pane(p, id) {
                    c.zj.close(c.run, &x)?;
                }
            }
        }
        Effect::Requeue(id) => _ = c.move_to(id, Stage::Queue, app)?,
        Effect::ToReview(id) => _ = c.move_to(id, Stage::Review, app)?,
        Effect::Done(id) => _ = c.move_to(id, Stage::Done, app)?,
        Effect::Drop(id) => _ = c.move_to(id, Stage::Dropped, app)?,
        Effect::Adopt { session, title } => {
            let s = c.store.session(&session)?.context("That session is gone.")?;
            let id = c.store.add_task(NewTask { title: &title, description: "", dir: Some(&s.cwd), labels: &[], todoist_id: None })?;
            if !c.move_to(id, Stage::Active, app)? {
                c.store.delete_task(id)?;
                return Ok(());
            }
            c.store.link_session(&session, id, &s.cwd)?;
            app.selected = Some(CardKey::Task(id));
        }
        Effect::Dismiss(session) => c.store.end_session(&session)?,
        Effect::Show(id) => {
            // Moving through the list outside zellij, or onto a card without a pane, is fine.
            if c.zj.board().is_ok() {
                if let Ok(x) = c.live_pane(p, id) {
                    c.zj.show(c.run, &x)?;
                }
            }
        }
        Effect::Enter(id) => c.zj.enter(c.run, &c.live_pane(p, id)?)?,
        Effect::Fullscreen(id) => c.zj.fullscreen(c.run, &c.live_pane(p, id)?)?,
        Effect::ToggleBoardWide => c.zj.toggle_board_fullscreen(c.run)?,
        Effect::Wrap(id) => {
            // Enter would answer a permission prompt, or rerun an ended pane's command.
            match c.store.current_session(id)?.map(|s| s.state) {
                Some(AgentState::Waiting) => {}
                Some(s) => {
                    app.message = Some(format!("Agent is {s}; answer it first."));
                    return Ok(());
                }
                None => bail!("No live session. Resume it first."),
            }
            c.zj.type_line(c.run, &c.live_pane(p, id)?, "/wrap")?;
            app.message = Some("Asked it to wrap up.".into());
        }
        Effect::SpawnPen => {
            c.refresh(p)?;
            c.zj.open(c.run, "pen", c.cwd, &c.claude_dir(), &["/pen"], &c.stack(p)?)?;
        }
        Effect::OpenExplainer(id) => xdg_open(&c.task(id)?.explainer.context("No explainer yet.")?)?,
        Effect::OpenPr(id) => {
            let urls: Vec<String> = c.store.prs(id)?.into_iter().filter_map(|p| p.url).collect();
            if urls.is_empty() {
                bail!("No PR yet.");
            }
            for u in urls {
                if !u.starts_with("https://") {
                    bail!("Not opening {u}: not an https URL.");
                }
                xdg_open(&u)?;
            }
        }
        Effect::Add { title, dir, labels, experiment } => {
            if let Some(d) = &dir {
                is_dir(d)?;
            }
            let id = c.store.add_task(NewTask { title: &title, description: "", dir: dir.as_deref(), labels: &labels, todoist_id: None })?;
            if experiment {
                c.store.set_kind(id, "experiment", None)?;
            }
            app.quick = None;
            app.selected = Some(CardKey::Task(id));
        }
        Effect::Quit => {}
    }
    Ok(())
}

fn is_dir(d: &str) -> Result<()> {
    if !std::path::Path::new(d).is_dir() {
        bail!("No such directory: {d}");
    }
    Ok(())
}

/// Start (`/next <id>`) or resume (`--resume <session>`) a stream in a new pane beside the board.
fn open(c: &Ctx, p: &mut Panes, app: &mut App, id: i64, resume: bool) -> Result<()> {
    let t = c.task(id)?;
    let session = c.store.current_session(id)?.filter(|_| resume);
    // A resumed session has to run where it was started.
    let cwd = session.as_ref().map(|s| s.cwd.clone()).filter(|d| !d.is_empty()).or(t.dir.clone());
    let cwd = match cwd {
        Some(d) => d,
        None if t.kind == "review" => match review_workspace(c, app, &t)? {
            Some(d) => d,
            None => return Ok(()),
        },
        None => return ask_dir(c, app, &t, resume),
    };
    // Checked before the stage changes, so a board outside zellij can't start what it can't open.
    c.zj.board()?;
    if t.stage != Stage::Active && !c.move_to(id, Stage::Active, app)? {
        return Ok(());
    }
    c.refresh(p)?;
    if let Some(x) = c.pane(p, id)? {
        return c.zj.show(c.run, &x);
    }
    let next = if t.kind == "review" { format!("/review-pr {id}") } else { format!("/next {id}") };
    let args = match &session {
        Some(s) => vec!["--resume", s.id.as_str()],
        None => vec![next.as_str()],
    };
    let x = c.zj.open(c.run, &t.title, &cwd, &c.claude_dir(), &args, &c.stack(p)?)?;
    p.opened.insert(id, x.clone());
    p.live.push(x);
    Ok(())
}

/// Prefills the directory prompt with the most recently moved task's directory.
fn ask_dir(c: &Ctx, app: &mut App, t: &Task, resume: bool) -> Result<()> {
    let mut dirs: Vec<Task> = c.store.tasks()?.into_iter().filter(|o| o.id != t.id && o.dir.is_some()).collect();
    dirs.sort_by_key(|o| -o.stage_at);
    let recent = dirs.into_iter().next().and_then(|o| o.dir).unwrap_or_default();
    app.prompt = Some(Prompt::dir(t.id, resume, &t.title, recent));
    Ok(())
}

/// Makes a review task its own jj workspace at the PR's head under ~/git-work and points the task at it.
/// `None`: it asked for the parent repo instead.
fn review_workspace(c: &Ctx, app: &mut App, t: &Task) -> Result<Option<String>> {
    let link = t.link.as_deref().unwrap_or_default();
    let (repo, number) = reviews::pr_ref(link).with_context(|| format!("Can't read a PR from {link:?}."))?;
    let home = crate::config::home();
    let guess = home.join("git-work").join(repo);
    let parent = match c.store.review_parent(repo)? {
        Some(p) => p,
        None if guess.join(".jj").is_dir() => {
            let p = guess.to_string_lossy().into_owned();
            c.store.set_review_parent(repo, &p)?;
            p
        }
        None => {
            app.prompt = Some(Prompt::review_parent(t.id, repo));
            return Ok(None);
        }
    };
    c.zj.board()?;
    let cmd = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let json = c.run.run(&cmd(&["gh", "pr", "view", link, "--json", "headRefName"]))?;
    let head = serde_json::from_str::<serde_json::Value>(&json).ok().and_then(|v| v["headRefName"].as_str().map(String::from)).unwrap_or_default();
    // The head goes into jj args and a revset: refuse anything unusual rather than quote it.
    if head.is_empty() || head.starts_with('-') || !head.chars().all(|ch| ch.is_ascii_alphanumeric() || "/_.-".contains(ch)) {
        bail!("Can't use the PR's head branch {head:?}.");
    }
    let path = reviews::workspace_path(&home, repo, number).to_string_lossy().into_owned();
    let [fetch, add] = <[_; 2]>::try_from(reviews::workspace_cmds(&parent, &path, &format!("{repo}-{number}-review"), &head)).expect("two commands");
    c.run.run(&fetch).with_context(|| format!("Can't fetch {head} from origin"))?;
    // `jj git fetch` only warns about a missing branch, and a failed `workspace add` leaves a half-made workspace, so check first.
    // shortcut: fork PRs aren't supported (head not on origin); add `gh pr checkout` in a colocated workspace if they come up.
    if c.run.run(&cmd(&["jj", "-R", &parent, "log", "-r", &format!("{head}@origin"), "--no-graph", "-T", "commit_id"])).is_err() {
        bail!("Can't fetch {head} from origin (fork PR?)");
    }
    c.run.run(&add)?;
    c.store.add_event(t.id, "workspace", &path)?;
    c.store.update_task(t.id, &t.title, &t.description, Some(&path), &t.labels)?;
    Ok(Some(path))
}

/// Detached: Command::output() would block until the browser closes its inherited pipes.
// shortcut: the child is never waited on (zombie until exit), fine for a short-lived TUI.
fn xdg_open(target: &str) -> Result<()> {
    std::process::Command::new("xdg-open")
        .arg(target)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("running xdg-open")?;
    Ok(())
}

/// Every second: refresh the live panes and close the panes of streams that left Active, once their agent is idle.
/// Working or Blocked (say, a wrapped-up agent on a permission prompt) keeps its pane.
/// Once per stage entry: a stream later resumed by hand outside Active is only flagged.
pub fn tick(c: &Ctx, p: &mut Panes) -> Result<()> {
    if c.zj.board().is_err() {
        return Ok(());
    }
    c.refresh(p)?;
    let sessions = c.store.sessions()?;
    for t in c.store.tasks()?.iter().filter(|t| t.stage != Stage::Active && t.stage_at >= p.since) {
        let Some(s) = sessions.iter().find(|s| s.task_id == Some(t.id) && s.current) else { continue };
        if !matches!(s.state, AgentState::Waiting | AgentState::Ended) || !p.settled.insert((t.id, t.stage_at)) {
            continue;
        }
        // pane_of confirms the pane is live and in this zellij session.
        if let Some(x) = c.zj.pane_of(s, &p.live).filter(|x| Some(*x) != c.zj.board_pane.as_deref()) {
            c.zj.close(c.run, x)?;
            p.live.retain(|l| l != x);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::{add, app, cfg, session};
    use crate::store::SessionUpdate;
    use std::cell::RefCell;

    /// Records every command; `open` answers "terminal_9". `live` is what the events stream reported (see `panes`).
    struct Rec {
        calls: RefCell<Vec<String>>,
        live: Vec<&'static str>,
    }
    impl Runner for Rec {
        fn run(&self, cmd: &[String]) -> Result<String> {
            let line = cmd.join(" ");
            self.calls.borrow_mut().push(line.clone());
            Ok(if line.contains("--name open") {
                "terminal_9\n".into()
            } else if line.starts_with("gh pr view") {
                r#"{"headRefName":"feat/x"}"#.into()
            } else {
                String::new()
            })
        }
    }
    fn rec(live: &[&'static str]) -> Rec {
        Rec { calls: RefCell::new(vec![]), live: live.to_vec() }
    }
    /// Pane state as if the events stream had reported `r.live`.
    fn panes(r: &Rec) -> Panes {
        let mut p = Panes::new(0);
        p.set_live(r.live.iter().map(|x| x.to_string()).collect());
        p
    }
    const P: &str = "wyrcan pipe --plugin zellij:wyrcan --name";
    fn inside() -> Zellij {
        Zellij { session: Some("z".into()), board_pane: Some("terminal_0".into()), bin: "wyrcan".into() }
    }
    fn ctx<'a>(s: &'a Store, c: &'a Config, zj: &'a Zellij, r: &'a Rec) -> Ctx<'a> {
        Ctx { store: s, cfg: c, zj, run: r, verify: &|_| true, cwd: "/home" }
    }
    fn stage(s: &Store, id: i64) -> Stage {
        s.task(id).unwrap().unwrap().stage
    }
    /// Puts session `sid` of task `id` in zellij session "z", pane `pane`.
    fn pane(s: &Store, sid: &str, pane: &str, state: AgentState) {
        s.touch_session(SessionUpdate { id: sid, cwd: "/w", state, zellij_session: Some("z"), zellij_pane: Some(pane) }, false).unwrap();
    }

    #[test]
    fn start_at_cap_is_refused_and_opens_nothing() {
        let (s, c, zj, r) = (Store::open_in_memory(), cfg(1), inside(), rec(&[]));
        add(&s, "busy", Stage::Active);
        let q = add(&s, "next", Stage::Queue);
        let mut ap = app(&s, &c);
        run_all(vec![Effect::Start(q)], &ctx(&s, &c, &zj, &r), &mut panes(&r), &mut ap);
        assert!(r.calls.borrow().is_empty(), "{:?}", r.calls.borrow());
        assert_eq!(ap.message.as_deref(), Some("All 1 active slots are in use. Park or finish a stream first."));
        assert_eq!(stage(&s, q), Stage::Queue);
    }

    #[test]
    fn start_outside_zellij_says_so_and_changes_nothing() {
        let (s, c, r) = (Store::open_in_memory(), cfg(3), rec(&[]));
        let zj = Zellij { session: None, board_pane: None, bin: String::new() };
        let q = add(&s, "next", Stage::Queue);
        let mut ap = app(&s, &c);
        run_all(vec![Effect::Start(q)], &ctx(&s, &c, &zj, &r), &mut panes(&r), &mut ap);
        assert!(ap.message.as_deref().unwrap().starts_with("not inside wyrcan"), "{:?}", ap.message);
        assert_eq!(stage(&s, q), Stage::Queue);
        assert!(r.calls.borrow().is_empty());
    }

    #[test]
    fn start_opens_a_pane_and_remembers_it() {
        let (s, c, zj, r) = (Store::open_in_memory(), cfg(3), inside(), rec(&["terminal_0", "terminal_9"]));
        let q = add(&s, "next", Stage::Queue);
        let mut ap = app(&s, &c);
        let mut p = panes(&r);
        assert!(!run_all(vec![Effect::Start(q)], &ctx(&s, &c, &zj, &r), &mut p, &mut ap));
        assert_eq!(stage(&s, q), Stage::Active);
        let want = format!(r#""argv":["env","CLAUDE_CONFIG_DIR=/c","claude","/next {q}"]"#);
        assert!(r.calls.borrow().iter().any(|l| l.starts_with(&format!("{P} open")) && l.contains(&want)), "{:?}", r.calls.borrow());
        // The session hasn't reported its pane yet, but Enter already finds it.
        r.calls.borrow_mut().clear();
        run_all(vec![Effect::Enter(q)], &ctx(&s, &c, &zj, &r), &mut p, &mut ap);
        assert_eq!(r.calls.borrow().last().unwrap(), &format!("{P} focus --args pane=terminal_9"));
    }

    #[test]
    fn start_without_dir_asks_for_one() {
        let (s, c, zj, r) = (Store::open_in_memory(), cfg(3), inside(), rec(&["terminal_0", "terminal_9"]));
        let q = s.add_task(NewTask { title: "t", description: "", dir: None, labels: &[], todoist_id: None }).unwrap();
        let mut ap = app(&s, &c);
        let mut p = panes(&r);
        run_all(vec![Effect::Start(q)], &ctx(&s, &c, &zj, &r), &mut p, &mut ap);
        assert_eq!(ap.prompt.as_ref().unwrap().label, "Directory for 't':");
        assert_eq!(stage(&s, q), Stage::Queue);
        let dir = std::env::temp_dir().to_string_lossy().into_owned();
        run_all(vec![Effect::SetDir { id: q, dir: dir.clone() }, Effect::Start(q)], &ctx(&s, &c, &zj, &r), &mut p, &mut ap);
        assert_eq!(s.task(q).unwrap().unwrap().dir.as_deref(), Some(dir.as_str()));
        assert_eq!(stage(&s, q), Stage::Active);
    }

    #[test]
    fn start_review_creates_workspace_and_runs_review_pr() {
        let (s, c, zj, r) = (Store::open_in_memory(), cfg(3), inside(), rec(&["terminal_0", "terminal_9"]));
        let q = s.add_task(NewTask { title: "Add filter", description: "PR by @al", dir: None, labels: &["delta".into()], todoist_id: None }).unwrap();
        s.set_kind(q, "review", Some("https://github.com/acme/delta/pull/812")).unwrap();
        s.set_review_parent("delta", "/h/git-work/delta").unwrap();
        let mut ap = app(&s, &c);
        run_all(vec![Effect::Start(q)], &ctx(&s, &c, &zj, &r), &mut panes(&r), &mut ap);
        assert_eq!(ap.message, None);
        let ws = crate::reviews::workspace_path(&crate::config::home(), "delta", 812).to_string_lossy().into_owned();
        let calls = r.calls.borrow();
        let at = |want: &str| calls.iter().position(|l| l.starts_with(want)).unwrap_or_else(|| panic!("no {want} in {calls:?}"));
        let fetch = at("jj -R /h/git-work/delta git fetch --branch feat/x");
        let add = at(&format!("jj -R /h/git-work/delta workspace add --name delta-812-review -r feat/x@origin {ws}"));
        let open = at(&format!("{P} open"));
        assert!(fetch < add && add < open, "{calls:?}");
        assert!(calls[open].contains(&format!(r#""claude","/review-pr {q}"]"#)), "{}", calls[open]);
        assert_eq!(s.task(q).unwrap().unwrap().dir.as_deref(), Some(ws.as_str()));
        assert_eq!(s.events(q).unwrap().into_iter().rfind(|e| e.kind == "workspace").unwrap().detail, ws);
        assert_eq!(stage(&s, q), Stage::Active);
    }

    #[test]
    fn start_review_without_a_parent_asks_for_one() {
        let (s, c, zj, r) = (Store::open_in_memory(), cfg(3), inside(), rec(&[]));
        let q = s.add_task(NewTask { title: "t", description: "", dir: None, labels: &[], todoist_id: None }).unwrap();
        s.set_kind(q, "review", Some("https://github.com/acme/no-such-repo-xyz/pull/3")).unwrap();
        let mut ap = app(&s, &c);
        run_all(vec![Effect::Start(q)], &ctx(&s, &c, &zj, &r), &mut panes(&r), &mut ap);
        assert_eq!(ap.prompt.as_ref().unwrap().label, "Local jj repo for 'no-such-repo-xyz':");
        assert!(r.calls.borrow().is_empty());
        let dir = std::env::temp_dir().join(format!("wyrcan-parent-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".jj")).unwrap();
        let dir = dir.to_string_lossy().into_owned();
        run_all(vec![Effect::SetReviewParent { id: q, repo: "no-such-repo-xyz".into(), dir: dir.clone() }], &ctx(&s, &c, &zj, &r), &mut panes(&r), &mut ap);
        assert_eq!(s.review_parent("no-such-repo-xyz").unwrap(), Some(dir.clone()));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn move_right_from_active_hits_the_gate() {
        let (s, c, zj, r) = (Store::open_in_memory(), cfg(3), inside(), rec(&[]));
        let id = add(&s, "a", Stage::Active);
        let mut ap = app(&s, &c);
        let e = ap.on_key_event(crate::app::tests::key('L'));
        run_all(e, &ctx(&s, &c, &zj, &r), &mut Panes::new(0), &mut ap);
        assert_eq!(ap.message.as_deref(), Some("Not ready for review: no PR or reason recorded; no explainer"));
        assert_eq!(stage(&s, id), Stage::Active);
    }

    #[test]
    fn move_right_from_review_needs_merge() {
        let (s, c, zj, r) = (Store::open_in_memory(), cfg(3), inside(), rec(&[]));
        let id = add(&s, "a", Stage::Review);
        s.set_pr(id, "/r/app", Some("u"), None).unwrap();
        s.set_pr_state(id, "/r/app", crate::model::PrState::Waiting, "").unwrap();
        let mut ap = app(&s, &c);
        let e = ap.on_key_event(crate::app::tests::key('L'));
        assert_eq!(e, [Effect::Done(id)]);
        run_all(e, &ctx(&s, &c, &zj, &r), &mut Panes::new(0), &mut ap);
        assert_eq!(ap.message.as_deref(), Some("Not merged yet. Drop it if you want to abandon it."));
        assert_eq!(stage(&s, id), Stage::Review);
    }

    #[test]
    fn adopt_links_or_is_refused_at_the_cap() {
        let (s, c, zj, r) = (Store::open_in_memory(), cfg(1), inside(), rec(&[]));
        session(&s, "u1", None, AgentState::Waiting);
        let mut ap = app(&s, &c);
        let adopt = || vec![Effect::Adopt { session: "u1".into(), title: "proj".into() }];
        run_all(adopt(), &ctx(&s, &c, &zj, &r), &mut Panes::new(0), &mut ap);
        let t = s.tasks().unwrap();
        assert_eq!((t.len(), t[0].stage, t[0].title.as_str(), t[0].dir.as_deref()), (1, Stage::Active, "proj", Some("/w/proj")));
        assert_eq!(s.session("u1").unwrap().unwrap().task_id, Some(t[0].id));
        // Full now: a second adopt adds nothing.
        session(&s, "u2", None, AgentState::Waiting);
        run_all(vec![Effect::Adopt { session: "u2".into(), title: "x".into() }], &ctx(&s, &c, &zj, &r), &mut Panes::new(0), &mut ap);
        assert_eq!(ap.message.as_deref(), Some("All 1 active slots are in use. Park or finish a stream first."));
        assert_eq!(s.tasks().unwrap().len(), 1);
    }

    #[test]
    fn resume_after_pane_id_reuse_opens_its_own_pane() {
        let (s, c, zj, r) = (Store::open_in_memory(), cfg(3), inside(), rec(&["terminal_0", "terminal_2"]));
        let parked = add(&s, "parked", Stage::Parked);
        session(&s, "old", Some(parked), AgentState::Waiting);
        pane(&s, "old", "terminal_2", AgentState::Waiting);
        // zellij restarted under the same name; another stream's session now sits in terminal_2.
        let other = add(&s, "other", Stage::Active);
        session(&s, "new", Some(other), AgentState::Working);
        pane(&s, "new", "terminal_2", AgentState::Working);
        let mut ap = app(&s, &c);
        let mut p = panes(&r);
        run_all(vec![Effect::Resume(parked)], &ctx(&s, &c, &zj, &r), &mut p, &mut ap);
        assert_eq!(ap.message, None);
        assert_eq!(stage(&s, parked), Stage::Active);
        let calls = r.calls.borrow();
        assert!(calls.iter().any(|l| l.starts_with(&format!("{P} open")) && l.contains(r#""claude","--resume","old"]"#)), "{calls:?}");
        assert!(!calls.iter().any(|l| l.ends_with("pane=terminal_2")), "{calls:?}");
    }

    #[test]
    fn opened_pane_is_dropped_once_another_session_claims_it() {
        let (s, c, zj, r) = (Store::open_in_memory(), cfg(3), inside(), rec(&["terminal_0", "terminal_9"]));
        let a = add(&s, "a", Stage::Active);
        let b = add(&s, "b", Stage::Active);
        session(&s, "sb", Some(b), AgentState::Working);
        pane(&s, "sb", "terminal_9", AgentState::Working);
        let mut ap = app(&s, &c);
        let mut p = panes(&r);
        p.opened.insert(a, "terminal_9".into());
        run_all(vec![Effect::Enter(a)], &ctx(&s, &c, &zj, &r), &mut p, &mut ap);
        assert_eq!(ap.message.as_deref(), Some("No live session. Resume it first."));
        assert!(!r.calls.borrow().iter().any(|l| l.contains("pane=terminal_9")));
    }

    #[test]
    fn park_closes_the_live_pane() {
        let (s, c, zj, r) = (Store::open_in_memory(), cfg(3), inside(), rec(&["terminal_0", "terminal_4"]));
        let id = add(&s, "a", Stage::Active);
        session(&s, "s1", Some(id), AgentState::Waiting);
        pane(&s, "s1", "terminal_4", AgentState::Waiting);
        let mut ap = app(&s, &c);
        run_all(vec![Effect::Park(id)], &ctx(&s, &c, &zj, &r), &mut panes(&r), &mut ap);
        assert_eq!(stage(&s, id), Stage::Parked);
        assert_eq!(r.calls.borrow().last().unwrap(), &format!("{P} close --args pane=terminal_4"));
    }

    #[test]
    fn wrap_types_into_the_pane_or_says_why_not() {
        let (s, c, zj) = (Store::open_in_memory(), cfg(3), inside());
        let id = add(&s, "a", Stage::Active);
        session(&s, "s1", Some(id), AgentState::Waiting);
        pane(&s, "s1", "terminal_4", AgentState::Waiting);
        let mut ap = app(&s, &c);
        let gone = rec(&["terminal_0"]);
        run_all(vec![Effect::Wrap(id)], &ctx(&s, &c, &zj, &gone), &mut panes(&gone), &mut ap);
        assert_eq!(ap.message.as_deref(), Some("No live session. Resume it first."));
        let r = rec(&["terminal_0", "terminal_4"]);
        run_all(vec![Effect::Wrap(id)], &ctx(&s, &c, &zj, &r), &mut panes(&r), &mut ap);
        assert!(r.calls.borrow().contains(&format!("{P} write --args pane=terminal_4 -- /wrap")));
    }

    #[test]
    fn wrap_only_types_into_a_waiting_agent() {
        let (s, c, zj) = (Store::open_in_memory(), cfg(3), inside());
        let id = add(&s, "a", Stage::Active);
        session(&s, "s1", Some(id), AgentState::Waiting);
        for (state, msg) in [(AgentState::Blocked, "Agent is blocked; answer it first."), (AgentState::Ended, "Agent is ended; answer it first."), (AgentState::Working, "Agent is working; answer it first.")] {
            pane(&s, "s1", "terminal_4", state);
            let r = rec(&["terminal_0", "terminal_4"]);
            let mut ap = app(&s, &c);
            run_all(vec![Effect::Wrap(id)], &ctx(&s, &c, &zj, &r), &mut panes(&r), &mut ap);
            assert_eq!(ap.message.as_deref(), Some(msg));
            assert!(!r.calls.borrow().iter().any(|l| l.contains("write")), "{:?}", r.calls.borrow());
        }
    }

    #[test]
    fn dismiss_ends_an_untracked_session_until_it_reports_again() {
        let (s, c, zj, r) = (Store::open_in_memory(), cfg(3), inside(), rec(&[]));
        session(&s, "u1", None, AgentState::Waiting);
        s.mark_edited("u1", "/w/proj", "git main").unwrap();
        let mut ap = app(&s, &c);
        run_all(vec![Effect::Dismiss("u1".into())], &ctx(&s, &c, &zj, &r), &mut Panes::new(0), &mut ap);
        assert!(s.untracked().unwrap().is_empty());
        session(&s, "u1", None, AgentState::Working);
        assert_eq!(s.untracked().unwrap().len(), 1);
    }

    #[test]
    fn open_pr_refuses_a_non_https_value() {
        let (s, c, zj, r) = (Store::open_in_memory(), cfg(3), inside(), rec(&[]));
        let id = add(&s, "a", Stage::Review);
        s.set_pr(id, "/r/app", Some("--help"), None).unwrap();
        let mut ap = app(&s, &c);
        run_all(vec![Effect::OpenPr(id)], &ctx(&s, &c, &zj, &r), &mut Panes::new(0), &mut ap);
        assert_eq!(ap.message.as_deref(), Some("Not opening --help: not an https URL."));
    }

    /// Draws the wide board, then drags `id`'s card onto the `to` column and runs what that asks for.
    fn drag(s: &Store, c: &Config, ap: &mut App, id: i64, to: crate::app::Group) {
        use crate::app::Target;
        use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind as K};
        use ratatui::layout::Rect;
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 40)).unwrap();
        term.draw(|f| crate::ui::draw(f, ap)).unwrap();
        let rect = |t: Target| ap.hits.iter().find(|h| h.target == t).unwrap().rect;
        let (from, onto) = (rect(Target::Card(CardKey::Task(id))), rect(Target::Group(to)));
        let ev = |kind, r: Rect| MouseEvent { kind, column: r.x + 2, row: r.y + 2, modifiers: KeyModifiers::NONE };
        let mut e = ap.on_mouse(ev(K::Down(MouseButton::Left), from));
        e.extend(ap.on_mouse(ev(K::Drag(MouseButton::Left), onto)));
        assert_eq!(ap.drag.as_ref().and_then(|d| d.over), Some(to), "the target highlights");
        e.extend(ap.on_mouse(ev(K::Up(MouseButton::Left), onto)));
        let (zj, r) = (inside(), rec(&["terminal_0"]));
        run_all(e, &ctx(s, c, &zj, &r), &mut panes(&r), ap);
    }

    #[test]
    fn drag_to_review_is_refused_by_gate() {
        let (s, c) = (Store::open_in_memory(), cfg(3));
        let id = add(&s, "a", Stage::Active);
        let mut ap = app(&s, &c);
        drag(&s, &c, &mut ap, id, crate::app::Group::Review);
        assert_eq!(ap.message.as_deref(), Some("Not ready for review: no PR or reason recorded; no explainer"));
        assert_eq!(stage(&s, id), Stage::Active);
    }

    #[test]
    fn drag_to_parked_moves() {
        let (s, c) = (Store::open_in_memory(), cfg(3));
        let id = add(&s, "a", Stage::Active);
        let q = add(&s, "q", Stage::Queue);
        let mut ap = app(&s, &c);
        drag(&s, &c, &mut ap, id, crate::app::Group::Parked);
        assert_eq!((ap.message.as_deref(), stage(&s, id)), (None, Stage::Parked));
        // Onto its own column: nothing happens.
        drag(&s, &c, &mut ap, q, crate::app::Group::Queue);
        assert_eq!((ap.message.as_deref(), stage(&s, q)), (None, Stage::Queue));
    }

    #[test]
    fn quick_add_goes_to_the_bottom_of_the_queue() {
        let (s, c, zj, r) = (Store::open_in_memory(), cfg(3), inside(), rec(&[]));
        let first = add(&s, "first", Stage::Queue);
        let mut ap = app(&s, &c);
        ap.quick = Some(Default::default());
        let e = vec![Effect::Add { title: "second".into(), dir: None, labels: vec!["api".into()], experiment: true }];
        run_all(e, &ctx(&s, &c, &zj, &r), &mut Panes::new(0), &mut ap);
        let queue: Vec<_> = s.tasks().unwrap().into_iter().filter(|t| t.stage == Stage::Queue).map(|t| (t.title, t.labels, t.kind)).collect();
        assert_eq!(queue, [("first".into(), vec![], "default".into()), ("second".to_string(), vec!["api".to_string()], "experiment".to_string())]);
        assert!(ap.quick.is_none() && ap.selected != Some(CardKey::Task(first)));
        // A directory that isn't there is refused and the box stays open.
        ap.quick = Some(Default::default());
        let e = vec![Effect::Add { title: "x".into(), dir: Some("/nonexistent/dir".into()), labels: vec![], experiment: false }];
        run_all(e, &ctx(&s, &c, &zj, &r), &mut Panes::new(0), &mut ap);
        assert_eq!(ap.message.as_deref(), Some("No such directory: /nonexistent/dir"));
        assert!(ap.quick.is_some());
        assert_eq!(s.tasks().unwrap().len(), 2);
    }

    #[test]
    fn tick_closes_idle_panes_of_streams_that_left_active() {
        let (s, c, zj) = (Store::open_in_memory(), cfg(3), inside());
        let r = rec(&["terminal_0", "terminal_1", "terminal_2", "terminal_3", "terminal_4", "terminal_5", "terminal_6"]);
        let mk = |stage: Stage, sid: &str, pane_id: &str, state| {
            let id = add(&s, sid, Stage::Active);
            session(&s, sid, Some(id), state);
            pane(&s, sid, pane_id, state);
            if stage != Stage::Active {
                s.transition(id, stage, 9, &|_, _| vec![]).unwrap().unwrap();
            }
        };
        mk(Stage::Review, "idle", "terminal_1", AgentState::Waiting);
        mk(Stage::Parked, "busy", "terminal_2", AgentState::Working);
        // Wrapped up but stuck on a permission prompt: its pane must stay.
        mk(Stage::Review, "asking", "terminal_5", AgentState::Blocked);
        mk(Stage::Parked, "gone", "terminal_6", AgentState::Ended);
        mk(Stage::Active, "active", "terminal_3", AgentState::Waiting);
        // Ended with its pane already gone: nothing to close, but the decision is made.
        mk(Stage::Parked, "nopane", "terminal_8", AgentState::Ended);
        // A stale session whose recorded pane id is now someone else's: other zellij session.
        mk(Stage::Review, "stale", "terminal_4", AgentState::Waiting);
        s.touch_session(SessionUpdate { id: "stale", cwd: "/w", state: AgentState::Waiting, zellij_session: Some("old"), zellij_pane: None }, false).unwrap();
        let mut p = panes(&r);
        tick(&ctx(&s, &c, &zj, &r), &mut p).unwrap();
        let mut closes: Vec<String> = r.calls.borrow().iter().filter(|l| l.contains("--name close")).cloned().collect();
        closes.sort();
        assert_eq!(closes, [format!("{P} close --args pane=terminal_1"), format!("{P} close --args pane=terminal_6")]);
        // Resumed by hand in the user's own pane afterwards: flagged, never closed again.
        pane(&s, "idle", "terminal_7", AgentState::Waiting);
        pane(&s, "nopane", "terminal_8", AgentState::Waiting);
        let r3 = rec(&["terminal_0", "terminal_7", "terminal_8"]);
        p.set_live(panes(&r3).live);
        tick(&ctx(&s, &c, &zj, &r3), &mut p).unwrap();
        assert!(!r3.calls.borrow().iter().any(|l| l.contains("--name close")), "{:?}", r3.calls.borrow());
        // Streams that left Active before the board started are left alone.
        let r2 = rec(&["terminal_0", "terminal_1"]);
        let mut p2 = Panes::new(i64::MAX);
        p2.set_live(panes(&r2).live);
        tick(&ctx(&s, &c, &zj, &r2), &mut p2).unwrap();
        assert!(!r2.calls.borrow().iter().any(|l| l.contains("--name close")));
    }
}
