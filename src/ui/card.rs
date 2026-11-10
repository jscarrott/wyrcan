//! A card: rounded border, state circle, full wrapped title, status line, label chips, flag and missing checks.
use super::theme;
use crate::app::Card;
use ratatui::text::{Line, Span};

/// Display width (CJK and emoji take two columns).
fn dw(s: &str) -> usize {
    Span::raw(s).width()
}

/// Word-wraps to `width` columns, hard-splitting words that don't fit; newlines count as spaces.
pub fn wrap(s: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let (mut out, mut cur) = (vec![], String::new());
    for mut w in s.split_whitespace() {
        loop {
            let (cl, wl) = (dw(&cur), dw(w));
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
            // Hard split: as many chars as fit, at least one so it always progresses.
            let (mut used, mut cut) = (0, 0);
            for (i, ch) in w.char_indices() {
                let cw = dw(&w[i..i + ch.len_utf8()]);
                if i > 0 && used + cw > width {
                    break;
                }
                used += cw;
                cut = i + ch.len_utf8();
            }
            out.push(w[..cut].to_string());
            w = &w[cut..];
        }
    }
    if !cur.is_empty() || out.is_empty() {
        out.push(cur);
    }
    out
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

pub fn home_short(path: &str) -> String {
    let home = crate::config::home().to_string_lossy().into_owned();
    match path.strip_prefix(&home) {
        Some(rest) if !home.is_empty() => format!("~{rest}"),
        _ => path.to_string(),
    }
}

/// The line under the title: agent state, PR state or how long it's been parked.
fn status(c: &Card, now: i64) -> Vec<Span<'static>> {
    use crate::model::{PrState, Stage};
    let dim = theme::dim();
    match c.task.as_ref().map(|t| t.stage) {
        Some(Stage::Review) => {
            let open: Vec<_> = c.prs.iter().filter(|p| p.waiver.is_none()).cloned().collect();
            let text = match crate::gate::worst(&open) {
                Some(PrState::Waiting) => {
                    let who = open.iter().map(|p| p.reviewers.as_str()).filter(|r| !r.is_empty()).collect::<Vec<_>>().join(", ");
                    format!("⧗ waiting on {}", if who.is_empty() { "review" } else { &who })
                }
                Some(PrState::Approved) => "✓ approved".into(),
                Some(PrState::Merged) => "✓ merged".into(),
                Some(PrState::ChangesRequested) => "✗ changes requested".into(),
                Some(PrState::Closed) => "✗ PR closed".into(),
                Some(PrState::Unknown) => "PR state unknown".into(),
                None => "in review".into(),
            };
            vec![Span::styled(text, dim)]
        }
        Some(Stage::Parked) => vec![Span::styled(format!("parked {}", age(now, c.task.as_ref().map_or(now, |t| t.stage_at))), dim)],
        Some(Stage::Queue) => vec![],
        _ => match &c.session {
            Some(s) => {
                let (glyph, name, color) = theme::agent(s.state);
                vec![Span::styled(format!("{glyph} {name} · {}", age(now, s.state_at)), ratatui::style::Style::new().fg(color))]
            }
            None => vec![Span::styled("no session", dim)],
        },
    }
}

/// Cuts spans down to `max` columns; only status lines and chips in very narrow cards need it.
fn fit(spans: Vec<Span<'static>>, max: usize) -> Vec<Span<'static>> {
    let mut left = max;
    let mut out = vec![];
    for s in spans {
        let w = s.width();
        if w <= left {
            left -= w;
            out.push(s);
            continue;
        }
        let mut cut = String::new();
        for ch in s.content.chars() {
            let cw = dw(ch.encode_utf8(&mut [0; 4]));
            if cw > left {
                break;
            }
            left -= cw;
            cut.push(ch);
        }
        out.push(Span::styled(cut, s.style));
        break;
    }
    out
}

/// Packs label chips into lines no wider than `width`.
pub fn chips(labels: &[String], width: usize) -> Vec<Vec<Span<'static>>> {
    let mut rows: Vec<Vec<Span>> = vec![];
    let mut used = 0;
    for l in labels {
        let chip = Span::styled(format!(" {l} "), theme::chip());
        let w = chip.width();
        match rows.last_mut() {
            Some(row) if used + 1 + w <= width => {
                row.push(Span::raw(" "));
                row.push(chip);
                used += 1 + w;
            }
            _ => {
                rows.push(vec![chip]);
                used = w;
            }
        }
    }
    rows
}

pub fn card_lines(c: &Card, width: usize, selected: bool, now: i64) -> Vec<Line<'static>> {
    use ratatui::style::{Color, Style};
    // Border, a space each side, and a 2-column indent that lines up with the title after the circle.
    let inner = width.saturating_sub(4).max(1);
    let text_w = inner.saturating_sub(2).max(1);
    let indent = || Span::raw("  ");
    let mut body: Vec<Vec<Span<'static>>> = vec![];

    let title = match (&c.task, &c.session) {
        (Some(t), _) => t.title.clone(),
        (None, Some(s)) => home_short(&s.cwd),
        _ => String::new(),
    };
    let (circle, ccolor) = theme::circle(c.session.as_ref().map(|s| s.state));
    for (i, l) in wrap(&title, text_w).into_iter().enumerate() {
        let lead = if i == 0 { Span::styled(format!("{circle} "), Style::new().fg(ccolor)) } else { indent() };
        body.push(vec![lead, Span::styled(l, theme::bold())]);
    }
    let st = status(c, now);
    if !st.is_empty() {
        body.push([vec![indent()], st].concat());
    }
    for p in c.prs.iter().filter(|p| p.waiver.is_some()) {
        let why = format!("no PR for {}: {}", crate::gate::basename(&p.root), p.waiver.as_deref().unwrap_or_default());
        body.extend(wrap(&why, text_w).into_iter().map(|l| vec![indent(), Span::styled(l, theme::dim())]));
    }
    let labels = c.task.as_ref().map_or(&[][..], |t| &t.labels[..]);
    body.extend(chips(labels, text_w).into_iter().map(|row| [vec![indent()], row].concat()));
    if let Some(f) = &c.flag {
        let color = if f == "ready to close" { Color::Green } else { theme::ACCENT };
        body.extend(wrap(&format!("⚑ {f}"), text_w).into_iter().map(|l| vec![indent(), Span::styled(l, Style::new().fg(color))]));
    }
    if !c.missing.is_empty() {
        let m = format!("needs: {}", c.missing.join(" · "));
        body.extend(wrap(&m, text_w).into_iter().map(|l| vec![indent(), Span::styled(l, theme::dim())]));
    }

    let border = Style::new().fg(if selected { theme::ACCENT } else { theme::DIM });
    let rule = "─".repeat(width.saturating_sub(2));
    let mut lines = vec![Line::styled(format!("╭{rule}╮"), border)];
    for spans in body {
        let mut spans = fit(spans, inner);
        let used: usize = spans.iter().map(|s| s.width()).sum();
        spans.insert(0, Span::styled("│ ", border));
        spans.push(Span::raw(" ".repeat(inner.saturating_sub(used))));
        spans.push(Span::styled(" │", border));
        lines.push(Line::from(spans));
    }
    lines.push(Line::styled(format!("╰{rule}╯"), border));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AgentState, Session, SessionKind, Stage, Task};

    pub fn card(title: &str) -> Card {
        let task = Task {
            id: 1,
            title: title.into(),
            description: String::new(),
            kind: "default".into(),
            stage: Stage::Active,
            position: 0.0,
            dir: None,
            labels: vec!["delta".into(), "api".into()],
            explainer: None,
            todoist_id: None,
            created_at: 0,
            stage_at: 0,
        };
        let session = Session {
            id: "s".into(),
            task_id: Some(1),
            kind: SessionKind::Task,
            cwd: "/w".into(),
            state: AgentState::Working,
            state_at: 0,
            zellij_session: None,
            zellij_pane: None,
            edited: true,
            current: true,
        };
        Card { task: Some(task), session: Some(session), prs: vec![], missing: vec!["no explainer".into()], flag: Some("working outside Active".into()) }
    }

    fn text(lines: &[Line]) -> String {
        lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn wrap_breaks_words_and_long_tokens() {
        assert_eq!(wrap("fix the flaky login test", 10), ["fix the", "flaky", "login test"]);
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap("", 4), [""]);
        assert_eq!(wrap("two\nlines", 20), ["two lines"]);
        assert_eq!(wrap("修复登录", 4), ["修复", "登录"]);
    }

    #[test]
    fn age_formats() {
        assert_eq!(age(1000, 1000), "now");
        assert_eq!(age(1000, 940), "1m");
        assert_eq!(age(10000, 2800), "2h");
        assert_eq!(age(200000, 0), "2d");
    }

    #[test]
    fn card_has_border_title_status_chips_and_flags() {
        let t = text(&card_lines(&card("Fix auth timeout"), 30, false, 240));
        let lines: Vec<&str> = t.lines().collect();
        assert!(lines[0].starts_with('╭') && lines[0].ends_with('╮'), "{t}");
        assert!(lines.last().unwrap().starts_with('╰'), "{t}");
        for want in ["◉ Fix auth timeout", "● working · 4m", " delta ", " api ", "working outside Active", "no explainer"] {
            assert!(t.contains(want), "missing {want:?} in\n{t}");
        }
        // Every line is exactly the card's width.
        assert!(lines.iter().all(|l| dw(l) == 30), "{t}");
    }

    #[test]
    fn cards_survive_awkward_titles_and_sizes() {
        let long = "abcdefghij".repeat(8);
        for title in [long.as_str(), "修复 登录 超时 🚀", "first line\nsecond line"] {
            for width in [10, 24, 60] {
                let lines = card_lines(&card(title), width, true, 0);
                assert!(lines.iter().all(|l| l.width() <= width.max(6)), "{title:?} at {width}");
            }
        }
        // At width 24 the long word appears whole, split across lines.
        let t = text(&card_lines(&card(&long), 24, false, 0));
        let joined: String = t.chars().filter(|c| c.is_ascii_lowercase()).collect();
        assert!(joined.contains(&long), "{t}");
        // A 10×5 terminal, and a wide but very short one, draw without panicking.
        use crate::app::{App, Group, Snapshot};
        let snap = Snapshot { groups: vec![(Group::Active, vec![card(&long), card("修复 登录 超时 🚀")])], max_active: 3, ..Default::default() };
        let mut app = App::new(snap);
        for w in [10, 140] {
            let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, 5)).unwrap();
            term.draw(|f| super::super::draw(f, &mut app)).unwrap();
        }
    }
}
