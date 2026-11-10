//! Overlays: the task view, quick add and the one-line prompt box.
use super::card::{age, chips, home_short, wrap};
use super::theme;
use crate::app::{parse_quick_add, Input, TaskView};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph};
use ratatui::Frame;

/// `YYYY-MM-DD` (UTC) of a Unix time; Howard Hinnant's civil-from-days.
fn date(ts: i64) -> String {
    let z = ts.div_euclid(86400) + 719468;
    let (era, doe) = (z.div_euclid(146097), z.rem_euclid(146097));
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let (d, m) = (doy - (153 * mp + 2) / 5 + 1, if mp < 10 { mp + 3 } else { mp - 9 });
    format!("{:04}-{m:02}-{d:02}", yoe + era * 400 + i64::from(m <= 2))
}

/// The task view: title, description and thread on the left, the facts in a side panel on the right.
/// `gate` is what the card shows for the review gate; empty leaves the field out.
pub fn draw_task(f: &mut Frame, area: Rect, v: &mut TaskView, gate: Vec<String>, now: i64) {
    f.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::ACCENT))
        .title(Span::styled(format!(" #{} ", v.id), theme::dim()))
        .padding(Padding::new(2, 2, 1, 0));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let Some(d) = &v.detail else {
        f.render_widget(Line::styled("Loading…", theme::dim()), inner);
        return;
    };
    let [left, right] = Layout::horizontal([Constraint::Fill(2), Constraint::Fill(1)]).spacing(2).areas(inner);

    let w = left.width as usize;
    let mut lines: Vec<Line> = wrap(&d.task.title, w).into_iter().map(|l| Line::styled(l, theme::bold())).collect();
    lines.extend(chips(&d.task.labels, w).into_iter().map(Line::from));
    lines.push(Line::raw(""));
    if d.task.description.trim().is_empty() {
        lines.push(Line::styled("No description", theme::dim()));
    }
    for para in d.task.description.trim().lines() {
        lines.extend(wrap(para, w).into_iter().map(Line::raw));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![Span::styled("Thread", theme::bold()), Span::styled(format!("  {}", d.events.len()), theme::dim())]));
    lines.push(Line::styled("─".repeat(w), theme::dim()));
    for e in &d.events {
        let when = age(now, e.at);
        // Wrapped as one string so the age leads the first line; later lines indent under the text.
        let pad = when.len() + 1;
        for (i, l) in wrap(&format!("{when} {}: {}", e.kind, e.detail), w.saturating_sub(pad)).into_iter().enumerate() {
            lines.push(match i {
                // A pane narrower than the age hard-splits it, so the line may not start with it.
                0 => match l.strip_prefix(when.as_str()) {
                    Some(rest) => Line::from(vec![Span::styled(when.clone(), theme::dim()), Span::raw(rest.to_string())]),
                    None => Line::raw(l),
                },
                _ => Line::raw(format!("{:pad$}{l}", "")),
            });
        }
    }
    let side = Block::new().borders(Borders::LEFT).border_style(theme::dim()).padding(Padding::left(2));
    let sw = side.inner(right).width as usize;
    let mut facts: Vec<Line> = vec![];
    let mut field = |name: &str, values: Vec<String>| {
        if !facts.is_empty() {
            facts.push(Line::raw(""));
        }
        facts.push(Line::styled(name.to_string(), theme::dim()));
        for v in if values.is_empty() { vec!["none".into()] } else { values } {
            facts.extend(wrap(&v, sw).into_iter().map(Line::raw));
        }
    };
    let t = &d.task;
    field("Stage", vec![format!("{} · {}", t.stage, age(now, t.stage_at))]);
    field("Directory", t.dir.iter().map(|p| home_short(p)).collect());
    field("Repos", d.repos.iter().map(|p| home_short(p)).collect());
    let prs = d.prs.iter().map(|p| {
        let repo = crate::gate::basename(&p.root);
        match (&p.waiver, p.reviewers.is_empty()) {
            (Some(why), _) => format!("{repo}: no PR, {why}"),
            (None, true) => format!("{repo} {}", p.state.as_str().replace('_', " ")),
            (None, false) => format!("{repo} {} · {}", p.state.as_str().replace('_', " "), p.reviewers),
        }
    });
    field("PRs", prs.collect());
    field("Explainer", t.explainer.iter().map(|p| home_short(p)).collect());
    if !gate.is_empty() {
        field("Gate", gate);
    }
    field("Session", d.session.iter().map(|s| format!("{} · {}", s.id, s.state)).collect());
    field("Created", vec![date(t.created_at)]);
    // Both sides scroll together, as far as the longer one needs.
    let h = inner.height as usize;
    v.scroll = v.scroll.min(lines.len().max(facts.len()).saturating_sub(h) as u16);
    f.render_widget(Paragraph::new(lines).scroll((v.scroll, 0)), left);
    f.render_widget(Paragraph::new(facts).block(side).scroll((v.scroll, 0)), right);
}

/// A one-line text box with its label in the border and a block cursor at `cursor` (in chars).
pub fn draw_prompt(f: &mut Frame, area: Rect, label: &str, text: &str, cursor: usize) {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::ACCENT))
        .title(Span::styled(format!(" {label} "), theme::bold()))
        .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    f.render_widget(Clear, area);
    f.render_widget(block, area);
    // shortcut: scrolls by chars, so very wide (CJK) text can still overflow the box; fine for titles and paths.
    let skip = cursor.saturating_sub((inner.width as usize).saturating_sub(1));
    let chars: Vec<char> = text.chars().skip(skip).collect();
    let at = cursor - skip;
    let (before, rest): (String, String) = (chars[..at.min(chars.len())].iter().collect(), chars[at.min(chars.len())..].iter().collect());
    let mut under = rest.chars();
    let cur = under.next().map_or(" ".to_string(), String::from);
    let line = Line::from(vec![Span::raw(before), Span::styled(cur, Style::new().bg(Color::White).fg(Color::Black)), Span::raw(under.collect::<String>())]);
    f.render_widget(line, inner);
}

/// Quick add: the box, and under it what it will add, parsed as you type.
pub fn draw_quick_add(f: &mut Frame, area: Rect, q: &Input) {
    let [boxed, preview] = Layout::vertical([Constraint::Length(3), Constraint::Length(1)]).areas(area);
    draw_prompt(f, boxed, "Add task", &q.text, q.cursor);
    let (title, dir, labels) = parse_quick_add(&q.text);
    let mut spans = vec![Span::raw("  ")];
    if title.is_empty() {
        spans.push(Span::styled("Type a title, with @label and #directory", theme::dim()));
    } else {
        spans.push(Span::styled(title, theme::bold()));
    }
    for l in labels {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(format!(" {l} "), theme::chip()));
    }
    if let Some(d) = dir {
        spans.push(Span::styled(format!("  in {}", home_short(&d)), theme::dim()));
        if !std::path::Path::new(&d).is_dir() {
            spans.push(Span::styled("  no such directory", Style::new().fg(Color::Yellow)));
        }
    }
    f.render_widget(Clear, preview);
    f.render_widget(Line::from(spans), preview);
}

#[cfg(test)]
mod tests {
    use crate::app::tests::{add, app, cfg, key, session};
    use crate::app::{detail, App, Prompt};
    use ratatui::crossterm::event::KeyCode;
    use crate::model::{AgentState, PrState, Stage};
    use crate::store::Store;
    use ratatui::{backend::TestBackend, Terminal};

    fn text(ap: &mut App, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| crate::ui::draw(f, ap)).unwrap();
        let buf = term.backend().buffer();
        (0..h).map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect::<String>()).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn task_view_shows_thread_and_side_panel() {
        let s = Store::open_in_memory();
        let id = add(&s, "Fix auth timeout on token refresh", Stage::Active);
        s.update_task(id, "Fix auth timeout on token refresh", "Tokens expire mid-request.", Some("/w"), &["delta".into()]).unwrap();
        session(&s, "sess-1", Some(id), AgentState::Waiting);
        s.mark_edited("sess-1", "/w/delta", "git main").unwrap();
        s.set_pr(id, "/w/delta", Some("https://x/pr/1"), None).unwrap();
        s.set_pr_state(id, "/w/delta", PrState::Waiting, "sam").unwrap();
        s.add_event(id, "summary", "Refreshed tokens before expiry").unwrap();
        let mut ap = app(&s, &cfg(3));
        ap.on_key_event(key(' '));
        ap.modal.as_mut().unwrap().detail = detail(&s, id).unwrap();
        let t = text(&mut ap, 140, 40);
        for want in [
            "Fix auth timeout on token refresh",
            "Tokens expire mid-request.",
            "now pr: delta: waiting",
            "now summary: Refreshed tokens before expiry",
            "Stage",
            "active",
            "Directory",
            "Repos",
            "/w/delta",
            "PRs",
            "delta waiting · sam",
            "Explainer",
            "none",
            "Session",
            "sess-1 · waiting",
            "Created",
        ] {
            assert!(t.contains(want), "missing {want:?} in\n{t}");
        }
        // The thread is oldest first.
        assert!(t.find("pr: delta").unwrap() < t.find("summary: Refreshed").unwrap(), "{t}");
    }

    /// The task view, quick add and the prompt box survive tiny terminals (a crash in raw mode wrecks the terminal).
    #[test]
    fn overlays_draw_at_tiny_sizes() {
        let s = Store::open_in_memory();
        let id = add(&s, "Fix auth timeout", Stage::Active);
        s.add_event(id, "summary", "Refreshed tokens").unwrap();
        for (w, h) in [(10, 5), (20, 10), (25, 10), (1, 1)] {
            let mut ap = app(&s, &cfg(3));
            ap.on_key_event(key(' '));
            ap.modal.as_mut().unwrap().detail = detail(&s, id).unwrap();
            text(&mut ap, w, h);
            let mut ap = app(&s, &cfg(3));
            ap.on_key_event(key('+'));
            for c in "a long title @label #~/x".chars() {
                ap.on_key_event(key(c));
            }
            ap.on_key_event(KeyCode::Left.into());
            text(&mut ap, w, h);
            let mut ap = app(&s, &cfg(3));
            ap.prompt = Some(Prompt::dir(id, false, "Fix auth timeout", "/some/long/directory".into()));
            text(&mut ap, w, h);
        }
    }

    #[test]
    fn task_view_shows_gate_status() {
        let s = Store::open_in_memory();
        let id = add(&s, "a", Stage::Active);
        let mut ap = app(&s, &cfg(3));
        ap.on_key_event(key(' '));
        ap.modal.as_mut().unwrap().detail = detail(&s, id).unwrap();
        let t = text(&mut ap, 140, 40);
        assert!(t.contains("Gate") && t.contains("no explainer"), "{t}");
        // The gate wants the explainer on disk, and a PR or a reason even when no repo was edited.
        s.set_explainer(id, &std::env::temp_dir().to_string_lossy()).unwrap();
        s.set_pr(id, "/w", None, Some("notes only")).unwrap();
        ap.set_snapshot(crate::app::snapshot(&s, &cfg(3)).unwrap());
        ap.modal.as_mut().unwrap().detail = detail(&s, id).unwrap();
        assert!(text(&mut ap, 140, 40).contains("ready for review"));
        let r = add(&s, "r", Stage::Review);
        let mut ap = app(&s, &cfg(3));
        ap.selected = Some(crate::app::CardKey::Task(r));
        ap.on_key_event(key(' '));
        ap.modal.as_mut().unwrap().detail = detail(&s, r).unwrap();
        assert!(text(&mut ap, 140, 40).contains("passed"));
    }

    #[test]
    fn quick_add_preview_shows_parsed_parts() {
        let s = Store::open_in_memory();
        let mut ap = app(&s, &cfg(3));
        ap.on_key_event(key('+'));
        for c in "Fix login @delta #/tmp".chars() {
            ap.on_key_event(key(c));
        }
        let t = text(&mut ap, 140, 30);
        let lines: Vec<&str> = t.lines().collect();
        let boxed = lines.iter().position(|l| l.contains("Fix login @delta #/tmp")).unwrap_or_else(|| panic!("{t}"));
        let preview = lines[boxed + 2];
        for want in ["Fix login", " delta ", "/tmp"] {
            assert!(preview.contains(want), "missing {want:?} in {preview:?}");
        }
        assert!(!preview.contains('@') && !preview.contains('#'), "{preview:?}");
    }
}
