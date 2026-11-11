//! The record of closed work: Done and Dropped tasks with when, how long and what came of them.
//! One function feeds both the board's Closed view and `claude-board closed`.
use crate::model::{Pr, Stage, Task};
use crate::store::Store;
use std::sync::OnceLock;

#[derive(Debug)]
pub struct Closed {
    pub task: Task,
    pub closed_at: i64,
    pub started_at: Option<i64>,
    pub prs: Vec<Pr>,
    pub summary: Option<String>,
}

/// Done and Dropped tasks closed at or after `since`, newest first.
pub fn closed(store: &Store, since: Option<i64>) -> anyhow::Result<Vec<Closed>> {
    let mut out = vec![];
    for task in store.tasks()?.into_iter().filter(|t| matches!(t.stage, Stage::Done | Stage::Dropped)) {
        let closed_at = task.stage_at;
        if since.is_some_and(|s| closed_at < s) {
            continue;
        }
        let events = store.events(task.id)?;
        let started_at = events.iter().find(|e| e.kind == "stage" && e.detail.ends_with("→ active")).map(|e| e.at);
        let summary = events.iter().rev().find(|e| e.kind == "summary").map(|e| e.detail.clone());
        out.push(Closed { prs: store.prs(task.id)?, task, closed_at, started_at, summary });
    }
    out.sort_by_key(|c| std::cmp::Reverse(c.closed_at));
    Ok(out)
}

/// `3d 4h`, `5h 12m`, `8m`; `—` when the task never started.
pub fn duration_text(started: Option<i64>, closed: i64) -> String {
    let Some(s) = started else { return "—".into() };
    let secs = (closed - s).max(0);
    let (d, h, m) = (secs / 86400, secs % 86400 / 3600, secs % 3600 / 60);
    match (d, h) {
        (0, 0) => format!("{m}m"),
        (0, _) => format!("{h}h {m}m"),
        _ => format!("{d}d {h}h"),
    }
}

/// `owner/repo#12` for a GitHub PR URL, the URL itself otherwise.
fn pr_label(url: &str) -> String {
    let parts: Vec<&str> = url.strip_prefix("https://github.com/").unwrap_or("").split('/').collect();
    match parts[..] {
        [owner, repo, "pull", n] if n.parse::<u64>().is_ok() => format!("{owner}/{repo}#{n}"),
        _ => url.to_string(),
    }
}

impl Closed {
    pub fn took(&self) -> String {
        duration_text(self.started_at, self.closed_at)
    }

    /// The board's PR summary: short links and waiver reasons, empty with no PR rows.
    pub fn pr_summary(&self) -> String {
        let one = |p: &Pr| match (&p.url, &p.waiver) {
            (Some(u), _) => Some(pr_label(u)),
            (None, Some(w)) => Some(format!("no PR: {w}")),
            _ => None,
        };
        self.prs.iter().filter_map(one).collect::<Vec<_>>().join(", ")
    }
}

/// Seconds east of UTC. std has no local time, so ask `date` once; UTC if that fails.
fn utc_offset() -> i64 {
    static OFFSET: OnceLock<i64> = OnceLock::new();
    *OFFSET.get_or_init(|| {
        let out = std::process::Command::new("date").arg("+%z").output().ok().and_then(|o| String::from_utf8(o.stdout).ok());
        let z = out.as_deref().map(str::trim).unwrap_or("");
        let (sign, n) = match (z.get(..1), z.get(1..5).and_then(|n| n.parse::<i64>().ok())) {
            (Some("-"), Some(n)) => (-1, n),
            (Some("+"), Some(n)) => (1, n),
            _ => return 0,
        };
        sign * (n / 100 * 3600 + n % 100 * 60)
    })
}

/// `YYYY-MM-DD` in local time; Howard Hinnant's civil-from-days.
pub fn date(ts: i64) -> String {
    let z = (ts + utc_offset()).div_euclid(86400) + 719468;
    let (era, doe) = (z.div_euclid(146097), z.rem_euclid(146097));
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let (d, m) = (doy - (153 * mp + 2) / 5 + 1, if mp < 10 { mp + 3 } else { mp - 9 });
    format!("{:04}-{m:02}-{d:02}", yoe + era * 400 + i64::from(m <= 2))
}

/// The Markdown report `claude-board closed` prints.
pub fn report(store: &Store, rows: &[Closed], days: i64, now: i64) -> anyhow::Result<String> {
    let from = if days > 0 { now - days * 86400 } else { rows.last().map_or(now, |c| c.closed_at) };
    let mut out = format!("# Closed work: {} to {}\n\n", date(from), date(now));
    if rows.is_empty() {
        out.push_str("Nothing closed in this period.\n");
        return Ok(out);
    }
    let one_line = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let done = rows.iter().filter(|c| c.task.stage == Stage::Done).count();
    out.push_str(&format!("{done} done · {} dropped\n", rows.len() - done));
    for (stage, head, verb) in [(Stage::Done, "Done", "Closed"), (Stage::Dropped, "Dropped", "Dropped")] {
        let part: Vec<&Closed> = rows.iter().filter(|c| c.task.stage == stage).collect();
        if part.is_empty() {
            continue;
        }
        out.push_str(&format!("\n## {head}\n"));
        for c in part {
            out.push_str(&format!("\n### {}\n- {verb}: {} (took {})\n", one_line(&c.task.title), date(c.closed_at), c.took()));
            let repos: Vec<String> = store.task_repos(c.task.id)?.iter().map(|r| crate::gate::basename(r).to_string()).collect();
            if !repos.is_empty() {
                out.push_str(&format!("- Repos: {}\n", repos.join(", ")));
            }
            for p in &c.prs {
                match (&p.url, &p.waiver) {
                    (Some(u), _) => out.push_str(&format!("- PR: {}\n", one_line(u))),
                    (None, Some(w)) => out.push_str(&format!("- No PR: {}\n", one_line(w))),
                    _ => {}
                }
            }
            if let Some(e) = &c.task.explainer {
                out.push_str(&format!("- Explainer: {}\n", one_line(e)));
            }
            if let Some(s) = c.summary.as_deref().filter(|s| !s.trim().is_empty()) {
                out.push_str(&format!("\n{}\n", s.trim()));
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::app::tests::add;

    #[test]
    fn duration_text_formats() {
        assert_eq!(duration_text(Some(0), 8 * 60), "8m");
        assert_eq!(duration_text(Some(0), 5 * 3600 + 12 * 60), "5h 12m");
        assert_eq!(duration_text(Some(0), 3 * 86400 + 4 * 3600 + 59), "3d 4h");
        assert_eq!(duration_text(None, 99), "—");
    }

    #[test]
    fn pr_label_shortens_github_urls_only() {
        assert_eq!(pr_label("https://github.com/acme/app/pull/12"), "acme/app#12");
        assert_eq!(pr_label("https://example.com/x/1"), "https://example.com/x/1");
    }

    /// Done at `closed`, started at `started` (None: dropped from the queue).
    pub fn close(s: &Store, title: &str, stage: Stage, started: Option<i64>, closed: i64) -> i64 {
        let id = add(s, title, stage);
        s.conn.execute("UPDATE tasks SET stage_at=?1 WHERE id=?2", rusqlite::params![closed, id]).unwrap();
        if let Some(at) = started {
            s.add_event(id, "stage", "queue → active").unwrap();
            s.conn.execute("UPDATE events SET at=?1 WHERE task_id=?2", rusqlite::params![at, id]).unwrap();
        }
        id
    }

    #[test]
    fn closed_lists_done_and_dropped_newest_first_with_durations() {
        let s = Store::open_in_memory();
        let done = close(&s, "shipped", Stage::Done, Some(1000), 1000 + 3 * 3600);
        s.add_event(done, "summary", "It works.").unwrap();
        let dropped = close(&s, "abandoned", Stage::Dropped, None, 50_000);
        add(&s, "still queued", Stage::Queue);
        let all = closed(&s, None).unwrap();
        assert_eq!(all.iter().map(|c| c.task.id).collect::<Vec<_>>(), [dropped, done]);
        assert_eq!((all[0].started_at, all[0].took().as_str()), (None, "—"));
        assert_eq!((all[1].started_at, all[1].took().as_str(), all[1].summary.as_deref()), (Some(1000), "3h 0m", Some("It works.")));
        let recent = closed(&s, Some(20_000)).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].task.id, dropped);
    }
}
