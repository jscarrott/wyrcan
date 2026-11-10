//! Board state and input: a snapshot of the store, the selection, and keys/mouse turned into effects.
use crate::config::Config;
use crate::gate;
use crate::model::{AgentState, Event, Pr, PrState, Session, SessionKind, Stage, Task};
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

    pub fn of(stage: Stage) -> Option<Group> {
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

/// What the mouse can hit: a card, a column or group (a drop target), or "+ Add task".
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Card(CardKey),
    Group(Group),
    AddTask,
}

/// Where something landed in the last frame, for the mouse.
#[derive(Debug, Clone)]
pub struct Hit {
    pub rect: Rect,
    pub target: Target,
}

/// What the task view shows beyond the card; read from the store by the loop while the view is open.
#[derive(Debug)]
pub struct Detail {
    pub task: Task,
    pub session: Option<Session>,
    pub prs: Vec<Pr>,
    pub repos: Vec<String>,
    pub events: Vec<Event>,
}

pub fn detail(store: &Store, id: i64) -> anyhow::Result<Option<Detail>> {
    let Some(task) = store.task(id)? else { return Ok(None) };
    let (session, prs, repos, events) = (store.current_session(id)?, store.prs(id)?, store.task_repos(id)?, store.events(id)?);
    Ok(Some(Detail { task, session, prs, repos, events }))
}

#[derive(Debug)]
pub struct TaskView {
    pub id: i64,
    pub scroll: u16,
    pub detail: Option<Detail>,
}

/// The quick-add line; `cursor` counts chars.
#[derive(Debug, Default)]
pub struct Input {
    pub text: String,
    pub cursor: usize,
}

impl Input {
    fn byte(&self, i: usize) -> usize {
        self.text.char_indices().nth(i).map_or(self.text.len(), |(b, _)| b)
    }

    fn key(&mut self, code: KeyCode) {
        let n = self.text.chars().count();
        match code {
            KeyCode::Char(c) => {
                let b = self.byte(self.cursor);
                self.text.insert(b, c);
                self.cursor += 1;
            }
            KeyCode::Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                let b = self.byte(self.cursor);
                self.text.remove(b);
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(n),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = n,
            _ => {}
        }
    }
}

/// `@word` is a label, `#path` the directory (`~` expanded), the rest the title. Only at the start of a word.
pub fn parse_quick_add(s: &str) -> (String, Option<String>, Vec<String>) {
    let (mut title, mut dir, mut labels) = (vec![], None, vec![]);
    for w in s.split_whitespace() {
        match (w.strip_prefix('@'), w.strip_prefix('#')) {
            (Some(l), _) if !l.is_empty() => labels.push(l.to_string()),
            (_, Some(d)) if !d.is_empty() => dir = Some(crate::config::expand_home(d)),
            _ => title.push(w),
        }
    }
    (title.join(" "), dir, labels)
}

/// A card being dragged, and the drop target under the pointer.
#[derive(Debug)]
pub struct Drag {
    pub key: CardKey,
    pub over: Option<Group>,
}

pub struct App {
    pub snap: Snapshot,
    pub selected: Option<CardKey>,
    pub message: Option<String>,
    pub prompt: Option<Prompt>,
    pub confirm: Option<Pending>,
    pub modal: Option<TaskView>,
    pub quick: Option<Input>,
    pub drag: Option<Drag>,
    pub hits: Vec<Hit>,
    /// The wide form is on screen (set by `ui::draw` from the width).
    pub wide: bool,
    /// Wide form: each column's scroll offset in lines, by `Group as usize`.
    pub scroll: [usize; 5],
    /// The selection the wide form last scrolled into view; a new one gets scrolled to once.
    pub followed: Option<CardKey>,
}

impl App {
    pub fn new(snap: Snapshot) -> App {
        let mut a = App {
            snap,
            selected: None,
            message: None,
            prompt: None,
            confirm: None,
            modal: None,
            quick: None,
            drag: None,
            hits: vec![],
            wide: false,
            scroll: [0; 5],
            followed: None,
        };
        a.selected = a.flat().first().map(|c| c.key());
        a
    }

    pub fn group_of(&self, k: &CardKey) -> Option<Group> {
        self.snap.groups.iter().find(|g| g.1.iter().any(|c| c.key() == *k)).map(|g| g.0)
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
        // The task view follows its card; once the card leaves the board (done, dropped), it closes.
        if self.modal.as_ref().is_some_and(|v| self.selected != Some(CardKey::Task(v.id))) {
            self.modal = None;
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

    fn hit(&self, at: Position, want: fn(&Target) -> bool) -> Option<Target> {
        self.hits.iter().find(|h| want(&h.target) && h.rect.contains(at)).map(|h| h.target.clone())
    }

    fn group_at(&self, at: Position) -> Option<Group> {
        match self.hit(at, |t| matches!(t, Target::Group(_))) {
            Some(Target::Group(g)) => Some(g),
            _ => None,
        }
    }

    pub fn on_mouse(&mut self, m: MouseEvent) -> Vec<Effect> {
        if self.confirm.is_some() || self.prompt.is_some() || self.quick.is_some() {
            return vec![];
        }
        let at = Position { x: m.column, y: m.row };
        let wheel = match m.kind {
            MouseEventKind::ScrollDown => 1,
            MouseEventKind::ScrollUp => -1,
            _ => 0,
        };
        if let Some(v) = self.modal.as_mut() {
            v.scroll = v.scroll.saturating_add_signed(wheel * 3);
            return vec![];
        }
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.message = None;
                match self.hit(at, |t| !matches!(t, Target::Group(_))) {
                    Some(Target::AddTask) => {
                        self.quick = Some(Input::default());
                        vec![]
                    }
                    Some(Target::Card(k)) => {
                        self.drag = Some(Drag { key: k.clone(), over: None });
                        match self.flat().iter().position(|c| c.key() == k) {
                            Some(i) => self.select(i),
                            None => vec![],
                        }
                    }
                    _ => vec![],
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let over = self.group_at(at);
                if let Some(d) = self.drag.as_mut() {
                    d.over = over;
                }
                vec![]
            }
            MouseEventKind::Up(MouseButton::Left) => match (self.drag.take(), self.group_at(at)) {
                (Some(d), Some(g)) if self.selected.as_ref() == Some(&d.key) => self.drop_on(g),
                _ => vec![],
            },
            _ if wheel != 0 && self.wide => {
                if let Some(g) = self.group_at(at) {
                    let o = &mut self.scroll[g as usize];
                    *o = o.saturating_add_signed(wheel as isize * 3);
                }
                vec![]
            }
            _ if wheel != 0 => self.step(wheel as isize),
            _ => vec![],
        }
    }

    /// A card dropped on a column asks for that stage the way the keys do; the store has the last word.
    fn drop_on(&mut self, g: Group) -> Vec<Effect> {
        let Some((t, _)) = self.task() else { return vec![] };
        if Group::of(t.stage) == Some(g) {
            return vec![];
        }
        match g {
            Group::Active if t.stage == Stage::Queue => vec![Effect::Start(t.id)],
            Group::Active => vec![Effect::Resume(t.id)],
            Group::Review => vec![Effect::ToReview(t.id)],
            Group::Queue => vec![Effect::Requeue(t.id)],
            Group::Parked => self.guarded("Park", Effect::Park),
            Group::Untracked => vec![],
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
        if let Some(q) = self.quick.as_mut() {
            match k.code {
                KeyCode::Esc => self.quick = None,
                KeyCode::Enter => {
                    let (title, dir, labels) = parse_quick_add(&q.text);
                    if !title.is_empty() {
                        // Stays open until the add succeeds, so a bad directory can be fixed.
                        return vec![Effect::Add { title, dir, labels }];
                    }
                    self.quick = None;
                }
                c => q.key(c),
            }
            return vec![];
        }
        // In the task view these act on the card in view, as on the board; other keys stay in the view.
        let passes = matches!(k.code, KeyCode::Char('e' | 'o' | 'w' | 'P' | 'x' | 'D'));
        if let Some(v) = self.modal.as_mut().filter(|_| !passes) {
            match k.code {
                KeyCode::Esc => self.modal = None,
                KeyCode::Up | KeyCode::Char('k') => v.scroll = v.scroll.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => v.scroll += 1,
                _ => {}
            }
            return vec![];
        }
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
            KeyCode::Char(' ') => {
                if let Some((t, _)) = self.task() {
                    self.modal = Some(TaskView { id: t.id, scroll: 0, detail: None });
                }
                vec![]
            }
            KeyCode::Char('+') => {
                self.quick = Some(Input::default());
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

    /// Built by hand so tests never read the user's config file.
    pub fn cfg(max: usize) -> Config {
        Config {
            token_file: "/nonexistent/token".into(),
            project: "Claude Board".into(),
            todoist: false,
            state_dir: "/nonexistent/state".into(),
            claude_dir: "/c".into(),
            max_active: max,
            file: String::new(),
        }
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
    fn parse_quick_add_parses_tokens() {
        let home = crate::config::home().to_string_lossy().into_owned();
        let (t, d, l) = parse_quick_add("Fix login @delta @api #~/git-work/delta");
        assert_eq!((t.as_str(), d, l), ("Fix login", Some(format!("{home}/git-work/delta")), vec!["delta".to_string(), "api".to_string()]));
        // `@` and `#` only count at the start of a word; a bare one is text.
        assert_eq!(parse_quick_add("email me@example.com about issue#4 @ #"), ("email me@example.com about issue#4 @ #".into(), None, vec![]));
        assert_eq!(parse_quick_add("   "), (String::new(), None, vec![]));
        assert_eq!(parse_quick_add("x #~").1, Some(home));
    }

    fn type_into(ap: &mut App, s: &str) {
        for c in s.chars() {
            ap.on_key_event(key(c));
        }
    }

    #[test]
    fn quick_add_edits_and_submits() {
        let s = Store::open_in_memory();
        let mut ap = app(&s, &cfg(3));
        ap.on_key_event(key('+'));
        type_into(&mut ap, "fix login @api");
        // Board keys don't fire underneath: 'q' and 'D' were typed, not obeyed.
        ap.on_key_event(KeyCode::Home.into());
        type_into(&mut ap, "Dq ");
        ap.on_key_event(KeyCode::End.into());
        ap.on_key_event(KeyCode::Left.into());
        ap.on_key_event(KeyCode::Backspace.into());
        assert_eq!(ap.quick.as_ref().unwrap().text, "Dq fix login @ai");
        assert!(ap.confirm.is_none());
        let e = ap.on_key_event(KeyCode::Enter.into());
        assert_eq!(e, [Effect::Add { title: "Dq fix login".into(), dir: None, labels: vec!["ai".into()] }]);
        ap.on_key_event(KeyCode::Esc.into());
        assert!(ap.quick.is_none());
    }

    #[test]
    fn empty_quick_add_adds_nothing() {
        let s = Store::open_in_memory();
        let mut ap = app(&s, &cfg(3));
        ap.on_key_event(key('+'));
        type_into(&mut ap, "  @label  ");
        assert!(ap.on_key_event(KeyCode::Enter.into()).is_empty());
        assert!(ap.quick.is_none());
    }

    #[test]
    fn task_view_captures_keys_but_passes_actions() {
        let s = Store::open_in_memory();
        let id = add(&s, "t", Stage::Active);
        let mut ap = app(&s, &cfg(3));
        ap.on_key_event(key(' '));
        assert_eq!(ap.modal.as_ref().unwrap().id, id);
        assert!(ap.on_key_event(key('q')).is_empty(), "q doesn't quit under the modal");
        ap.on_key_event(KeyCode::Down.into());
        assert_eq!(ap.modal.as_ref().unwrap().scroll, 1);
        assert_eq!(ap.on_key_event(key('w')), [Effect::Wrap(id)]);
        assert_eq!(ap.on_key_event(key('P')), [Effect::Park(id)]);
        ap.on_key_event(KeyCode::Esc.into());
        assert!(ap.modal.is_none());
    }

    #[test]
    fn click_selects_and_wheel_scrolls() {
        let s = Store::open_in_memory();
        let (a, b) = (add(&s, "a", Stage::Queue), add(&s, "b", Stage::Queue));
        let mut ap = app(&s, &cfg(3));
        ap.hits = vec![
            Hit { rect: Rect::new(0, 0, 10, 3), target: Target::Card(CardKey::Task(a)) },
            Hit { rect: Rect::new(0, 3, 10, 3), target: Target::Card(CardKey::Task(b)) },
            Hit { rect: Rect::new(0, 0, 10, 8), target: Target::Group(Group::Queue) },
            Hit { rect: Rect::new(0, 6, 10, 1), target: Target::AddTask },
        ];
        let ev = |kind, row| MouseEvent { kind, column: 2, row, modifiers: KeyModifiers::NONE };
        assert_eq!(ap.on_mouse(ev(MouseEventKind::Down(MouseButton::Left), 4)), [Effect::Show(b)]);
        assert_eq!(ap.selected, Some(CardKey::Task(b)));
        // Narrow: the wheel walks the list.
        ap.on_mouse(ev(MouseEventKind::ScrollUp, 0));
        assert_eq!(ap.selected, Some(CardKey::Task(a)));
        // Wide: it scrolls the column under the pointer and leaves the selection alone.
        ap.wide = true;
        ap.on_mouse(ev(MouseEventKind::ScrollDown, 0));
        assert_eq!((ap.selected.clone(), ap.scroll[Group::Queue as usize]), (Some(CardKey::Task(a)), 3));
        ap.on_mouse(ev(MouseEventKind::Down(MouseButton::Left), 6));
        assert!(ap.quick.is_some());
    }
}
