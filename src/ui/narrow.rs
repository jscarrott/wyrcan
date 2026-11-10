//! The narrow form: one scrolling column of groups, for beside a Claude session.
use super::card::card_lines;
use super::{clip, theme};
use crate::app::{App, Group, Hit, Target};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

pub fn draw(f: &mut Frame, area: Rect, app: &App, now: i64) -> Vec<Hit> {
    let width = area.width as usize;
    let mut lines: Vec<Line> = vec![];
    // (first line, end line, target): cards, then each group from its header to its last line.
    let mut spans = vec![];
    let mut groups = vec![];
    for (g, cards) in &app.snap.groups {
        if !lines.is_empty() {
            lines.push(Line::raw(""));
        }
        let head = lines.len();
        let count = match g {
            Group::Active => format!(" {}/{}", cards.len(), app.snap.max_active),
            _ => format!(" {}", cards.len()),
        };
        let mut l = Line::from(vec![Span::styled(g.name(), theme::bold()), Span::styled(count, theme::dim())]);
        if app.drag.as_ref().is_some_and(|d| d.over == Some(*g) && app.group_of(&d.key) != Some(*g)) {
            l = Line::styled(format!("{:width$}", l.to_string()), theme::target());
        }
        lines.push(l);
        for c in cards {
            let key = c.key();
            let start = lines.len();
            lines.extend(card_lines(c, width, app.selected.as_ref() == Some(&key), now));
            spans.push((start, lines.len(), Target::Card(key)));
        }
        if *g == Group::Queue {
            spans.push((lines.len(), lines.len() + 1, Target::AddTask));
            lines.push(Line::styled(" + Add task", theme::dim()));
        }
        groups.push((head, lines.len(), Target::Group(*g)));
    }
    // Scroll just enough to keep the selected card on screen, its top first if it's taller than the screen.
    // shortcut: stateless, so the view snaps when scrolling back up; keep an offset in App if that grates.
    let h = area.height as usize;
    let off = spans
        .iter()
        .find(|s| app.selected.as_ref().is_some_and(|k| s.2 == Target::Card(k.clone())))
        .map_or(0, |&(start, end, _)| end.saturating_sub(h).min(start));
    f.render_widget(Paragraph::new(lines).scroll((off as u16, 0)), area);
    spans.into_iter().chain(groups).filter_map(|(start, end, target)| Some(Hit { rect: clip(area, off, start, end)?, target })).collect()
}

#[cfg(test)]
mod tests {
    use crate::app::tests::{add, app, cfg, session};
    use crate::app::{CardKey, Group, Target};
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
        let mut ap = app(&s, &cfg(3));
        let mut term = Terminal::new(TestBackend::new(40, 40)).unwrap();
        term.draw(|f| super::super::draw(f, &mut ap)).unwrap();
        let buf = term.backend().buffer();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        for want in ["ACTIVE 1/3", "REVIEW 1", "PARKED 0", "QUEUE 1", "UNTRACKED 0", "1/3 active · 1 blocked", "+ Add task"] {
            assert!(text.contains(want), "missing {want:?}");
        }
        for word in long.split(' ') {
            assert!(text.contains(word), "missing {word:?}");
        }
        let cards: Vec<_> = ap.hits.iter().filter_map(|h| if let Target::Card(k) = &h.target { Some((k.clone(), h.rect)) } else { None }).collect();
        assert_eq!(cards.iter().map(|c| c.0.clone()).collect::<Vec<_>>(), [CardKey::Task(a), CardKey::Task(r), CardKey::Task(q)]);
        // Each hit covers its card, top border to bottom border.
        for (_, rect) in &cards {
            assert_eq!(buf[(rect.x, rect.y)].symbol(), "╭");
            assert_eq!(buf[(rect.x, rect.bottom() - 1)].symbol(), "╰");
        }
        assert!(cards[0].1.height > 4, "long title wraps: {:?}", cards[0].1);
        // Each group is a drop target from its header down.
        let review = ap.hits.iter().find(|h| h.target == Target::Group(Group::Review)).unwrap().rect;
        assert!(review.contains(cards[1].1.as_position()) && !review.contains(cards[0].1.as_position()));
    }
}
