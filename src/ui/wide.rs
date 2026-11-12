//! The wide form: Todoist's board view, one scrolling column per stage.
use super::card::card_lines;
use super::{clip, theme};
use crate::app::{App, Group, Hit, Target};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

pub fn draw(f: &mut Frame, area: Rect, app: &mut App, now: i64) -> Vec<Hit> {
    let [rule, cols] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    f.render_widget(Line::styled(format!(" {}", "─".repeat((rule.width as usize).saturating_sub(2))), theme::dim()), rule);
    // Untracked only shows when there's something in it, and narrower.
    let shown: Vec<Group> = app.visible_groups().iter().copied().filter(|g| *g != Group::Untracked || !app.cards(*g).is_empty()).collect();
    let widths = shown.iter().map(|g| Constraint::Fill(if *g == Group::Untracked { 2 } else { 3 }));
    let rects = Layout::horizontal(widths).spacing(2).horizontal_margin(1).split(cols);
    let mut hits = vec![];
    for (g, r) in shown.into_iter().zip(rects.iter()) {
        hits.extend(column(f, *r, app, g, now));
    }
    app.followed = app.selected.clone();
    hits
}

/// One column: a header with its count, then cards a blank line apart, scrolled on its own.
fn column(f: &mut Frame, area: Rect, app: &mut App, g: Group, now: i64) -> Vec<Hit> {
    let [head, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    let width = area.width as usize;
    let cs = app.cards(g);
    let name = g.name();
    let count = match g {
        Group::Active => format!("{}/{}", cs.len(), app.snap.max_active),
        _ => cs.len().to_string(),
    };
    let title = format!(" {}{}  ", &name[..1], name[1..].to_lowercase());
    let over = app.drag.as_ref().is_some_and(|d| d.over == Some(g) && app.group_of(&d.key) != Some(g));
    let heading = if over {
        Line::styled(format!("{:width$}", format!("{title}{count}")), theme::target())
    } else {
        Line::from(vec![Span::styled(title, theme::bold()), Span::styled(count, theme::dim())])
    };

    // (first line, end line, target) of each card and of "+ Add task".
    let mut lines: Vec<Line> = vec![];
    let mut spans = vec![];
    for c in cs {
        if !lines.is_empty() {
            lines.push(Line::raw(""));
        }
        let key = c.key();
        let start = lines.len();
        lines.extend(card_lines(c, width, app.selected.as_ref() == Some(&key), now));
        spans.push((start, lines.len(), Target::Card(key)));
    }
    if g == Group::Queue {
        if !lines.is_empty() {
            lines.push(Line::raw(""));
        }
        spans.push((lines.len(), lines.len() + 1, Target::AddTask));
        lines.push(Line::styled(" + Add task", theme::dim()));
    }

    // A newly selected card scrolls into view (its top first if it's taller than the column);
    // otherwise the column stays where the wheel left it.
    let h = body.height as usize;
    let mut off = app.scroll[g as usize];
    if app.followed != app.selected {
        let sel = spans.iter().find(|s| app.selected.as_ref().is_some_and(|k| s.2 == Target::Card(k.clone())));
        if let Some(&(start, end, _)) = sel {
            off = off.min(start).max(end.saturating_sub(h).min(start));
        }
    }
    off = off.min(lines.len().saturating_sub(h));
    app.scroll[g as usize] = off;
    let more = |n: bool, s: &'static str| Span::styled(if n { s } else { "" }, theme::dim());
    let arrows = Line::from(vec![more(off > 0, "↑ "), more(off + h < lines.len(), "↓ ")]).right_aligned();

    f.render_widget(heading, head);
    if !over {
        f.render_widget(arrows, head);
    }
    f.render_widget(Paragraph::new(lines).scroll((off as u16, 0)), body);
    let mut hits: Vec<Hit> = spans.into_iter().filter_map(|(start, end, target)| Some(Hit { rect: clip(body, off, start, end)?, target })).collect();
    hits.push(Hit { rect: area, target: Target::Group(g) });
    hits
}

#[cfg(test)]
mod tests {
    use crate::app::tests::{add, app, cfg, session};
    use crate::app::{App, Group, Target};
    use crate::model::{AgentState, Stage};
    use crate::store::Store;
    use ratatui::{backend::TestBackend, Terminal};

    fn draw(ap: &mut App, w: u16, h: u16) -> Vec<String> {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| crate::ui::draw(f, ap)).unwrap();
        let buf = term.backend().buffer();
        (0..h).map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect()).collect()
    }

    #[test]
    fn wide_shows_four_columns_with_counts() {
        let s = Store::open_in_memory();
        let a = add(&s, "Fix auth timeout", Stage::Active);
        session(&s, "s1", Some(a), AgentState::Blocked);
        add(&s, "Pcap parser", Stage::Review);
        for t in ["q1", "q2"] {
            add(&s, t, Stage::Queue);
        }
        let mut ap = app(&s, &cfg(3));
        let rows = draw(&mut ap, 140, 40);
        assert!(ap.wide);
        assert!(rows[1].trim().chars().all(|c| c == '─'), "rule under the title: {:?}", rows[1]);
        let heads = &rows[2];
        let at = |s: &str| heads.find(s).unwrap_or_else(|| panic!("missing {s:?} in {heads:?}"));
        assert!(at("Active  1/3") < at("Review  1") && at("Review  1") < at("Parked  0") && at("Parked  0") < at("Queue  2"));
        assert!(!heads.contains("Untracked"));
        let text = rows.join("\n");
        assert!(text.contains("+ Add task"), "{text}");
        // Cards are spaced: a blank line between the two queue cards.
        // The column starts one left of its title.
        let qcol = heads[..heads.find("Queue").unwrap()].chars().count() - 1;
        let col: Vec<String> = rows.iter().map(|r| r.chars().skip(qcol).take(10).collect()).collect();
        let bottoms: Vec<usize> = col.iter().enumerate().filter(|(_, l)| l.starts_with('╰')).map(|(i, _)| i).collect();
        assert_eq!(bottoms.len(), 2);
        assert!(col[bottoms[0] + 1].trim().is_empty() && col[bottoms[0] + 2].starts_with('╭'), "{col:#?}");
        // Columns are drop targets.
        for g in [Group::Active, Group::Review, Group::Parked, Group::Queue] {
            assert!(ap.hits.iter().any(|h| h.target == Target::Group(g)), "{g:?}");
        }
    }

    #[test]
    fn untracked_column_only_when_present() {
        let s = Store::open_in_memory();
        add(&s, "q", Stage::Queue);
        let mut ap = app(&s, &cfg(3));
        assert!(!draw(&mut ap, 140, 20)[2].contains("Untracked"));
        session(&s, "u1", None, AgentState::Waiting);
        s.mark_edited("u1", "/w/proj", "git main").unwrap();
        ap.set_snapshot(crate::app::snapshot(&s, &cfg(3)).unwrap());
        let rows = draw(&mut ap, 140, 20);
        assert!(rows[2].contains("Untracked  1"), "{:?}", rows[2]);
        assert!(rows.join("\n").contains("/w/proj"));
    }

    #[test]
    fn wide_cards_show_full_titles() {
        let s = Store::open_in_memory();
        let long = "Investigate why the nightly ingest job drops events after a schema change in the upstream topic";
        add(&s, long, Stage::Parked);
        let mut ap = app(&s, &cfg(3));
        let text = draw(&mut ap, 140, 40).join("\n");
        for word in long.split(' ') {
            assert!(text.contains(word), "missing {word:?} in\n{text}");
        }
    }

    #[test]
    fn review_and_shelved_tasks_leave_my_work() {
        let s = Store::open_in_memory();
        add(&s, "queued work", Stage::Queue);
        let r = add(&s, "review their PR", Stage::Queue);
        s.set_kind(r, "review", None).unwrap();
        let x = add(&s, "try sled storage", Stage::Shelved);
        s.add_event(x, "summary", "sled is fast but unmaintained").unwrap();
        s.add_event(x, "bookmark", "/w/proj=exp-sled").unwrap();
        let mut ap = app(&s, &cfg(3));
        let text = draw(&mut ap, 140, 30).join("\n");
        assert!(text.contains("queued work") && text.contains("My work"), "{text}");
        for gone in ["Requested", "review their PR", "try sled storage"] {
            assert!(!text.contains(gone), "{gone:?} in\n{text}");
        }
        ap.on_key_event(ratatui::crossterm::event::KeyCode::Tab.into());
        let rows = draw(&mut ap, 140, 30);
        let heads = &rows[2];
        assert!(heads.find("Requested  1").unwrap() < heads.find("Reviewing  0").unwrap(), "{heads:?}");
        assert!(heads.contains("Waiting on author  0"), "{heads:?}");
        let text = rows.join("\n");
        assert!(text.contains("review their PR") && !text.contains("n next"), "{text}");
        ap.on_key_event(ratatui::crossterm::event::KeyCode::Tab.into());
        let text = draw(&mut ap, 140, 30).join("\n");
        for want in ["Shelved  1", "try sled storage", "sled is fast but unmaintained", "/w/proj → exp-sled", "s/⏎ resume experiment"] {
            assert!(text.contains(want), "missing {want:?} in\n{text}");
        }
        assert!(!text.contains("queued work"), "{text}");
    }

    #[test]
    fn columns_scroll_on_their_own() {
        let s = Store::open_in_memory();
        let qs: Vec<i64> = (0..12).map(|i| add(&s, &format!("queued task {i}"), Stage::Queue)).collect();
        for i in 0..12 {
            add(&s, &format!("parked task {i}"), Stage::Parked);
        }
        let mut ap = app(&s, &cfg(3));
        ap.selected = Some(crate::app::CardKey::Task(*qs.last().unwrap()));
        let text = draw(&mut ap, 140, 24).join("\n");
        assert!(text.contains("queued task 11"), "selected card scrolled into view:\n{text}");
        let first_parked = ap.snap.groups[Group::Parked as usize].1[0].task.as_ref().unwrap().title.clone();
        assert!(text.contains(&first_parked) && !text.contains("queued task 0"), "{text}");
    }
}
