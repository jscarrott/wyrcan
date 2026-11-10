use crate::board::{self, Board, Row, Section, SectionIds};
use crate::state::{AgentRecord, AgentState};
use crate::todoist::Task;
use crate::zellij::{self, Jump};
use ratatui::crossterm::event::KeyCode;

#[derive(Debug, PartialEq)]
pub enum Effect {
    SpawnNext,
    SpawnPen,
    Jump(Jump),
    Resume { session: String, cwd: String },
    Complete { task_id: String, session: Option<String> },
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

pub struct App {
    pub board: Board,
    pub selected: (Section, usize),
    pub message: Option<String>,
    pub confirm: Option<Pending>,
    pub fetch_error: Option<String>,
    tasks: Vec<Task>,
    agents: Vec<AgentRecord>,
    ids: SectionIds,
    current_session: Option<String>,
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
            tasks: vec![],
            agents: vec![],
            ids,
            current_session,
        }
    }

    pub fn rows(&self, s: Section) -> &[Row] {
        match s {
            Section::UpNext => &self.board.up_next,
            Section::InProgress => &self.board.in_progress,
            Section::NeedsReview => &self.board.needs_review,
        }
    }

    fn selected_row(&self) -> Option<&Row> {
        self.rows(self.selected.0).get(self.selected.1)
    }

    fn rebuild(&mut self) {
        self.board = board::build(&self.tasks, &self.ids, &self.agents);
        let len = self.rows(self.selected.0).len();
        self.selected.1 = self.selected.1.min(len.saturating_sub(1));
    }

    pub fn on_tasks(&mut self, r: anyhow::Result<Vec<Task>>) {
        match r {
            Ok(t) => {
                self.tasks = t;
                self.fetch_error = None;
                self.rebuild();
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
    }

    fn shift_row(&mut self, d: isize) {
        let max = self.rows(self.selected.0).len().saturating_sub(1) as isize;
        self.selected.1 = (self.selected.1 as isize + d).clamp(0, max) as usize;
    }

    pub fn on_key(&mut self, key: KeyCode) -> Vec<Effect> {
        if let Some(p) = self.confirm.take() {
            return if key == KeyCode::Char('y') { vec![p.effect] } else { vec![] };
        }
        self.message = None;
        match key {
            KeyCode::Left | KeyCode::Char('h') => self.shift_section(-1),
            KeyCode::Right | KeyCode::Char('l') => self.shift_section(1),
            KeyCode::Up | KeyCode::Char('k') => self.shift_row(-1),
            KeyCode::Down | KeyCode::Char('j') => self.shift_row(1),
            KeyCode::Char('n') if self.board.up_next.is_empty() => self.message = Some("Up next is empty".into()),
            KeyCode::Char('n') => return vec![Effect::SpawnNext],
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

    fn enter(&mut self) -> Vec<Effect> {
        let Some(row) = self.selected_row() else { return vec![] };
        match zellij::jump_target(row.agent.as_ref(), self.current_session.as_deref()) {
            Jump::Resume { session, cwd } => self.ask("Resume session? y/n".into(), Effect::Resume { session, cwd }),
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
                self.ask(format!("Agent is {s} — complete anyway? y/n"), effect);
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
        assert!(app.confirm.is_some());
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
        assert_eq!(app.confirm.as_ref().unwrap().prompt, "Resume session? y/n");
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
}
