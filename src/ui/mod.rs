//! Drawing. `draw` picks the form by width; both share the header, writers line and footer.
mod card;
mod narrow;
pub mod theme;

use crate::app::{App, Hit};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

const KEYS: &[&str] = &[
    "↑↓ card", "←→ group", "n next", "s start", "P park", "w wrap", "H/L move", "x done", "D drop", "a adopt",
    "⏎ go to", "f full", "v wide", "p pen", "e explainer", "o PR", "q quit",
];

/// Packs items into lines of `width` columns, two spaces apart.
fn pack(items: &[&str], width: usize) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for it in items {
        match out.last_mut() {
            Some(l) if Span::raw(l.as_str()).width() + 2 + Span::raw(*it).width() <= width => {
                l.push_str("  ");
                l.push_str(it);
            }
            _ => out.push(it.to_string()),
        }
    }
    out
}

fn footer(app: &App, width: usize) -> Vec<Line<'static>> {
    let warn = Style::new().fg(ratatui::style::Color::Yellow);
    let text = |s: String, st: Style| card::wrap(&s, width).into_iter().map(|l| Line::styled(l, st)).collect();
    if let Some(c) = &app.confirm {
        text(c.prompt.clone(), warn)
    } else if let Some(p) = &app.prompt {
        // Not wrapped: spaces in the typed text must show.
        vec![Line::from(vec![Span::styled(format!("{} ", p.label), warn), Span::raw(format!("{}▏", p.text))])]
    } else if let Some(m) = &app.message {
        text(m.clone(), warn)
    } else {
        pack(KEYS, width).into_iter().map(|l| Line::styled(l, theme::dim())).collect()
    }
}

/// At this many columns or more the board uses the wide form (Task 7).
pub const WIDE_MIN: u16 = 100;

pub fn draw(f: &mut Frame, app: &App) -> Vec<Hit> {
    let now = crate::store::now();
    let area = f.area();
    let foot = footer(app, area.width as usize);
    let [header, body, writers, footer] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0), Constraint::Length(1), Constraint::Length(foot.len() as u16)]).areas(area);

    let sn = &app.snap;
    let left = Span::styled("Claude Board", theme::bold());
    let blocked = Style::new().fg(if sn.blocked > 0 { theme::ACCENT } else { theme::DIM });
    let right = [Span::styled(format!("{}/{} active · ", sn.active, sn.max_active), theme::dim()), Span::styled(format!("{} blocked", sn.blocked), blocked)];
    let gap = (area.width as usize).saturating_sub(left.width() + right.iter().map(|s| s.width()).sum::<usize>()).max(2);
    f.render_widget(Line::from([vec![left, Span::raw(" ".repeat(gap))], right.to_vec()].concat()), header);

    // Task 7 swaps in the wide form at WIDE_MIN columns and up.
    let hits = narrow::draw(f, body, app, now);

    let mut w = vec![Span::raw("Pen: ")];
    if sn.writers.is_empty() {
        w.push(Span::styled("none", theme::dim()));
    }
    for s in &sn.writers {
        let (_, name, color) = theme::agent(s.state);
        w.push(Span::styled(format!("pen {name} {}  ", card::age(now, s.state_at)), Style::new().fg(color)));
    }
    f.render_widget(Line::from(w), writers);
    f.render_widget(Paragraph::new(foot), footer);
    hits
}

#[cfg(test)]
mod tests {
    use crate::app::tests::{add, app, cfg, session};
    use crate::model::{AgentState, Stage};
    use crate::store::Store;
    use ratatui::{backend::TestBackend, Terminal};

    /// Stands in for running the board by hand: draws a store with a few tasks at a wide size.
    #[test]
    fn board_draws_from_store() {
        let s = Store::open_in_memory();
        let a = add(&s, "Fix auth timeout on token refresh", Stage::Active);
        session(&s, "s1", Some(a), AgentState::Working);
        add(&s, "Tidy ingest retry", Stage::Parked);
        add(&s, "Rate-limit the export endpoint", Stage::Queue);
        s.register_pen("pen", "/w").unwrap();
        let mut ap = app(&s, &cfg(3));
        ap.message = Some("not inside zellij".into());
        let mut term = Terminal::new(TestBackend::new(120, 30)).unwrap();
        term.draw(|f| {
            super::draw(f, &ap);
        })
        .unwrap();
        let text: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        for want in ["Claude Board", "1/3 active · 0 blocked", "ACTIVE 1/3", "PARKED 1", "QUEUE 1", "Fix auth timeout", "Pen: pen working", "not inside zellij"] {
            assert!(text.contains(want), "missing {want:?}");
        }
    }
}
