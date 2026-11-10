use crate::app::App;
use crate::board::{age, Row, Section};
use crate::state::{AgentRecord, AgentState};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Legacy records have state_at == 0, which has no meaningful age.
fn age_str(since: i64) -> String {
    if since == 0 { "—".into() } else { age(now(), since) }
}

fn state_style(s: AgentState) -> (&'static str, Color) {
    match s {
        AgentState::Working => ("working", Color::Green),
        AgentState::Waiting => ("waiting", Color::Yellow),
        AgentState::Blocked => ("blocked", Color::Red),
        AgentState::Ended => ("ended", Color::DarkGray),
        AgentState::Closed => ("closed", Color::DarkGray),
    }
}

fn row_item(r: &Row) -> ListItem<'static> {
    let mut spans = vec![Span::raw(r.task.content.clone())];
    if let Some(a) = &r.agent {
        let (name, color) = state_style(a.agent);
        spans.push(Span::styled(format!("  {name} {}", age_str(a.state_at)), Style::new().fg(color)));
    }
    if !r.task.labels.is_empty() {
        spans.push(Span::styled(format!("  @{}", r.task.labels.join(" @")), Style::new().fg(Color::Cyan)));
    }
    ListItem::new(Line::from(spans))
}

fn writer_span(a: &AgentRecord) -> Span<'static> {
    let (name, color) = state_style(a.agent);
    Span::styled(format!("pen {name} {}  ", age_str(a.state_at)), Style::new().fg(color))
}

pub fn draw(f: &mut Frame, app: &App) {
    let [header, cols, writers, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(f.area());

    let blocked = Style::new().fg(if app.board.blocked > 0 { Color::Red } else { Color::Reset });
    let mut h = vec![
        Span::styled("Claude Board  ", Style::new().add_modifier(Modifier::BOLD)),
        Span::styled(format!("{} blocked", app.board.blocked), blocked),
    ];
    if let Some(e) = &app.fetch_error {
        h.push(Span::styled(format!("  fetch failed: {e}"), Style::new().fg(Color::Red)));
    }
    f.render_widget(Line::from(h), header);

    let areas = Layout::horizontal([Constraint::Ratio(1, 3); 3]).split(cols);
    for ((title, s), area) in [("Up next", Section::UpNext), ("In progress", Section::InProgress), ("Needs review", Section::NeedsReview)]
        .into_iter()
        .zip(areas.iter())
    {
        let rows = app.rows(s);
        let active = app.selected.0 == s;
        let title = format!("{title} ({})", rows.len());
        let block = Block::bordered()
            .title(title)
            .border_style(if active { Style::new().fg(Color::Cyan) } else { Style::new() });
        let list = List::new(rows.iter().map(row_item))
            .block(block)
            .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
        let mut st = ListState::default();
        if active && !rows.is_empty() {
            st.select(Some(app.selected.1));
        }
        f.render_stateful_widget(list, *area, &mut st);
    }

    let mut w = vec![Span::raw("Pen: ")];
    if app.board.writers.is_empty() {
        w.push(Span::styled("none", Style::new().fg(Color::DarkGray)));
    }
    w.extend(app.board.writers.iter().map(writer_span));
    f.render_widget(Line::from(w), writers);

    let foot = if let Some(p) = &app.confirm {
        Line::styled(p.prompt.clone(), Style::new().fg(Color::Yellow))
    } else if let Some(m) = &app.message {
        Line::styled(m.clone(), Style::new().fg(Color::Yellow))
    } else {
        Line::styled(
            "←→ section  ↑↓ move  n next  p pen  ⏎ jump  x complete  o open  r refresh  q quit",
            Style::new().fg(Color::DarkGray),
        )
    };
    f.render_widget(Paragraph::new(foot), footer);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::SectionIds;
    use crate::state::Kind;
    use ratatui::{backend::TestBackend, Terminal};

    fn blocked_agent() -> AgentRecord {
        AgentRecord {
            session: "s".into(),
            cwd: "/".into(),
            task: None,
            kind: Kind::Pen,
            agent: AgentState::Blocked,
            state_at: 0,
            zellij_session: None,
            zellij_pane: None,
            repos: vec![],
        }
    }

    #[test]
    fn header_shows_blocked_count() {
        let ids = SectionIds { up_next: "u".into(), in_progress: "p".into(), needs_review: "r".into() };
        let mut app = App::new(ids, None);
        app.on_agents(vec![blocked_agent(), blocked_agent()]);
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let text: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("2 blocked"), "{text}");
    }
}
