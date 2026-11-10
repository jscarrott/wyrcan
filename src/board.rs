use crate::state::{AgentRecord, AgentState, Kind};
use crate::todoist::Task;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    UpNext,
    InProgress,
    NeedsReview,
}

#[derive(Debug, Clone)]
pub struct Row {
    pub task: Task,
    pub agent: Option<AgentRecord>,
}

#[derive(Debug, Default)]
pub struct Board {
    pub up_next: Vec<Row>,
    pub in_progress: Vec<Row>,
    pub needs_review: Vec<Row>,
    pub writers: Vec<AgentRecord>,
    pub blocked: usize,
}

pub struct SectionIds {
    pub up_next: String,
    pub in_progress: String,
    pub needs_review: String,
}

pub fn build(tasks: &[Task], ids: &SectionIds, agents: &[AgentRecord]) -> Board {
    // Newest live agent per task wins.
    let mut by_task: HashMap<&str, &AgentRecord> = HashMap::new();
    for a in agents.iter().filter(|a| a.agent != AgentState::Closed) {
        if let Some(t) = a.task.as_deref() {
            let slot = by_task.entry(t).or_insert(a);
            if a.state_at > slot.state_at {
                *slot = a;
            }
        }
    }

    let mut b = Board::default();
    for t in tasks {
        let Some(sid) = t.section_id.as_deref() else { continue };
        let dest = if sid == ids.up_next {
            &mut b.up_next
        } else if sid == ids.in_progress {
            &mut b.in_progress
        } else if sid == ids.needs_review {
            &mut b.needs_review
        } else {
            continue;
        };
        dest.push(Row { task: t.clone(), agent: by_task.get(t.id.as_str()).map(|a| (*a).clone()) });
    }

    b.up_next.sort_by_key(|r| r.task.child_order);
    for rows in [&mut b.in_progress, &mut b.needs_review] {
        // Agentless rows last (by child_order); then Blocked first, newest first.
        rows.sort_by_key(|r| match &r.agent {
            Some(a) => (0, a.agent != AgentState::Blocked, -a.state_at, r.task.child_order),
            None => (1, false, 0, r.task.child_order),
        });
    }

    b.writers = agents
        .iter()
        .filter(|a| a.kind == Kind::Pen && !matches!(a.agent, AgentState::Ended | AgentState::Closed))
        .cloned()
        .collect();
    let blocked = |a: &AgentRecord| a.agent == AgentState::Blocked;
    b.blocked = [&b.up_next, &b.in_progress, &b.needs_review]
        .iter()
        .flat_map(|rows| rows.iter().filter_map(|r| r.agent.as_ref()))
        .filter(|a| blocked(a))
        .count()
        + b.writers.iter().filter(|a| blocked(a)).count();
    b
}

pub fn age(now: i64, since: i64) -> String {
    let s = now - since;
    match s {
        ..60 => "now".into(),
        60..3600 => format!("{}m", s / 60),
        3600..86400 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids() -> SectionIds {
        SectionIds { up_next: "u".into(), in_progress: "p".into(), needs_review: "r".into() }
    }

    fn task(id: &str, sec: Option<&str>, order: i64) -> Task {
        Task {
            id: id.into(),
            content: id.into(),
            description: String::new(),
            labels: vec![],
            section_id: sec.map(Into::into),
            child_order: order,
        }
    }

    fn agent(session: &str, t: Option<&str>, st: AgentState, at: i64) -> AgentRecord {
        AgentRecord {
            session: session.into(),
            cwd: "/x".into(),
            task: t.map(Into::into),
            kind: Kind::Task,
            agent: st,
            state_at: at,
            zellij_session: None,
            zellij_pane: None,
            repos: vec![],
        }
    }

    fn order(rows: &[Row]) -> Vec<&str> {
        rows.iter().map(|r| r.task.id.as_str()).collect()
    }

    #[test]
    fn blocked_sorts_first_and_is_counted() {
        let tasks = [task("a", Some("p"), 1), task("b", Some("p"), 2), task("c", Some("p"), 3), task("d", Some("p"), 0)];
        let agents = [
            agent("1", Some("a"), AgentState::Working, 500),
            agent("2", Some("b"), AgentState::Blocked, 100),
            agent("3", Some("c"), AgentState::Waiting, 900),
        ];
        let b = build(&tasks, &ids(), &agents);
        assert_eq!(order(&b.in_progress), ["b", "c", "a", "d"]);
        assert_eq!(b.blocked, 1);
    }

    #[test]
    fn orphan_state_is_ignored() {
        let b = build(&[task("a", Some("p"), 0)], &ids(), &[agent("1", Some("zzz"), AgentState::Blocked, 1)]);
        assert_eq!(b.in_progress.len(), 1);
        assert!(b.in_progress[0].agent.is_none());
        assert_eq!(b.blocked, 0);
    }

    #[test]
    fn closed_agent_not_joined() {
        let b = build(&[task("a", Some("p"), 0)], &ids(), &[agent("1", Some("a"), AgentState::Closed, 1)]);
        assert!(b.in_progress[0].agent.is_none());
    }

    #[test]
    fn newest_agent_wins_and_sections_sort() {
        let tasks = [task("a", Some("u"), 2), task("b", Some("u"), 1), task("x", Some("other"), 0), task("n", None, 0)];
        let agents = [agent("old", Some("a"), AgentState::Working, 1), agent("new", Some("a"), AgentState::Waiting, 9)];
        let b = build(&tasks, &ids(), &agents);
        assert_eq!(order(&b.up_next), ["b", "a"]);
        assert_eq!(b.up_next[1].agent.as_ref().unwrap().session, "new");
        assert!(b.in_progress.is_empty() && b.needs_review.is_empty());
    }

    #[test]
    fn pen_writers_listed_until_ended() {
        let mut live = agent("w1", None, AgentState::Blocked, 1);
        live.kind = Kind::Pen;
        let mut ended = agent("w2", None, AgentState::Ended, 1);
        ended.kind = Kind::Pen;
        let mut closed = agent("w3", None, AgentState::Closed, 1);
        closed.kind = Kind::Pen;
        let b = build(&[], &ids(), &[live, ended, closed]);
        assert_eq!(b.writers.len(), 1);
        assert_eq!(b.writers[0].session, "w1");
        assert_eq!(b.blocked, 1);
    }

    #[test]
    fn age_formats() {
        assert_eq!(age(1000, 1000), "now");
        assert_eq!(age(1000, 940), "1m");
        assert_eq!(age(10000, 2800), "2h");
        assert_eq!(age(200000, 0), "2d");
    }
}
