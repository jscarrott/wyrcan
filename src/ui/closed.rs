//! The Closed view: one list of finished streams, newest first, in place of the board's groups.
use super::card::wrap;
use super::theme;
use crate::app::ClosedView;
use crate::closed::date;
use crate::model::Stage;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::Frame;

pub fn draw(f: &mut Frame, area: Rect, v: &ClosedView) {
    let [head, _, list] = Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Min(0)]).areas(area);
    let Some(rows) = &v.rows else {
        f.render_widget(Line::styled(" Loading…", theme::dim()), head);
        return;
    };
    let done = rows.iter().filter(|c| c.task.stage == Stage::Done).count();
    f.render_widget(Line::styled(format!(" Closed  {done} done · {} dropped", rows.len() - done), theme::bold()), head);
    if rows.is_empty() {
        f.render_widget(Line::styled(" Nothing closed yet.", theme::dim()), list);
        return;
    }
    let w = (list.width as usize).saturating_sub(3);
    let (mut lines, mut span) = (vec![], (0, 0));
    for (i, c) in rows.iter().enumerate() {
        let start = lines.len();
        let (mark, color) = if c.task.stage == Stage::Done { ("✓", ratatui::style::Color::Green) } else { ("✗", theme::DIM) };
        let title = if i == v.index() { Style::new().fg(theme::ACCENT).add_modifier(ratatui::style::Modifier::BOLD) } else { Style::new() };
        for (n, l) in wrap(&c.task.title, w).into_iter().enumerate() {
            let lead = if n == 0 { format!(" {mark} ") } else { "   ".into() };
            lines.push(Line::from(vec![ratatui::text::Span::styled(lead, Style::new().fg(color)), ratatui::text::Span::styled(l, title)]));
        }
        let pr = c.pr_summary();
        let tail = if pr.is_empty() { String::new() } else { format!(" · {pr}") };
        for l in wrap(&format!("closed {} · took {}{tail}", date(c.closed_at), c.took()), w) {
            lines.push(Line::styled(format!("   {l}"), theme::dim()));
        }
        if i == v.index() {
            span = (start, lines.len());
        }
    }
    // Scroll just enough to keep the selected row on screen, its top first if it's taller than the screen.
    let off = span.1.saturating_sub(list.height as usize).min(span.0);
    f.render_widget(Paragraph::new(lines).scroll((off as u16, 0)), list);
}

#[cfg(test)]
mod tests {
    use crate::app::tests::{add, app, cfg, open_closed};
    use crate::closed::tests::close;
    use crate::model::Stage;
    use crate::store::Store;
    use ratatui::{backend::TestBackend, Terminal};

    fn text(ap: &mut crate::app::App, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| crate::ui::draw(f, ap)).unwrap();
        term.backend().buffer().content().iter().map(|c| c.symbol()).collect()
    }

    fn store() -> Store {
        let s = Store::open_in_memory();
        let long = "Investigate why the nightly ingest job drops events after a schema change";
        let a = close(&s, long, Stage::Done, Some(1_000_000), 1_000_000 + 5 * 3600 + 12 * 60);
        s.set_pr(a, "/r/app", Some("https://github.com/acme/app/pull/12"), None).unwrap();
        let b = close(&s, "abandoned idea", Stage::Dropped, None, 900_000);
        s.set_pr(b, "/r/app", None, Some("never needed")).unwrap();
        add(&s, "still queued", Stage::Queue);
        s
    }

    #[test]
    fn closed_view_shows_rows_and_full_titles() {
        let s = store();
        let mut ap = app(&s, &cfg(3));
        open_closed(&mut ap, &s);
        for (w, h) in [(120, 30), (40, 40)] {
            let t = text(&mut ap, w, h);
            for want in ["Claude Board", "Closed  1 done · 1 dropped", "✓", "✗", "took 5h 12m", "acme/app#12", "no PR:", "never", "needed", "took —", "Pen: none"] {
                assert!(t.contains(want), "{w}x{h}: missing {want:?}");
            }
            for word in "Investigate why the nightly ingest job drops events after a schema change".split(' ') {
                assert!(t.contains(word), "{w}x{h}: missing {word:?}");
            }
            assert!(!t.contains("still queued") && ap.hits.is_empty(), "no board cards or hits under the view");
        }
        let empty = Store::open_in_memory();
        let mut ap = app(&empty, &cfg(3));
        open_closed(&mut ap, &empty);
        assert!(text(&mut ap, 80, 20).contains("Nothing closed yet."));
    }

    #[test]
    fn closed_view_draws_at_tiny_sizes() {
        let s = store();
        let mut ap = app(&s, &cfg(3));
        open_closed(&mut ap, &s);
        for (w, h) in [(1, 1), (10, 5), (20, 10)] {
            text(&mut ap, w, h);
        }
    }
}
