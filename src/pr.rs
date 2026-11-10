//! PR state from `gh`. Every failure is `Unknown` so a flaky network never refuses or crashes anything.
use crate::model::PrState;
use crate::store::Store;

pub fn parse(json: &str) -> (PrState, String) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else { return (PrState::Unknown, String::new()) };
    let state = match (v["state"].as_str(), v["reviewDecision"].as_str()) {
        (Some("MERGED"), _) => PrState::Merged,
        (Some("CLOSED"), _) => PrState::Closed,
        (Some("OPEN"), Some("CHANGES_REQUESTED")) => PrState::ChangesRequested,
        (Some("OPEN"), Some("APPROVED")) => PrState::Approved,
        (Some("OPEN"), _) => PrState::Waiting,
        _ => return (PrState::Unknown, String::new()),
    };
    // Users carry `login`, teams `name`.
    let who: Vec<&str> = v["reviewRequests"].as_array().into_iter().flatten().filter_map(|r| r["login"].as_str().or(r["name"].as_str())).collect();
    (state, who.join(", "))
}

/// Runs `prog pr view URL ...`, killing it after 15 s; stdout on success. `path` overrides the child's PATH (tests).
// shortcut: stdout is read after exit, so output past the pipe buffer (64 KiB) would hang until the kill; gh's JSON here is tiny.
fn gh(prog: &str, path: Option<&str>, url: &str, extra: &[&str]) -> Option<String> {
    use std::io::Read;
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    if !url.starts_with("https://") {
        return None; // never let a value starting with `-` reach gh as a flag
    }
    let mut cmd = std::process::Command::new(prog);
    cmd.args(["pr", "view", url]).args(extra).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    if let Some(p) = path {
        cmd.env("PATH", p);
    }
    let mut child = cmd.spawn().ok()?;
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if start.elapsed() < Duration::from_secs(15) => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    status.success().then_some(out)
}

fn fetch_with(prog: &str, path: Option<&str>, url: &str) -> (PrState, String) {
    gh(prog, path, url, &["--json", "state,reviewDecision,reviewRequests"]).map_or((PrState::Unknown, String::new()), |j| parse(&j))
}

pub fn fetch(url: &str) -> (PrState, String) {
    fetch_with("gh", None, url)
}

/// Re-reads GitHub for each of the task's PRs; a failed lookup keeps the last known state.
pub fn refresh(store: &Store, task_id: i64) {
    for p in store.prs(task_id).unwrap_or_default() {
        let Some(url) = p.url.as_deref().filter(|_| p.waiver.is_none()) else { continue };
        let (state, who) = fetch(url);
        if state != PrState::Unknown {
            let _ = store.set_pr_state(task_id, &p.root, state, &who);
        }
    }
}

/// The gate's verifier.
pub fn exists(url: &str) -> bool {
    gh("gh", None, url, &["--json", "state"]).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(n: &str) -> (PrState, String) {
        parse(&std::fs::read_to_string(format!("{}/tests/fixtures/pr_{n}.json", env!("CARGO_MANIFEST_DIR"))).unwrap())
    }

    #[test]
    fn parses_the_five_outcomes() {
        assert_eq!(fixture("waiting"), (PrState::Waiting, "sam, Platform".into()));
        assert_eq!(fixture("changes").0, PrState::ChangesRequested);
        assert_eq!(fixture("approved").0, PrState::Approved);
        assert_eq!(fixture("merged").0, PrState::Merged);
        assert_eq!(fixture("closed").0, PrState::Closed);
    }

    #[test]
    fn tolerates_missing_fields_and_garbage() {
        assert_eq!(parse(r#"{"state":"OPEN"}"#), (PrState::Waiting, String::new()));
        assert_eq!(parse(r#"{"state":"DRAFT?"}"#).0, PrState::Unknown);
        assert_eq!(parse("{}").0, PrState::Unknown);
        assert_eq!(parse("not json").0, PrState::Unknown);
        assert_eq!(parse(r#"{"state":"OPEN","reviewRequests":"x"}"#).0, PrState::Waiting);
    }

    #[test]
    fn gh_failure_is_unknown() {
        assert_eq!(fetch_with("gh", Some(""), "https://github.com/o/r/pull/1"), (PrState::Unknown, String::new()));
    }

    #[test]
    fn exists_rejects_non_urls_without_calling_gh() {
        assert!(!exists("not a url"));
        assert!(!exists("--help"));
    }
}
