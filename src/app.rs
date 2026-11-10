//! Board state and input: a snapshot of the store, the selection, and keys/mouse turned into effects.
use crate::config::Config;
use crate::gate;
use crate::model::{AgentState, Pr, PrState, Session, SessionKind, Stage, Task};
use crate::store::Store;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Active,
    Review,
    Parked,
    Queue,
    Untracked,
}

impl Group {
    pub const ALL: [Group; 5] = [Group::Active, Group::Review, Group::Parked, Group::Queue, Group::Untracked];

    pub fn name(self) -> &'static str {
        match self {
            Group::Active => "ACTIVE",
            Group::Review => "REVIEW",
            Group::Parked => "PARKED",
            Group::Queue => "QUEUE",
            Group::Untracked => "UNTRACKED",
        }
    }

    fn of(stage: Stage) -> Option<Group> {
        match stage {
            Stage::Active => Some(Group::Active),
            Stage::Review => Some(Group::Review),
            Stage::Parked => Some(Group::Parked),
            Stage::Queue => Some(Group::Queue),
            Stage::Done | Stage::Dropped => None,
        }
    }
}

/// One card; Untracked cards have no task.
#[derive(Clone, Debug)]
pub struct Card {
    pub task: Option<Task>,
    /// The task's current session, or the untracked session itself.
    pub session: Option<Session>,
    pub prs: Vec<Pr>,
    /// Review-gate checks still failing (Active cards only).
    pub missing: Vec<String>,
    pub flag: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CardKey {
    Task(i64),
    Session(String),
}

impl Card {
    pub fn key(&self) -> CardKey {
        match (&self.task, &self.session) {
            (Some(t), _) => CardKey::Task(t.id),
            (None, s) => CardKey::Session(s.as_ref().map(|s| s.id.clone()).unwrap_or_default()),
        }
    }
}

#[derive(Default)]
pub struct Snapshot {
    pub groups: Vec<(Group, Vec<Card>)>,
    pub active: usize,
    pub max_active: usize,
    pub blocked: usize,
    pub writers: Vec<Session>,
}

pub fn snapshot(store: &Store, cfg: &Config) -> anyhow::Result<Snapshot> {
    let sessions = store.sessions()?;
    let mut groups: Vec<(Group, Vec<Card>)> = Group::ALL.iter().map(|g| (*g, vec![])).collect();
    for t in store.tasks()? {
        let Some(g) = Group::of(t.stage) else { continue };
        let session = sessions.iter().find(|s| s.task_id == Some(t.id) && s.current).cloned();
        let prs = store.prs(t.id)?;
        // Display only, so PR URLs aren't verified with `gh` here; the real gate does that.
        let missing = if t.stage == Stage::Active { gate::evaluate(store, &t, &cfg.gate_checks(&t.kind), &|_| true) } else { vec![] };
        let flag = flag(&t, session.as_ref(), &prs).map(String::from);
        groups[g as usize].1.push(Card { task: Some(t), session, prs, missing, flag });
    }
    let rank = |c: &Card| match c.session.as_ref().map(|s| s.state) {
        Some(AgentState::Blocked) => 0,
        Some(AgentState::Waiting) => 1,
        Some(AgentState::Working) => 2,
        _ => 3,
    };
    groups[0].1.sort_by_key(rank);
    groups[4].1 = store.untracked()?.into_iter().map(|s| Card { task: None, session: Some(s), prs: vec![], missing: vec![], flag: None }).collect();
    let writers: Vec<Session> = sessions.into_iter().filter(|s| s.kind == SessionKind::Pen && s.state != AgentState::Ended).collect();
    let blocked = groups
        .iter()
        .flat_map(|g| &g.1)
        .filter_map(|c| c.session.as_ref())
        .chain(&writers)
        .filter(|s| s.state == AgentState::Blocked)
        .count();
    Ok(Snapshot { active: groups[0].1.len(), max_active: cfg.max_active, blocked, writers, groups })
}

fn flag(t: &Task, s: Option<&Session>, prs: &[Pr]) -> Option<&'static str> {
    if t.stage != Stage::Active && s.is_some_and(|s| s.state == AgentState::Working) {
        return Some("working outside Active");
    }
    if t.stage != Stage::Review {
        return None;
    }
    match gate::worst(prs) {
        Some(PrState::ChangesRequested) => Some("changes requested, waiting for a slot"),
        Some(PrState::Closed) => Some("PR closed"),
        _ if gate::ready_to_close(prs) => Some("ready to close"),
        _ => None,
    }
}

#[derive(Debug, PartialEq)]
pub enum Effect {
    Start(i64),
    Resume(i64),
    Park(i64),
    Requeue(i64),
    ToReview(i64),
    Done(i64),
    Drop(i64),
    Adopt { session: String, title: String },
    /// Saves the directory typed into the prompt; the Start/Resume after it then goes ahead.
    SetDir { id: i64, dir: String },
    Show(i64),
    Enter(i64),
    Fullscreen(i64),
    ToggleBoardWide,
    Wrap(i64),
    SpawnPen,
    OpenExplainer(i64),
    OpenPr(i64),
    #[allow(dead_code)] // emitted by quick add, Task 7
    Add { title: String, dir: Option<String>, labels: Vec<String> },
    Quit,
}

/// An action waiting for a y/n answer.
#[derive(Debug)]
pub struct Pending {
    pub prompt: String,
    effect: Effect,
}

#[derive(Debug)]
enum PromptKind {
    Dir { id: i64, resume: bool },
    Adopt { session: String },
}

/// A one-line text box in the footer.
#[derive(Debug)]
pub struct Prompt {
    pub label: String,
    pub text: String,
    kind: PromptKind,
}

impl Prompt {
    pub fn dir(id: i64, resume: bool, title: &str, prefill: String) -> Prompt {
        Prompt { label: format!("Directory for '{title}':"), text: prefill, kind: PromptKind::Dir { id, resume } }
    }
}

/// Where a card landed in the last frame, for mouse clicks.
#[derive(Debug, Clone)]
pub struct Hit {
    pub rect: Rect,
    pub key: CardKey,
}

pub struct App {
    pub snap: Snapshot,
    pub selected: Option<CardKey>,
    pub message: Option<String>,
    pub prompt: Option<Prompt>,
    pub confirm: Option<Pending>,
    #[allow(dead_code)] // the task view, Task 7
    pub modal: Option<i64>,
    pub hits: Vec<Hit>,
    /// The wide form is on screen (set by the loop from the terminal width).
    pub wide: bool,
}

impl App {
    pub fn new(snap: Snapshot) -> App {
        let mut a = App { snap, selected: None, message: None, prompt: None, confirm: None, modal: None, hits: vec![], wide: false };
        a.selected = a.flat().first().map(|c| c.key());
        a
    }

    /// Every card, in screen order.
    fn flat(&self) -> Vec<&Card> {
        self.snap.groups.iter().flat_map(|g| &g.1).collect()
    }

    fn index(&self) -> Option<usize> {
        let k = self.selected.as_ref()?;
        self.flat().iter().position(|c| c.key() == *k)
    }

    /// Keeps the selection on the same card; if it left the board, on the card now at its place.
    pub fn set_snapshot(&mut self, s: Snapshot) {
        let old = self.index();
        self.snap = s;
        if self.index().is_none() {
            let flat = self.flat();
            self.selected = flat.get(old.unwrap_or(0).min(flat.len().saturating_sub(1))).map(|c| c.key());
        }
    }

    pub fn selected_card(&self) -> Option<&Card> {
        self.flat().get(self.index()?).copied()
    }

    fn task(&self) -> Option<(Task, Option<AgentState>)> {
        let c = self.selected_card()?;
        Some((c.task.clone()?, c.session.as_ref().map(|s| s.state)))
    }

    fn select(&mut self, i: usize) -> Vec<Effect> {
        let Some(c) = self.flat().get(i).copied() else { return vec![] };
        let (key, id) = (c.key(), c.task.as_ref().map(|t| t.id));
        if self.selected.as_ref() == Some(&key) {
            return vec![];
        }
        self.selected = Some(key);
        match id {
            // Beside the stack, the selected stream's pane comes to the front.
            Some(id) if !self.wide => vec![Effect::Show(id)],
            _ => vec![],
        }
    }

    fn step(&mut self, d: isize) -> Vec<Effect> {
        let n = self.flat().len() as isize;
        let i = self.index().map_or(0, |i| (i as isize + d).clamp(0, (n - 1).max(0)) as usize);
        self.select(i)
    }

    /// To the first card of the previous/next group that has any.
    fn jump_group(&mut self, d: isize) -> Vec<Effect> {
        let (mut start, mut firsts, mut cur) = (0, vec![], 0);
        let sel = self.index();
        for (gi, (_, cards)) in self.snap.groups.iter().enumerate() {
            if sel.is_some_and(|i| (start..start + cards.len()).contains(&i)) {
                cur = gi;
            }
            if !cards.is_empty() {
                firsts.push((gi, start));
            }
            start += cards.len();
        }
        let target = if d > 0 { firsts.iter().find(|f| f.0 > cur) } else { firsts.iter().rev().find(|f| f.0 < cur) };
        match target {
            Some(&(_, i)) => self.select(i),
            None => vec![],
        }
    }

    pub fn on_mouse(&mut self, m: MouseEvent) -> Vec<Effect> {
        if self.confirm.is_some() || self.prompt.is_some() {
            return vec![];
        }
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let at = Position { x: m.column, y: m.row };
                let Some(k) = self.hits.iter().find(|h| h.rect.contains(at)).map(|h| h.key.clone()) else { return vec![] };
                self.message = None;
                match self.flat().iter().position(|c| c.key() == k) {
                    Some(i) => self.select(i),
                    None => vec![],
                }
            }
            MouseEventKind::ScrollDown => self.step(1),
            MouseEventKind::ScrollUp => self.step(-1),
            _ => vec![],
        }
    }

    /// Raw mode swallows SIGINT, so Ctrl-C arrives as a key.
    pub fn on_key_event(&mut self, k: KeyEvent) -> Vec<Effect> {
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            return vec![Effect::Quit];
        }
        if self.prompt.is_some() {
            return self.prompt_key(k.code);
        }
        if let Some(p) = self.confirm.take() {
            return if k.code == KeyCode::Char('y') { vec![p.effect] } else { vec![] };
        }
        self.message = None;
        let code = match (k.modifiers.contains(KeyModifiers::SHIFT), k.code) {
            (true, KeyCode::Left) => KeyCode::Char('H'),
            (true, KeyCode::Right) => KeyCode::Char('L'),
            (_, c) => c,
        };
        let id = self.task().map(|t| t.0.id);
        let on = |f: fn(i64) -> Effect| id.map(f).into_iter().collect();
        match code {
            KeyCode::Up | KeyCode::Char('k') => self.step(-1),
            KeyCode::Down | KeyCode::Char('j') => self.step(1),
            KeyCode::Left | KeyCode::Char('h') => self.jump_group(-1),
            KeyCode::Right | KeyCode::Char('l') => self.jump_group(1),
            KeyCode::Char('H') => self.move_by(-1),
            KeyCode::Char('L') => self.move_by(1),
            KeyCode::Char('n') => self.next(),
            KeyCode::Char('s') => self.start(),
            KeyCode::Char('P') => self.guarded("Park", Effect::Park),
            KeyCode::Char('x') => self.guarded("Done", Effect::Done),
            KeyCode::Char('D') => {
                if let Some((t, _)) = self.task() {
                    self.ask(format!("Drop '{}'? y/n", t.title), Effect::Drop(t.id));
                }
                vec![]
            }
            KeyCode::Char('a') => self.adopt(),
            KeyCode::Char('w') => on(Effect::Wrap),
            KeyCode::Enter => on(Effect::Enter),
            KeyCode::Char('f') => on(Effect::Fullscreen),
            KeyCode::Char('e') => on(Effect::OpenExplainer),
            KeyCode::Char('o') => on(Effect::OpenPr),
            KeyCode::Char('v') => vec![Effect::ToggleBoardWide],
            KeyCode::Char('p') => vec![Effect::SpawnPen],
            KeyCode::Char(' ' | '+') => {
                self.message = Some("Task view and quick add come in the next step.".into());
                vec![]
            }
            KeyCode::Char('q') => vec![Effect::Quit],
            _ => vec![],
        }
    }

    fn prompt_key(&mut self, code: KeyCode) -> Vec<Effect> {
        let Some(p) = self.prompt.as_mut() else { return vec![] };
        match code {
            KeyCode::Char(c) => p.text.push(c),
            KeyCode::Backspace => {
                p.text.pop();
            }
            KeyCode::Esc => self.prompt = None,
            KeyCode::Enter => {
                let p = self.prompt.take().expect("checked above");
                let text = p.text.trim().to_string();
                if text.is_empty() {
                    return vec![];
                }
                return match p.kind {
                    PromptKind::Dir { id, resume } => {
                        vec![Effect::SetDir { id, dir: text }, if resume { Effect::Resume(id) } else { Effect::Start(id) }]
                    }
                    PromptKind::Adopt { session } => vec![Effect::Adopt { session, title: text }],
                };
            }
            _ => {}
        }
        vec![]
    }

    fn ask(&mut self, prompt: String, effect: Effect) {
        self.confirm = Some(Pending { prompt, effect });
    }

    /// Asks first when the agent is busy.
    fn guarded(&mut self, verb: &str, effect: fn(i64) -> Effect) -> Vec<Effect> {
        let Some((t, state)) = self.task() else { return vec![] };
        match state {
            Some(s @ (AgentState::Working | AgentState::Blocked)) => {
                self.ask(format!("{verb} '{}'? Agent is {s} — y/n", t.title), effect(t.id));
                vec![]
            }
            _ => vec![effect(t.id)],
        }
    }

    /// One stage along Queue ↔ Active ↔ Review (→ Done); Parked sits beside Active.
    fn move_by(&mut self, d: isize) -> Vec<Effect> {
        let Some((t, _)) = self.task() else { return vec![] };
        match (t.stage, d > 0) {
            (Stage::Queue, true) => vec![Effect::Start(t.id)],
            (Stage::Active | Stage::Parked, false) => vec![Effect::Requeue(t.id)],
            (Stage::Active, true) => vec![Effect::ToReview(t.id)],
            (Stage::Parked, true) | (Stage::Review, false) => vec![Effect::Resume(t.id)],
            (Stage::Review, true) => self.guarded("Done", Effect::Done),
            _ => vec![],
        }
    }

    fn next(&mut self) -> Vec<Effect> {
        let top = self.snap.groups.iter().find(|g| g.0 == Group::Queue).and_then(|g| g.1.first());
        match top.and_then(|c| c.task.as_ref()) {
            Some(t) => vec![Effect::Start(t.id)],
            None => {
                self.message = Some("Queue is empty".into());
                vec![]
            }
        }
    }

    /// Queue cards start; Parked ones resume; an Active one reopens its session if the pane is gone.
    fn start(&mut self) -> Vec<Effect> {
        match self.task() {
            Some((t, _)) if t.stage == Stage::Queue => vec![Effect::Start(t.id)],
            Some((t, _)) if matches!(t.stage, Stage::Parked | Stage::Active) => vec![Effect::Resume(t.id)],
            _ => vec![],
        }
    }

    fn adopt(&mut self) -> Vec<Effect> {
        let Some(s) = self.selected_card().filter(|c| c.task.is_none()).and_then(|c| c.session.as_ref()) else { return vec![] };
        let name = s.cwd.trim_end_matches('/').rsplit('/').next().unwrap_or_default().to_string();
        let kind = PromptKind::Adopt { session: s.id.clone() };
        self.prompt = Some(Prompt { label: "Title for this session:".into(), text: name, kind });
        vec![]
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::store::{NewTask, SessionUpdate};

    pub fn cfg(max: usize) -> Config {
        let mut c = Config::from_env();
        c.max_active = max;
        c.file = String::new();
        c.claude_dir = "/c".into();
        c
    }

    pub fn add(s: &Store, title: &str, stage: Stage) -> i64 {
        let id = s.add_task(NewTask { title, description: "", dir: Some("/w"), labels: &[], todoist_id: None }).unwrap();
        s.conn.execute("UPDATE tasks SET stage=?1 WHERE id=?2", rusqlite::params![stage.as_str(), id]).unwrap();
        id
    }

    pub fn session(s: &Store, sid: &str, task: Option<i64>, state: AgentState) {
        s.touch_session(SessionUpdate { id: sid, cwd: "/w/proj", state, zellij_session: None, zellij_pane: None }, true).unwrap();
        if let Some(t) = task {
            s.link_session(sid, t, "/w/proj").unwrap();
        }
    }

    pub fn app(s: &Store, c: &Config) -> App {
        App::new(snapshot(s, c).unwrap())
    }

    pub fn key(c: char) -> KeyEvent {
        KeyCode::Char(c).into()
    }

    fn select(a: &mut App, id: i64) {
        a.selected = Some(CardKey::Task(id));
    }

    fn ids(sn: &Snapshot, g: Group) -> Vec<i64> {
        sn.groups.iter().find(|x| x.0 == g).unwrap().1.iter().map(|c| c.task.as_ref().unwrap().id).collect()
    }

    fn flag_of(sn: &Snapshot, id: i64) -> Option<String> {
        sn.groups.iter().flat_map(|g| &g.1).find(|c| c.key() == CardKey::Task(id)).unwrap().flag.clone()
    }

    #[test]
    fn snapshot_sorts_blocked_first_and_counts() {
        let s = Store::open_in_memory();
        let (work, wait, block, none) =
            (add(&s, "w", Stage::Active), add(&s, "a", Stage::Active), add(&s, "b", Stage::Active), add(&s, "n", Stage::Active));
        session(&s, "s1", Some(work), AgentState::Working);
        session(&s, "s2", Some(wait), AgentState::Waiting);
        session(&s, "s3", Some(block), AgentState::Blocked);
        let q = add(&s, "q", Stage::Queue);
        s.register_pen("pen", "/w").unwrap();
        s.touch_session(SessionUpdate { id: "pen", cwd: "/w", state: AgentState::Blocked, zellij_session: None, zellij_pane: None }, false).unwrap();
        let sn = snapshot(&s, &cfg(5)).unwrap();
        assert_eq!(sn.groups.iter().map(|g| g.0).collect::<Vec<_>>(), Group::ALL);
        assert_eq!(ids(&sn, Group::Active), [block, wait, work, none]);
        assert_eq!(ids(&sn, Group::Queue), [q]);
        assert_eq!((sn.active, sn.max_active, sn.blocked, sn.writers.len()), (4, 5, 2, 1));
        // Active cards carry what the gate still lacks.
        let c = &sn.groups[0].1[0];
        assert_eq!(c.missing, ["no explainer"]);
    }

    #[test]
    fn snapshot_flags() {
        let s = Store::open_in_memory();
        let outside = add(&s, "o", Stage::Parked);
        session(&s, "s1", Some(outside), AgentState::Working);
        let review = |st: Option<PrState>, waiver: Option<&str>| {
            let id = add(&s, "r", Stage::Review);
            if st.is_some() || waiver.is_some() {
                s.set_pr(id, "/r/app", Some("u"), waiver).unwrap();
                s.set_pr_state(id, "/r/app", st.unwrap_or(PrState::Waiting), "sam").unwrap();
            }
            id
        };
        let changes = review(Some(PrState::ChangesRequested), None);
        let closed = review(Some(PrState::Closed), None);
        let merged = review(Some(PrState::Merged), None);
        let waived = review(None, Some("dotfiles only"));
        let waiting = review(Some(PrState::Waiting), None);
        let sn = snapshot(&s, &cfg(3)).unwrap();
        assert_eq!(flag_of(&sn, outside).as_deref(), Some("working outside Active"));
        assert_eq!(flag_of(&sn, changes).as_deref(), Some("changes requested, waiting for a slot"));
        assert_eq!(flag_of(&sn, closed).as_deref(), Some("PR closed"));
        assert_eq!(flag_of(&sn, merged).as_deref(), Some("ready to close"));
        assert_eq!(flag_of(&sn, waived).as_deref(), Some("ready to close"));
        assert_eq!(flag_of(&sn, waiting), None);
    }

    #[test]
    fn selection_follows_card_across_refresh() {
        let s = Store::open_in_memory();
        let c = cfg(3);
        let (a, b) = (add(&s, "a", Stage::Queue), add(&s, "b", Stage::Queue));
        let mut ap = app(&s, &c);
        assert_eq!(ap.selected, Some(CardKey::Task(a)));
        ap.on_key_event(key('j'));
        assert_eq!(ap.selected, Some(CardKey::Task(b)));
        // b moves to Active (top of the list): the selection goes with it.
        s.transition(b, Stage::Active, 3, &|_, _| vec![]).unwrap().unwrap();
        ap.set_snapshot(snapshot(&s, &c).unwrap());
        assert_eq!(ap.selected, Some(CardKey::Task(b)));
        assert_eq!(ap.selected_card().unwrap().task.as_ref().unwrap().stage, Stage::Active);
        // When the card leaves the board, the selection stays at its place in the list.
        s.transition(b, Stage::Dropped, 3, &|_, _| vec![]).unwrap().unwrap();
        ap.set_snapshot(snapshot(&s, &c).unwrap());
        assert_eq!(ap.selected, Some(CardKey::Task(a)));
    }

    #[test]
    fn drop_asks_first() {
        let s = Store::open_in_memory();
        let id = add(&s, "fix login", Stage::Queue);
        let mut ap = app(&s, &cfg(3));
        assert!(ap.on_key_event(key('D')).is_empty());
        assert_eq!(ap.confirm.as_ref().unwrap().prompt, "Drop 'fix login'? y/n");
        assert!(ap.on_key_event(key('n')).is_empty());
        assert!(ap.confirm.is_none());
        ap.on_key_event(key('D'));
        assert_eq!(ap.on_key_event(key('y')), [Effect::Drop(id)]);
    }

    #[test]
    fn park_asks_when_working() {
        let s = Store::open_in_memory();
        let (busy, idle) = (add(&s, "busy", Stage::Active), add(&s, "idle", Stage::Active));
        session(&s, "s1", Some(busy), AgentState::Working);
        session(&s, "s2", Some(idle), AgentState::Waiting);
        let mut ap = app(&s, &cfg(3));
        select(&mut ap, busy);
        assert!(ap.on_key_event(key('P')).is_empty());
        assert_eq!(ap.confirm.as_ref().unwrap().prompt, "Park 'busy'? Agent is working — y/n");
        assert_eq!(ap.on_key_event(key('y')), [Effect::Park(busy)]);
        select(&mut ap, idle);
        assert_eq!(ap.on_key_event(key('P')), [Effect::Park(idle)]);
    }

    #[test]
    fn keys_move_along_the_stages() {
        let s = Store::open_in_memory();
        let (q, a, r, p) = (add(&s, "q", Stage::Queue), add(&s, "a", Stage::Active), add(&s, "r", Stage::Review), add(&s, "p", Stage::Parked));
        let mut ap = app(&s, &cfg(3));
        let shift = |c| KeyEvent::new(c, KeyModifiers::SHIFT);
        select(&mut ap, q);
        assert_eq!(ap.on_key_event(key('L')), [Effect::Start(q)]);
        assert!(ap.on_key_event(key('H')).is_empty());
        select(&mut ap, a);
        assert_eq!(ap.on_key_event(shift(KeyCode::Right)), [Effect::ToReview(a)]);
        assert_eq!(ap.on_key_event(shift(KeyCode::Left)), [Effect::Requeue(a)]);
        select(&mut ap, r);
        assert_eq!(ap.on_key_event(key('L')), [Effect::Done(r)]);
        assert_eq!(ap.on_key_event(key('H')), [Effect::Resume(r)]);
        select(&mut ap, p);
        assert_eq!(ap.on_key_event(key('s')), [Effect::Resume(p)]);
        assert_eq!(ap.on_key_event(key('n')), [Effect::Start(q)]);
    }

    #[test]
    fn narrow_selection_change_shows_the_pane() {
        let s = Store::open_in_memory();
        let (a, b) = (add(&s, "a", Stage::Queue), add(&s, "b", Stage::Queue));
        let mut ap = app(&s, &cfg(3));
        assert_eq!(ap.on_key_event(key('j')), [Effect::Show(b)]);
        ap.wide = true;
        assert!(ap.on_key_event(key('k')).is_empty());
        assert_eq!(ap.selected, Some(CardKey::Task(a)));
    }

    #[test]
    fn untracked_card_can_be_adopted() {
        let s = Store::open_in_memory();
        session(&s, "u1", None, AgentState::Waiting);
        s.mark_edited("u1", "/w/proj", "git main").unwrap();
        let mut ap = app(&s, &cfg(3));
        assert_eq!(ap.selected, Some(CardKey::Session("u1".into())));
        assert!(ap.on_key_event(key('a')).is_empty());
        let p = ap.prompt.as_ref().unwrap();
        assert_eq!((p.label.as_str(), p.text.as_str()), ("Title for this session:", "proj"));
        for c in "-x".chars() {
            ap.on_key_event(key(c));
        }
        assert_eq!(ap.on_key_event(KeyCode::Enter.into()), [Effect::Adopt { session: "u1".into(), title: "proj-x".into() }]);
        assert!(ap.prompt.is_none());
    }

    #[test]
    fn ctrl_c_quits_even_while_confirming() {
        let s = Store::open_in_memory();
        add(&s, "t", Stage::Queue);
        let mut ap = app(&s, &cfg(3));
        ap.on_key_event(key('D'));
        assert!(ap.confirm.is_some());
        assert_eq!(ap.on_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)), [Effect::Quit]);
        ap.confirm = None;
        ap.prompt = Some(Prompt::dir(1, false, "t", String::new()));
        assert_eq!(ap.on_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)), [Effect::Quit]);
    }

    #[test]
    fn click_selects_and_wheel_scrolls() {
        let s = Store::open_in_memory();
        let (a, b) = (add(&s, "a", Stage::Queue), add(&s, "b", Stage::Queue));
        let mut ap = app(&s, &cfg(3));
        ap.wide = true;
        ap.hits = vec![Hit { rect: Rect::new(0, 0, 10, 3), key: CardKey::Task(a) }, Hit { rect: Rect::new(0, 3, 10, 3), key: CardKey::Task(b) }];
        let ev = |kind, row| MouseEvent { kind, column: 2, row, modifiers: KeyModifiers::NONE };
        ap.on_mouse(ev(MouseEventKind::Down(MouseButton::Left), 4));
        assert_eq!(ap.selected, Some(CardKey::Task(b)));
        ap.on_mouse(ev(MouseEventKind::ScrollUp, 0));
        assert_eq!(ap.selected, Some(CardKey::Task(a)));
    }
}
