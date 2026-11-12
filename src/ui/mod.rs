//! Drawing. `draw` picks the form by width; both share the header, writers line and footer.
mod card;
mod closed;
mod modal;
mod narrow;
mod wide;
pub mod theme;

use crate::app::{App, View};
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

const KEYS: &[&str] = &[
    "⇥ view", "↑↓ card", "←→ group", "space view", "+ add", "n next", "s start", "P park", "w wrap", "H/L move", "x done", "D drop/dismiss",
    "a adopt", "⏎ go to", "f full", "v wide", "p pen", "e explainer", "o PR", "q quit",
];
const VIEW_KEYS: &[&str] = &["↑↓ scroll", "e explainer", "o PR", "w wrap", "P park", "x done", "D drop", "esc close"];
const CLOSED_KEYS: &[&str] = &["↑↓ move", "space view", "e explainer", "o PR", "c/esc board", "q quit"];
const CLOSED_VIEW_KEYS: &[&str] = &["↑↓ scroll", "e explainer", "o PR", "esc close"];
const ADD_KEYS: &[&str] = &["⏎ add to queue", "esc cancel", "@label", "#directory", "!exp experiment"];
const PROMPT_KEYS: &[&str] = &["⏎ ok", "esc cancel"];

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
    let keys = match () {
        _ if app.quick.is_some() => ADD_KEYS,
        _ if app.prompt.is_some() => PROMPT_KEYS,
        _ if app.closed.is_some() && app.modal.is_some() => CLOSED_VIEW_KEYS,
        _ if app.closed.is_some() => CLOSED_KEYS,
        _ if app.modal.is_some() => VIEW_KEYS,
        _ => KEYS,
    };
    if let Some(c) = &app.confirm {
        text(c.prompt.clone(), warn)
    } else if let Some(m) = &app.message {
        text(m.clone(), warn)
    } else {
        pack(keys, width).into_iter().map(|l| Line::styled(l, theme::dim())).collect()
    }
}

/// The screen rect of lines `start..end` of a paragraph drawn in `area` scrolled down `off` lines, if any shows.
fn clip(area: Rect, off: usize, start: usize, end: usize) -> Option<Rect> {
    let (top, bot) = (start.max(off), end.min(off + area.height as usize));
    (top < bot).then(|| Rect::new(area.x, area.y + (top - off) as u16, area.width, (bot - top) as u16))
}

/// At this many columns or more the board uses the wide form.
pub const WIDE_MIN: u16 = 100;

/// Draws the board and records in `app` the form in use and where things landed, for the mouse.
pub fn draw(f: &mut Frame, app: &mut App) {
    let now = crate::store::now();
    let area = f.area();
    app.wide = area.width >= WIDE_MIN;
    let foot = footer(app, area.width as usize);
    let input_h = if app.quick.is_some() { 4 } else if app.prompt.is_some() { 3 } else { 0 };
    let [header, body, input, writers, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(input_h),
        Constraint::Length(1),
        Constraint::Length(foot.len() as u16),
    ])
    .areas(area);

    let sn = &app.snap;
    let mut left = vec![Span::raw(" "), Span::styled("Claude Board", theme::bold())];
    for v in View::ALL {
        let st = if v == app.view { Style::new().fg(theme::ACCENT) } else { theme::dim() };
        left.extend([Span::styled(if v == View::Work { "   " } else { " · " }, theme::dim()), Span::styled(v.name(), st)]);
    }
    let blocked = Style::new().fg(if sn.blocked > 0 { theme::ACCENT } else { theme::DIM });
    let right = [Span::styled(format!("{}/{} active · ", sn.active, sn.max_active), theme::dim()), Span::styled(format!("{} blocked", sn.blocked), blocked)];
    let width = |s: &[Span]| s.iter().map(|s| s.width()).sum::<usize>();
    // Too narrow for every view: the narrow form's group headers already say which view is on.
    if 3 + width(&left) + width(&right) > area.width as usize {
        left.truncate(2);
    }
    let gap = (area.width as usize).saturating_sub(1 + width(&left) + width(&right)).max(2);
    f.render_widget(Line::from([left, vec![Span::raw(" ".repeat(gap))], right.to_vec()].concat()), header);

    let mut w = vec![Span::raw(" Pen: ")];
    if sn.writers.is_empty() {
        w.push(Span::styled("none", theme::dim()));
    }
    for s in &sn.writers {
        let (_, name, color) = theme::agent(s.state);
        w.push(Span::styled(format!("pen {name} {}  ", card::age(now, s.state_at)), Style::new().fg(color)));
    }
    f.render_widget(Line::from(w), writers);
    f.render_widget(Paragraph::new(foot), footer);

    // The Closed view only reads, so it leaves no hits for the mouse.
    let mut hits = if let Some(c) = &app.closed {
        closed::draw(f, body, c);
        vec![]
    } else if app.wide { wide::draw(f, body, app, now) } else { narrow::draw(f, body, app, now) };
    if let Some(q) = &app.quick {
        modal::draw_quick_add(f, input, q);
    } else if let Some(p) = &app.prompt {
        modal::draw_prompt(f, input, &p.label, &p.text, p.text.chars().count());
    }
    // The gate status the card carries: what an Active stream still lacks, or that a Review one passed.
    let gate = match app.selected_card().filter(|_| app.closed.is_none()) {
        Some(c) if c.task.as_ref().is_some_and(|t| t.stage == crate::model::Stage::Review) => vec!["passed".into()],
        Some(c) if c.task.as_ref().is_some_and(|t| t.stage == crate::model::Stage::Active) && c.missing.is_empty() => vec!["ready for review".into()],
        Some(c) => c.missing.clone(),
        None => vec![],
    };
    if let Some(v) = app.modal.as_mut() {
        let [mid] = Layout::vertical([Constraint::Percentage(80)]).flex(Flex::Center).areas(area);
        let [mid] = Layout::horizontal([Constraint::Percentage(80)]).flex(Flex::Center).areas(mid);
        modal::draw_task(f, mid, v, gate, now);
        // The board under the view takes no clicks.
        hits.clear();
    }
    app.hits = hits;
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
        term.draw(|f| super::draw(f, &mut ap)).unwrap();
        let text: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        for want in ["Claude Board", "1/3 active · 0 blocked", "Active  1/3", "Parked  1", "Queue  1", "Fix auth timeout", "Pen: pen working", "not inside zellij"] {
            assert!(text.contains(want), "missing {want:?}");
        }
    }
}
