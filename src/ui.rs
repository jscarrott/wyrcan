use crate::app::{App, Hit};
use crate::board::{age, Row, Section};
use crate::state::{AgentRecord, AgentState};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, HighlightSpacing, List, ListItem, ListState, Paragraph, Wrap};
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

/// Word-wraps to `width` columns, hard-splitting words that don't fit.
// shortcut: counts chars, not display width; wide glyphs (CJK, emoji) can overrun by a column.
fn wrap(s: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let (mut out, mut cur) = (vec![], String::new());
    for mut w in s.split_whitespace() {
        loop {
            let (cl, wl) = (cur.chars().count(), w.chars().count());
            if cl == 0 && wl <= width || cl > 0 && cl + 1 + wl <= width {
                if cl > 0 {
                    cur.push(' ');
                }
                cur.push_str(w);
                break;
            }
            if cl > 0 {
                out.push(std::mem::take(&mut cur));
                continue;
            }
            let cut = w.char_indices().nth(width).map_or(w.len(), |(i, _)| i);
            out.push(w[..cut].to_string());
            w = &w[cut..];
        }
    }
    if !cur.is_empty() || out.is_empty() {
        out.push(cur);
    }
    out
}

/// Full title, then agent state and labels, then a blank gap line.
fn card(r: &Row, width: usize) -> ListItem<'static> {
    let mut lines: Vec<Line> = wrap(&r.task.content, width)
        .into_iter()
        .map(|l| Line::styled(l, Style::new().add_modifier(Modifier::BOLD)))
        .collect();
    let mut meta = vec![];
    if let Some(a) = &r.agent {
        let (name, color) = state_style(a.agent);
        meta.push(Span::styled(format!("{name} {}", age_str(a.state_at)), Style::new().fg(color)));
    }
    if !r.task.description.is_empty() {
        meta.push(Span::styled("  ≡", Style::new().fg(Color::DarkGray)));
    }
    if !meta.is_empty() {
        lines.push(Line::from(meta));
    }
    if !r.task.labels.is_empty() {
        let labels = format!("@{}", r.task.labels.join(" @"));
        lines.extend(wrap(&labels, width).into_iter().map(|l| Line::styled(l, Style::new().fg(Color::Cyan))));
    }
    lines.push(Line::raw(""));
    ListItem::new(lines)
}

fn centered(area: Rect, pct: u16) -> Rect {
    let [v] = Layout::vertical([Constraint::Percentage(pct)]).flex(ratatui::layout::Flex::Center).areas(area);
    let [h] = Layout::horizontal([Constraint::Percentage(pct)]).flex(ratatui::layout::Flex::Center).areas(v);
    h
}

fn detail(f: &mut Frame, app: &App) {
    let Some(r) = app.selected_row() else { return };
    let dim = Style::new().fg(Color::DarkGray);
    let head = |t: &'static str| Line::styled(t, Style::new().add_modifier(Modifier::BOLD | Modifier::UNDERLINED));
    let mut lines = vec![Line::styled(r.task.content.clone(), Style::new().add_modifier(Modifier::BOLD)), Line::raw("")];
    if !r.task.labels.is_empty() {
        lines.push(Line::styled(format!("@{}", r.task.labels.join(" @")), Style::new().fg(Color::Cyan)));
    }
    if let Some(a) = &r.agent {
        let (name, color) = state_style(a.agent);
        lines.push(Line::styled(format!("{name} {}", age_str(a.state_at)), Style::new().fg(color)));
        lines.push(Line::styled(format!("session {}", a.session), dim));
        lines.push(Line::styled(format!("cwd     {}", a.cwd), dim));
        for repo in &a.repos {
            lines.push(Line::styled(format!("repo    {} @ {}", repo.root, repo.ref_), dim));
        }
    }
    lines.push(Line::raw(""));
    lines.push(head("Description"));
    if r.task.description.is_empty() {
        lines.push(Line::styled("none", dim));
    } else {
        lines.extend(r.task.description.lines().map(|l| Line::raw(l.to_string())));
    }
    lines.push(Line::raw(""));
    lines.push(head("Latest comment"));
    match app.comments.get(&r.task.id) {
        None => lines.push(Line::styled("loading…", dim)),
        Some(None) => lines.push(Line::styled("none", dim)),
        Some(Some(c)) => lines.extend(c.lines().map(|l| Line::raw(l.to_string()))),
    }
    let area = centered(f.area(), 80);
    let block = Block::bordered()
        .border_style(Style::new().fg(Color::Cyan))
        .title_bottom(Line::styled(" ↑↓ scroll  H/L move  x complete  o open  esc close ", dim));
    f.render_widget(Clear, area);
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((app.scroll, 0)).block(block), area);
}

fn writer_span(a: &AgentRecord) -> Span<'static> {
    let (name, color) = state_style(a.agent);
    Span::styled(format!("pen {name} {}  ", age_str(a.state_at)), Style::new().fg(color))
}

/// Draws the board and returns where each card and column landed, for mouse hits.
pub fn draw(f: &mut Frame, app: &App) -> Vec<Hit> {
    let mut hits = vec![];
    let mut columns = vec![];
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
        let title = format!(" {title} ({}) ", rows.len());
        let border = if app.hover == Some(s) && app.drag != Some(s) {
            Style::new().fg(Color::Yellow)
        } else if active {
            Style::new().fg(Color::Cyan)
        } else {
            Style::new()
        };
        let block = Block::bordered().title(title).border_style(border);
        // Borders take 2 columns and the highlight bar 2 more.
        let width = area.width.saturating_sub(4) as usize;
        let items: Vec<ListItem> = rows.iter().map(|r| card(r, width)).collect();
        let heights: Vec<u16> = items.iter().map(|i| i.height() as u16).collect();
        let list = List::new(items)
            .block(block)
            .highlight_symbol(Line::styled("▌ ", Style::new().fg(Color::Cyan)))
            .repeat_highlight_symbol(true)
            .highlight_spacing(HighlightSpacing::Always)
            .highlight_style(Style::new().fg(Color::Cyan));
        let mut st = ListState::default();
        if active && !rows.is_empty() {
            st.select(Some(app.selected.1));
        }
        f.render_stateful_widget(list, *area, &mut st);

        let inner = area.inner(ratatui::layout::Margin::new(1, 1));
        let mut y = inner.y;
        for (i, h) in heights.iter().enumerate().skip(st.offset()) {
            if y >= inner.bottom() {
                break;
            }
            let rect = Rect::new(inner.x, y, inner.width, (*h).min(inner.bottom() - y));
            hits.push(Hit { rect, section: s, index: Some(i) });
            y += h;
        }
        columns.push(Hit { rect: *area, section: s, index: None });
    }
    hits.extend(columns);

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
            "←→ column  ↑↓ card  space expand  H/L or drag: move  n next  p pen  ⏎ jump  x complete  o open  r refresh  q quit",
            Style::new().fg(Color::DarkGray),
        )
    };
    f.render_widget(Paragraph::new(foot), footer);
    if app.expanded {
        detail(f, app);
    }
    hits
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
        term.draw(|f| { draw(f, &app); }).unwrap();
        let text: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("2 blocked"), "{text}");
    }

    #[test]
    fn wrap_breaks_words_and_long_tokens() {
        assert_eq!(wrap("fix the flaky login test", 10), ["fix the", "flaky", "login test"]);
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap("", 4), [""]);
    }

    #[test]
    fn long_title_is_shown_in_full_and_hits_cover_cards() {
        use crate::todoist::Task;
        let ids = SectionIds { up_next: "u".into(), in_progress: "p".into(), needs_review: "r".into() };
        let mut app = App::new(ids, None);
        let t = |id: &str, content: &str| Task {
            id: id.into(),
            content: content.into(),
            description: String::new(),
            labels: vec![],
            section_id: Some("u".into()),
            child_order: 0,
        };
        let long = "Investigate why the nightly ingest job drops events after a schema change";
        app.on_tasks(Ok(vec![t("a", long), t("b", "short")]));
        let mut term = Terminal::new(TestBackend::new(90, 30)).unwrap();
        let mut hits = vec![];
        term.draw(|f| hits = draw(f, &app)).unwrap();
        let text: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        for word in long.split(' ') {
            assert!(text.contains(word), "missing {word:?}");
        }
        let cards: Vec<_> = hits.iter().filter(|h| h.index.is_some()).collect();
        assert_eq!(cards.len(), 2);
        assert!(cards[0].rect.height > 2, "long title wraps: {:?}", cards[0].rect);
        assert_eq!(cards[1].rect.y, cards[0].rect.bottom());
    }
}
