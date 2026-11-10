use crate::board::{self, Board, Row, Section, SectionIds};
use crate::state::{AgentRecord, AgentState};
use crate::todoist::Task;
use crate::zellij::{self, Jump};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use std::collections::HashMap;

#[derive(Debug, PartialEq)]
pub enum Effect {
    SpawnNext(String),
    SpawnPen,
    Jump(Jump),
    Resume { session: String, cwd: String },
    Complete { task_id: String, session: Option<String> },
    Move { task_id: String, section_id: String },
    LoadComment(String),
    OpenInTodoist(String),
    Refresh,
    Quit,
}

/// An action waiting for a y/n answer.
#[derive(Debug)]
pub struct Pending {
    pub prompt: String,
    effect: Effect,
}

/// A clickable area from the last frame: a card (index) or a column's empty space (None).
#[derive(Debug, Clone, Copy)]
pub struct Hit {
    pub rect: Rect,
    pub section: Section,
    pub index: Option<usize>,
}

pub struct App {
    pub board: Board,
    pub selected: (Section, usize),
    pub message: Option<String>,
    pub confirm: Option<Pending>,
    pub fetch_error: Option<String>,
    /// The detail view of the selected card is open.
    pub expanded: bool,
    pub scroll: u16,
    /// Latest comment per task id; missing = not loaded yet.
    pub comments: HashMap<String, Option<String>>,
    /// Column a mouse drag started in, and the column it is over.
    pub drag: Option<Section>,
    pub hover: Option<Section>,
    /// Cards before columns, so a card wins over its column.
    pub hits: Vec<Hit>,
    tasks: Vec<Task>,
    agents: Vec<AgentRecord>,
    ids: SectionIds,
    current_session: Option<String>,
    /// Up next tasks already handed to a spawned /next session that has not claimed them yet.
    spawned: Vec<String>,
}

const SECTIONS: [Section; 3] = [Section::UpNext, Section::InProgress, Section::NeedsReview];

impl App {
    pub fn new(ids: SectionIds, current_session: Option<String>) -> App {
        App {
            board: Board::default(),
            selected: (Section::UpNext, 0),
            message: None,
            confirm: None,
            fetch_error: None,
            expanded: false,
            scroll: 0,
            comments: HashMap::new(),
            drag: None,
            hover: None,
            hits: vec![],
            tasks: vec![],
            agents: vec![],
            ids,
            current_session,
            spawned: vec![],
        }
    }

    pub fn rows(&self, s: Section) -> &[Row] {
        match s {
            Section::UpNext => &self.board.up_next,
            Section::InProgress => &self.board.in_progress,
            Section::NeedsReview => &self.board.needs_review,
        }
    }

    pub fn selected_row(&self) -> Option<&Row> {
        self.rows(self.selected.0).get(self.selected.1)
    }

    /// Rebuilds the board, keeping the cursor on the same task (rows reorder on every refresh).
    fn rebuild(&mut self) {
        let id = self.selected_row().map(|r| r.task.id.clone());
        self.board = board::build(&self.tasks, &self.ids, &self.agents);
        let rows = self.rows(self.selected.0);
        let i = id.and_then(|id| rows.iter().position(|r| r.task.id == id));
        self.selected.1 = i.unwrap_or(self.selected.1.min(rows.len().saturating_sub(1)));
    }

    pub fn on_tasks(&mut self, r: anyhow::Result<Vec<Task>>) {
        match r {
            Ok(t) => {
                self.tasks = t;
                self.fetch_error = None;
                self.rebuild();
                // shortcut: a spawned session that dies before claiming leaves its task skipped until restart.
                let up = &self.board.up_next;
                self.spawned.retain(|id| up.iter().any(|r| &r.task.id == id));
            }
            Err(e) => self.fetch_error = Some(format!("{e:#}")),
        }
    }

    pub fn on_agents(&mut self, agents: Vec<AgentRecord>) {
        self.agents = agents;
        self.rebuild();
    }

    fn shift_section(&mut self, d: isize) {
        let i = SECTIONS.iter().position(|s| *s == self.selected.0).unwrap_or(0) as isize;
        let s = SECTIONS[(i + d).clamp(0, 2) as usize];
        let len = self.rows(s).len();
        self.selected = (s, self.selected.1.min(len.saturating_sub(1)));
        self.scroll = 0;
    }

    fn shift_row(&mut self, d: isize) {
        let max = self.rows(self.selected.0).len().saturating_sub(1) as isize;
        self.selected.1 = (self.selected.1 as isize + d).clamp(0, max) as usize;
        self.scroll = 0;
    }

    fn section_id(&self, s: Section) -> &str {
        match s {
            Section::UpNext => &self.ids.up_next,
            Section::InProgress => &self.ids.in_progress,
            Section::NeedsReview => &self.ids.needs_review,
        }
    }

    /// Moves the selected card one column; past Needs review means complete.
    fn move_by(&mut self, d: isize) -> Vec<Effect> {
        let i = SECTIONS.iter().position(|s| *s == self.selected.0).unwrap_or(0) as isize + d;
        match i {
            ..0 => vec![],
            3.. => self.complete(),
            _ => self.move_to(SECTIONS[i as usize]),
        }
    }

    /// Moves locally at once (the cursor follows the card), then in Todoist.
    fn move_to(&mut self, s: Section) -> Vec<Effect> {
        let Some(id) = self.selected_row().map(|r| r.task.id.clone()) else { return vec![] };
        if s == self.selected.0 {
            return vec![];
        }
        let sid = self.section_id(s).to_string();
        if let Some(t) = self.tasks.iter_mut().find(|t| t.id == id) {
            t.section_id = Some(sid.clone());
        }
        self.selected.0 = s;
        self.rebuild();
        vec![Effect::Move { task_id: id, section_id: sid }]
    }

    fn expand(&mut self) -> Vec<Effect> {
        let Some(id) = self.selected_row().map(|r| r.task.id.clone()) else { return vec![] };
        self.expanded = true;
        self.scroll = 0;
        vec![Effect::LoadComment(id)]
    }

    pub fn on_comment(&mut self, task_id: String, c: Option<String>) {
        self.comments.insert(task_id, c);
    }

    fn hit(&self, x: u16, y: u16) -> Option<Hit> {
        self.hits.iter().copied().find(|h| h.rect.contains(Position { x, y }))
    }

    pub fn on_mouse(&mut self, m: MouseEvent) -> Vec<Effect> {
        if self.confirm.is_some() {
            return vec![];
        }
        let hit = self.hit(m.column, m.row);
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let Some(h) = hit else { return vec![] };
                self.message = None;
                self.expanded = false;
                let len = self.rows(h.section).len();
                let i = h.index.unwrap_or(self.selected.1).min(len.saturating_sub(1));
                self.selected = (h.section, i);
                self.drag = h.index.map(|_| h.section);
            }
            MouseEventKind::Drag(MouseButton::Left) => self.hover = self.drag.and(hit.map(|h| h.section)),
            MouseEventKind::Up(MouseButton::Left) => {
                self.hover = None;
                if let (Some(from), Some(h)) = (self.drag.take(), hit) {
                    if h.section != from {
                        return self.move_to(h.section);
                    }
                }
            }
            MouseEventKind::ScrollDown if self.expanded => self.scroll = self.scroll.saturating_add(1),
            MouseEventKind::ScrollUp if self.expanded => self.scroll = self.scroll.saturating_sub(1),
            MouseEventKind::ScrollDown => self.shift_row(1),
            MouseEventKind::ScrollUp => self.shift_row(-1),
            _ => {}
        }
        vec![]
    }

    /// Raw mode swallows SIGINT, so Ctrl-C arrives as a key.
    pub fn on_key_event(&mut self, k: KeyEvent) -> Vec<Effect> {
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            return vec![Effect::Quit];
        }
        match (k.modifiers.contains(KeyModifiers::SHIFT), k.code) {
            (true, KeyCode::Left) => self.on_key(KeyCode::Char('H')),
            (true, KeyCode::Right) => self.on_key(KeyCode::Char('L')),
            (_, c) => self.on_key(c),
        }
    }

    pub fn on_key(&mut self, key: KeyCode) -> Vec<Effect> {
        if let Some(p) = self.confirm.take() {
            return if key == KeyCode::Char('y') { vec![p.effect] } else { vec![] };
        }
        self.message = None;
        if self.expanded {
            match key {
                KeyCode::Esc | KeyCode::Char(' ') | KeyCode::Char('e') => {
                    self.expanded = false;
                    return vec![];
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.scroll = self.scroll.saturating_sub(1);
                    return vec![];
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.scroll = self.scroll.saturating_add(1);
                    return vec![];
                }
                // The view stays on its card; close it to change cards.
                KeyCode::Left | KeyCode::Right | KeyCode::Char('h') | KeyCode::Char('l') => return vec![],
                _ => {}
            }
        }
        match key {
            KeyCode::Left | KeyCode::Char('h') => self.shift_section(-1),
            KeyCode::Right | KeyCode::Char('l') => self.shift_section(1),
            KeyCode::Up | KeyCode::Char('k') => self.shift_row(-1),
            KeyCode::Down | KeyCode::Char('j') => self.shift_row(1),
            KeyCode::Char('H') => return self.move_by(-1),
            KeyCode::Char('L') => return self.move_by(1),
            KeyCode::Char(' ') | KeyCode::Char('e') => return self.expand(),
            KeyCode::Char('n') => return self.next(),
            KeyCode::Char('p') => return vec![Effect::SpawnPen],
            KeyCode::Char('r') => return vec![Effect::Refresh],
            KeyCode::Char('q') => return vec![Effect::Quit],
            KeyCode::Enter => return self.enter(),
            KeyCode::Char('x') => return self.complete(),
            KeyCode::Char('o') => {
                if let Some(r) = self.selected_row() {
                    return vec![Effect::OpenInTodoist(r.task.id.clone())];
                }
            }
            _ => {}
        }
        vec![]
    }

    fn ask(&mut self, prompt: String, effect: Effect) {
        self.confirm = Some(Pending { prompt, effect });
    }

    fn next(&mut self) -> Vec<Effect> {
        let free = self.board.up_next.iter().find(|r| !self.spawned.contains(&r.task.id));
        let Some(id) = free.map(|r| r.task.id.clone()) else {
            self.message = Some("Up next is empty".into());
            return vec![];
        };
        self.spawned.push(id.clone());
        vec![Effect::SpawnNext(id)]
    }

    fn enter(&mut self) -> Vec<Effect> {
        let Some(row) = self.selected_row() else { return vec![] };
        let title = row.task.content.clone();
        match zellij::jump_target(row.agent.as_ref(), self.current_session.as_deref()) {
            Jump::Resume { session, cwd } => self.ask(format!("Resume '{title}'? y/n"), Effect::Resume { session, cwd }),
            Jump::Elsewhere => self.message = Some("running elsewhere".into()),
            Jump::NoAgent => self.message = Some("no agent".into()),
            j @ Jump::Focus(_) => return vec![Effect::Jump(j)],
        }
        vec![]
    }

    fn complete(&mut self) -> Vec<Effect> {
        let Some(row) = self.selected_row() else { return vec![] };
        let effect = Effect::Complete {
            task_id: row.task.id.clone(),
            session: row.agent.as_ref().map(|a| a.session.clone()),
        };
        match row.agent.as_ref().map(|a| a.agent) {
            Some(s @ (AgentState::Working | AgentState::Blocked)) => {
                let s = format!("{s:?}").to_lowercase();
                let title = &row.task.content;
                self.ask(format!("Complete '{title}'? Agent is {s} — y/n"), effect);
                vec![]
            }
            _ => vec![effect],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Kind;

    fn ids() -> SectionIds {
        SectionIds { up_next: "u".into(), in_progress: "p".into(), needs_review: "r".into() }
    }

    fn task(id: &str) -> Task {
        Task { id: id.into(), content: id.into(), description: String::new(), labels: vec![], section_id: None, child_order: 0 }
    }

    fn agent(state: AgentState) -> AgentRecord {
        AgentRecord {
            session: "sid".into(),
            cwd: "/w".into(),
            task: Some("t1".into()),
            kind: Kind::Task,
            agent: state,
            state_at: 100,
            zellij_session: Some("z".into()),
            zellij_pane: Some("terminal_3".into()),
            repos: vec![],
        }
    }

    /// App whose In progress section holds one row with the given agent.
    fn app_with(state: AgentState) -> App {
        let mut app = App::new(ids(), Some("z".into()));
        app.board.in_progress = vec![Row { task: task("t1"), agent: Some(agent(state)) }];
        app.selected = (Section::InProgress, 0);
        app
    }

    #[test]
    fn next_with_empty_queue_spawns_nothing() {
        let mut app = App::new(ids(), None);
        assert!(app.on_key(KeyCode::Char('n')).is_empty());
        assert_eq!(app.message.as_deref(), Some("Up next is empty"));
    }

    #[test]
    fn complete_working_agent_asks_first() {
        let mut app = app_with(AgentState::Working);
        assert!(app.on_key(KeyCode::Char('x')).is_empty());
        assert_eq!(app.confirm.as_ref().unwrap().prompt, "Complete 't1'? Agent is working — y/n");
        assert_eq!(
            app.on_key(KeyCode::Char('y')),
            vec![Effect::Complete { task_id: "t1".into(), session: Some("sid".into()) }]
        );
    }

    #[test]
    fn complete_waiting_agent_no_confirm() {
        let mut app = app_with(AgentState::Waiting);
        assert_eq!(app.on_key(KeyCode::Char('x')).len(), 1);
        assert!(app.confirm.is_none());
    }

    #[test]
    fn enter_on_ended_asks_to_resume() {
        let mut app = app_with(AgentState::Ended);
        assert!(app.on_key(KeyCode::Enter).is_empty());
        assert_eq!(app.confirm.as_ref().unwrap().prompt, "Resume 't1'? y/n");
        assert_eq!(app.on_key(KeyCode::Char('y')), vec![Effect::Resume { session: "sid".into(), cwd: "/w".into() }]);
    }

    #[test]
    fn enter_elsewhere_sets_message() {
        let mut app = app_with(AgentState::Working);
        app.current_session = Some("other".into());
        assert!(app.on_key(KeyCode::Enter).is_empty());
        assert_eq!(app.message.as_deref(), Some("running elsewhere"));
    }

    #[test]
    fn empty_section_actions_do_nothing() {
        let mut app = App::new(ids(), None);
        for k in ['x', 'o'] {
            assert!(app.on_key(KeyCode::Char(k)).is_empty());
        }
        assert!(app.on_key(KeyCode::Enter).is_empty());
        assert!(app.message.is_none() && app.confirm.is_none());
    }

    #[test]
    fn fetch_error_keeps_last_tasks() {
        let mut app = App::new(ids(), None);
        let mut t = task("t1");
        t.section_id = Some("u".into());
        app.on_tasks(Ok(vec![t]));
        app.on_tasks(Err(anyhow::anyhow!("boom")));
        assert_eq!(app.board.up_next.len(), 1);
        assert_eq!(app.fetch_error.as_deref(), Some("boom"));
    }

    #[test]
    fn selection_clamps_when_rows_shrink() {
        let mut app = App::new(ids(), None);
        let tasks: Vec<Task> = (0..3)
            .map(|i| Task { section_id: Some("u".into()), ..task(&i.to_string()) })
            .collect();
        app.on_tasks(Ok(tasks));
        app.selected = (Section::UpNext, 2);
        app.on_tasks(Ok(vec![Task { section_id: Some("u".into()), ..task("0") }]));
        assert_eq!(app.selected, (Section::UpNext, 0));
    }

    fn up(ids: &[&str]) -> Vec<Task> {
        ids.iter().enumerate().map(|(i, id)| Task { section_id: Some("u".into()), child_order: i as i64, ..task(id) }).collect()
    }

    #[test]
    fn selection_follows_task_across_reorder() {
        let mut app = App::new(ids(), None);
        app.on_tasks(Ok(up(&["a", "b", "c"])));
        app.selected = (Section::UpNext, 1);
        app.on_tasks(Ok(up(&["c", "a", "b"])));
        assert_eq!(app.selected, (Section::UpNext, 2));
        assert_eq!(app.on_key(KeyCode::Char('o')), vec![Effect::OpenInTodoist("b".into())]);
    }

    #[test]
    fn double_next_spawns_different_tasks() {
        let mut app = App::new(ids(), None);
        app.on_tasks(Ok(up(&["a", "b"])));
        assert_eq!(app.on_key(KeyCode::Char('n')), vec![Effect::SpawnNext("a".into())]);
        assert_eq!(app.on_key(KeyCode::Char('n')), vec![Effect::SpawnNext("b".into())]);
        assert!(app.on_key(KeyCode::Char('n')).is_empty());
        assert_eq!(app.message.as_deref(), Some("Up next is empty"));
        // A refresh before "a" is claimed must not hand it out again.
        app.on_tasks(Ok(up(&["a", "b"])));
        assert!(app.on_key(KeyCode::Char('n')).is_empty());
        // Once "a" leaves Up next it stops being tracked.
        app.on_tasks(Ok(up(&["b", "c"])));
        assert_eq!(app.on_key(KeyCode::Char('n')), vec![Effect::SpawnNext("c".into())]);
    }

    #[test]
    fn ctrl_c_quits_even_while_confirming() {
        let mut app = app_with(AgentState::Working);
        app.on_key(KeyCode::Char('x'));
        let k = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(app.on_key_event(k), vec![Effect::Quit]);
        assert!(app.on_key_event(KeyCode::Char('c').into()).is_empty());
    }

    #[test]
    fn move_card_across_columns() {
        let mut app = App::new(ids(), None);
        app.on_tasks(Ok(up(&["a", "b"])));
        app.selected = (Section::UpNext, 1);
        assert!(app.on_key(KeyCode::Char('H')).is_empty(), "nothing left of Up next");
        assert_eq!(app.on_key(KeyCode::Char('L')), vec![Effect::Move { task_id: "b".into(), section_id: "p".into() }]);
        assert_eq!(app.selected, (Section::InProgress, 0));
        assert_eq!(app.board.up_next.len(), 1);
        let shift_right = KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT);
        assert_eq!(app.on_key_event(shift_right), vec![Effect::Move { task_id: "b".into(), section_id: "r".into() }]);
        // Past Needs review completes (no agent, so no confirm).
        assert_eq!(app.on_key(KeyCode::Char('L')), vec![Effect::Complete { task_id: "b".into(), session: None }]);
    }

    #[test]
    fn mouse_drag_moves_card() {
        use ratatui::crossterm::event::KeyModifiers as M;
        let mut app = App::new(ids(), None);
        app.on_tasks(Ok(up(&["a"])));
        app.hits = vec![
            Hit { rect: Rect::new(0, 0, 10, 3), section: Section::UpNext, index: Some(0) },
            Hit { rect: Rect::new(0, 0, 10, 20), section: Section::UpNext, index: None },
            Hit { rect: Rect::new(10, 0, 10, 20), section: Section::InProgress, index: None },
        ];
        let ev = |kind, column| MouseEvent { kind, column, row: 1, modifiers: M::NONE };
        assert!(app.on_mouse(ev(MouseEventKind::Down(MouseButton::Left), 2)).is_empty());
        app.on_mouse(ev(MouseEventKind::Drag(MouseButton::Left), 12));
        assert_eq!(app.hover, Some(Section::InProgress));
        let e = app.on_mouse(ev(MouseEventKind::Up(MouseButton::Left), 12));
        assert_eq!(e, vec![Effect::Move { task_id: "a".into(), section_id: "p".into() }]);
        assert_eq!((app.drag, app.hover), (None, None));
        // A press on a column's empty space never starts a drag.
        app.on_mouse(MouseEvent { row: 10, ..ev(MouseEventKind::Down(MouseButton::Left), 2) });
        assert_eq!(app.drag, None);
    }

    #[test]
    fn expand_loads_comment_and_esc_closes() {
        let mut app = app_with(AgentState::Waiting);
        assert_eq!(app.on_key(KeyCode::Char(' ')), vec![Effect::LoadComment("t1".into())]);
        assert!(app.expanded);
        app.on_key(KeyCode::Char('j'));
        assert_eq!((app.scroll, app.selected.1), (1, 0));
        app.on_key(KeyCode::Esc);
        assert!(!app.expanded);
    }
}
