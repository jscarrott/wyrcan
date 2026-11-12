//! PR state from `gh`. Every failure is `Unknown` so a flaky network never refuses or crashes anything.
use crate::model::PrState;
use crate::store::Store;
use crate::zellij::Runner;
use anyhow::{bail, Context, Result};
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

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

/// Runs `prog ARGS`, killing it after 15 s; stdout on success. `path` overrides the child's PATH (tests).
pub fn gh_args(prog: &str, path: Option<&str>, args: &[&str]) -> Option<String> {
    let mut cmd = std::process::Command::new(prog);
    cmd.args(args);
    if let Some(p) = path {
        cmd.env("PATH", p);
    }
    run_limited(cmd, Duration::from_secs(15)).ok()
}

/// Runs `cmd`, killing it after `limit`. Stdout on success; otherwise its stderr (or stdout) is the error.
pub fn run_limited(mut cmd: Command, limit: Duration) -> Result<String> {
    let prog = cmd.get_program().to_string_lossy().into_owned();
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().with_context(|| format!("running {prog}"))?;
    // Both pipes are drained while it runs, so a chatty child can't fill one and stall.
    fn drain(p: Option<impl Read + Send + 'static>) -> mpsc::Receiver<Vec<u8>> {
        let (tx, rx) = mpsc::channel();
        if let Some(mut p) = p {
            std::thread::spawn(move || {
                let mut buf = [0; 8192];
                while let Ok(n @ 1..) = p.read(&mut buf) {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            });
        }
        rx
    }
    /// What the pipe gave until it closed, or until `deadline`: a grandchild (say, an ssh ControlPersist master) may hold it open.
    fn collect(rx: &mpsc::Receiver<Vec<u8>>, deadline: Instant) -> String {
        let mut v = vec![];
        while let Ok(chunk) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            v.extend(chunk);
        }
        String::from_utf8_lossy(&v).into_owned()
    }
    let (out, err) = (drain(child.stdout.take()), drain(child.stderr.take()));
    let deadline = Instant::now() + limit;
    let status = loop {
        match child.try_wait()? {
            Some(s) => break s,
            None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            None => {
                let _ = child.kill();
                let _ = child.wait();
                // The readers are left behind: a grandchild may still hold the pipes.
                bail!("{prog} took longer than {}s", limit.as_secs());
            }
        }
    };
    let (out, err) = (collect(&out, deadline), collect(&err, deadline));
    if !status.success() {
        bail!("{}", if err.trim().is_empty() { out.trim() } else { err.trim() });
    }
    Ok(out)
}

/// Runs the gh and jj calls for review workspaces under `run_limited`: 15 s, like every other gh call.
pub struct TimeoutRunner;

impl Runner for TimeoutRunner {
    fn run(&self, cmd: &[String]) -> Result<String> {
        let (prog, args) = cmd.split_first().context("empty command")?;
        // shortcut: `jj git fetch` gets 120 s, as a first fetch in a big repo can outlast 15 s; move it off the UI thread if that wait hurts.
        let fetch = prog == "jj" && args.windows(2).any(|w| w[0] == "git" && w[1] == "fetch");
        let mut c = Command::new(prog);
        c.args(args);
        run_limited(c, Duration::from_secs(if fetch { 120 } else { 15 }))
    }
}

fn gh(prog: &str, path: Option<&str>, url: &str, extra: &[&str]) -> Option<String> {
    if !url.starts_with("https://") {
        return None; // never let a value starting with `-` reach gh as a flag
    }
    gh_args(prog, path, &[&["pr", "view", url], extra].concat())
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
    fn run_limited_reports_stderr_and_kills_slow_commands() {
        let sh = |script: &str| {
            let mut c = Command::new("sh");
            c.args(["-c", script]);
            c
        };
        assert_eq!(run_limited(sh("echo hi"), Duration::from_secs(5)).unwrap(), "hi\n");
        assert_eq!(run_limited(sh("echo bad >&2; exit 1"), Duration::from_secs(5)).unwrap_err().to_string(), "bad");
        let t = Instant::now();
        assert!(run_limited(sh("sleep 5"), Duration::from_millis(200)).unwrap_err().to_string().contains("took longer"));
        assert!(t.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn run_limited_is_not_held_up_by_a_grandchild_holding_stdout() {
        let mut c = Command::new("sh");
        // Like an ssh ControlPersist master: it outlives the command with the pipe still open.
        c.args(["-c", "sleep 5 & echo hi"]);
        let t = Instant::now();
        assert_eq!(run_limited(c, Duration::from_millis(500)).unwrap(), "hi\n");
        assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
    }

    #[test]
    fn exists_rejects_non_urls_without_calling_gh() {
        assert!(!exists("not a url"));
        assert!(!exists("--help"));
    }
}
