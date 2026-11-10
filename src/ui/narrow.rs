//! The narrow form: one scrolling column of groups, for beside a Claude session.
use super::card::card_lines;
use super::theme;
use crate::app::{App, Group, Hit};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

pub fn draw(f: &mut Frame, area: Rect, app: &App, now: i64) -> Vec<Hit> {
    let width = area.width as usize;
    let mut lines: Vec<Line> = vec![];
    // (first line, end line, card) for each card.
    let mut spans = vec![];
    for (g, cards) in &app.snap.groups {
        if !lines.is_empty() {
            lines.push(Line::raw(""));
        }
        let count = match g {
            Group::Active => format!(" {}/{}", cards.len(), app.snap.max_active),
            _ => format!(" {}", cards.len()),
        };
        lines.push(Line::from(vec![Span::styled(g.name(), theme::bold()), Span::styled(count, theme::dim())]));
        for c in cards {
            let key = c.key();
            let start = lines.len();
            lines.extend(card_lines(c, width, app.selected.as_ref() == Some(&key), now));
            spans.push((start, lines.len(), key));
        }
    }
    // Scroll just enough to keep the selected card on screen, its top first if it's taller than the screen.
    // shortcut: stateless, so the view snaps when scrolling back up; keep an offset in App if that grates.
    let h = area.height as usize;
    let off = spans
        .iter()
        .find(|s| app.selected.as_ref() == Some(&s.2))
        .map_or(0, |&(start, end, _)| end.saturating_sub(h).min(start));
    f.render_widget(Paragraph::new(lines).scroll((off as u16, 0)), area);
    spans
        .into_iter()
        .filter_map(|(start, end, key)| {
            let (top, bot) = (start.max(off), end.min(off + h));
            (top < bot).then(|| Hit { rect: Rect::new(area.x, area.y + (top - off) as u16, area.width, (bot - top) as u16), key })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::app::tests::{add, app, cfg, session};
    use crate::app::CardKey;
    use crate::model::{AgentState, Stage};
    use crate::store::Store;
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn narrow_shows_groups_counts_and_full_titles() {
        let s = Store::open_in_memory();
        let long = "Investigate why the nightly ingest job drops events after a schema change";
        let a = add(&s, long, Stage::Active);
        session(&s, "s1", Some(a), AgentState::Blocked);
        let r = add(&s, "pcap parser", Stage::Review);
        let q = add(&s, "rate-limit export", Stage::Queue);
        let ap = app(&s, &cfg(3));
        let mut term = Terminal::new(TestBackend::new(40, 40)).unwrap();
        let mut hits = vec![];
        term.draw(|f| hits = super::super::draw(f, &ap)).unwrap();
        let buf = term.backend().buffer();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        for want in ["ACTIVE 1/3", "REVIEW 1", "PARKED 0", "QUEUE 1", "UNTRACKED 0", "1/3 active · 1 blocked"] {
            assert!(text.contains(want), "missing {want:?}");
        }
        for word in long.split(' ') {
            assert!(text.contains(word), "missing {word:?}");
        }
        let keys: Vec<_> = hits.iter().map(|h| h.key.clone()).collect();
        assert_eq!(keys, [CardKey::Task(a), CardKey::Task(r), CardKey::Task(q)]);
        // Each hit covers its card, top border to bottom border.
        for h in &hits {
            assert_eq!(buf[(h.rect.x, h.rect.y)].symbol(), "╭");
            assert_eq!(buf[(h.rect.x, h.rect.bottom() - 1)].symbol(), "╰");
        }
        assert!(hits[0].rect.height > 4, "long title wraps: {:?}", hits[0].rect);
    }
}
