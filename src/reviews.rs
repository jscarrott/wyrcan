//! Parsers for `gh` output used to import PRs awaiting the user's review. Pure: no process calls.
use crate::config::Config;
use crate::model::{AgentState, PrState, Stage};
use crate::pr;
use crate::store::{NewTask, Store};
use anyhow::Result;
use std::collections::HashMap;

#[derive(Debug, PartialEq)]
pub struct Found {
    pub url: String,
    pub title: String,
    pub author: String,
    pub repo: String,
    pub number: u64,
}

#[derive(Debug, PartialEq)]
pub struct PrReview {
    pub state: PrState,
    pub reviewed_by: Vec<String>,
    pub head: String,
}

/// From `gh search prs --json url,title,author,repository,number`.
pub fn parse_search(json: &str) -> Option<Vec<Found>> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let mut out: Vec<Found> = Vec::new();
    for i in v.as_array()? {
        let Some(url) = i["url"].as_str().filter(|u| u.starts_with("https://")) else { continue };
        if out.iter().any(|f| f.url == url) {
            continue;
        }
        out.push(Found {
            url: url.into(),
            title: i["title"].as_str().unwrap_or_default().chars().map(|c| if c.is_control() { ' ' } else { c }).collect(),
            author: i["author"]["login"].as_str().unwrap_or_default().into(),
            repo: i["repository"]["nameWithOwner"].as_str().unwrap_or_default().into(),
            number: i["number"].as_u64().unwrap_or(0),
        });
    }
    Some(out)
}

/// From `gh pr view URL --json state,reviews,headRefName`.
pub fn parse_pr_review(json: &str) -> Option<PrReview> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let state = match v["state"].as_str()? {
        "MERGED" => PrState::Merged,
        "CLOSED" => PrState::Closed,
        "OPEN" => PrState::Waiting,
        _ => PrState::Unknown,
    };
    let mut reviewed_by: Vec<String> = Vec::new();
    for l in v["reviews"].as_array().into_iter().flatten().filter_map(|r| r["author"]["login"].as_str()) {
        if !reviewed_by.iter().any(|x| x == l) {
            reviewed_by.push(l.into());
        }
    }
    Some(PrReview { state, reviewed_by, head: v["headRefName"].as_str().unwrap_or_default().into() })
}

/// `git@github.com:o/r.git` or `https://github.com/o/r(.git)` -> `o/r`.
pub fn parse_remote(url: &str) -> Option<String> {
    let rest = url.strip_prefix("git@github.com:").or_else(|| url.strip_prefix("https://github.com/"))?;
    let rest = rest.trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let (o, r) = rest.split_once('/')?;
    (!o.is_empty() && !r.is_empty() && !r.contains('/')).then(|| rest.to_string())
}

/// One poll of GitHub. `None` means that `gh` call failed.
pub struct Poll {
    pub requested: Option<Vec<Found>>,
    pub open_in_repos: Option<Vec<Found>>,
    pub prs: HashMap<String, PrReview>,
    pub me: String,
}

/// Repos edited for tasks that moved within this many days count as active.
const ACTIVE_DAYS: i64 = 30;

const SEARCH: [&str; 7] = ["search", "prs", "--state=open", "--json", "url,title,author,repository,number", "--limit", "100"];

/// `origin` of the repo at `root`, as `owner/name`; git first, then jj for jj-only checkouts.
fn repo_slug(root: &str, path: Option<&str>) -> Option<String> {
    let git = pr::gh_args("git", path, &["-C", root, "remote", "get-url", "origin"]);
    let url = git.or_else(|| {
        let out = pr::gh_args("jj", path, &["-R", root, "git", "remote", "list"])?;
        out.lines().find_map(|l| l.strip_prefix("origin ").map(str::to_string))
    })?;
    parse_remote(url.trim())
}

/// Asks GitHub what the board needs. Without a login, every field is empty so `apply` changes nothing.
fn fetch_with(store: &Store, cfg: &Config, path: Option<&str>) -> Poll {
    let gh = |args: &[&str]| pr::gh_args("gh", path, args);
    let none = Poll { requested: None, open_in_repos: None, prs: HashMap::new(), me: String::new() };
    let Some(me) = gh(&["api", "user", "--jq", ".login"]).map(|m| m.trim().to_string()).filter(|m| !m.is_empty()) else { return none };
    let requested = gh(&[&SEARCH[..], &["--review-requested=@me"]].concat()).and_then(|j| parse_search(&j));
    let open_in_repos = cfg.reviews_all_open.then(|| {
        let since = crate::store::now() - ACTIVE_DAYS * 86400;
        let mut repos: Vec<String> = store.active_repo_roots(since).unwrap_or_default().iter().filter_map(|r| repo_slug(r, path)).collect();
        repos.sort();
        repos.dedup();
        let args: Vec<&str> = SEARCH.iter().copied().chain(repos.iter().flat_map(|r| ["--repo", r.as_str()])).collect();
        if repos.is_empty() { None } else { gh(&args).and_then(|j| parse_search(&j)).map(|f| f.into_iter().filter(|f| f.author != me).collect()) }
    });
    let mut prs = HashMap::new();
    for t in store.tasks().unwrap_or_default().into_iter().filter(|t| t.kind == "review" && !matches!(t.stage, Stage::Done | Stage::Dropped)) {
        let Some(url) = t.link.filter(|u| u.starts_with("https://")) else { continue };
        if let Some(r) = gh(&["pr", "view", &url, "--json", "state,reviews,headRefName"]).and_then(|j| parse_pr_review(&j)) {
            prs.insert(url, r);
        }
    }
    Poll { requested, open_in_repos: open_in_repos.flatten(), prs, me }
}

pub fn fetch(store: &Store, cfg: &Config) -> Poll {
    fetch_with(store, cfg, None)
}

/// Import new PRs as review tasks, then move known ones by what GitHub says. Missing data never moves anything.
pub fn apply(store: &Store, poll: &Poll) -> Result<()> {
    let go = |id: i64, to: Stage| store.transition(id, to, usize::MAX, &|_, _| vec![]).map(|_| ());
    let sources = [(&poll.requested, "requested"), (&poll.open_in_repos, "not requested")];
    for (found, why) in sources {
        for f in found.iter().flatten().filter(|f| !f.repo.is_empty() && f.number != 0) {
            if store.review_task(&f.url)?.is_some() {
                continue;
            }
            let label = f.repo.rsplit('/').next().unwrap_or_default().to_string();
            let desc = format!("PR by @{}", f.author);
            let id = store.add_task(NewTask { title: &f.title, description: &desc, dir: None, labels: &[label], todoist_id: None })?;
            store.set_kind(id, "review", Some(&f.url))?;
            store.replace_event(id, "review", why)?;
        }
    }
    for t in store.tasks()?.into_iter().filter(|t| t.kind == "review") {
        let Some(url) = t.link.as_deref() else { continue };
        let stage = t.stage;
        if matches!(stage, Stage::Done | Stage::Dropped) {
            continue;
        }
        let asked = poll.requested.as_ref().map(|r| r.iter().any(|f| f.url == url));
        let pr = poll.prs.get(url);
        if asked == Some(true) && stage == Stage::Parked {
            go(t.id, Stage::Queue)?;
            store.replace_event(t.id, "review", "requested")?;
        } else if let (Some(false), Some(pr)) = (asked, pr) {
            let latest = store.events(t.id)?.into_iter().rfind(|e| e.kind == "review");
            if latest.is_some_and(|e| e.detail == "requested") && !pr.reviewed_by.contains(&poll.me) {
                store.replace_event(t.id, "review", "no longer requested")?;
            }
        }
        let Some(pr) = pr else { continue };
        match pr.state {
            PrState::Merged => go(t.id, Stage::Done)?,
            PrState::Closed => go(t.id, Stage::Dropped)?,
            _ => {
                let ended = store.current_session(t.id)?.is_none_or(|s| s.state == AgentState::Ended);
                if stage == Stage::Active && pr.reviewed_by.contains(&poll.me) && ended {
                    go(t.id, Stage::Parked)?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(n: &str) -> String {
        std::fs::read_to_string(format!("{}/tests/fixtures/{n}.json", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    #[test]
    fn search_parses_dedupes_and_sanitises() {
        let f = parse_search(&fixture("search_prs")).unwrap();
        assert_eq!(f.len(), 2);
        assert_eq!(f[0], Found { url: "https://github.com/acme/delta/pull/812".into(), title: "Add pcap filter".into(), author: "alice".into(), repo: "acme/delta".into(), number: 812 });
        assert!(!f[1].title.contains('\n') && !f[1].title.contains('\u{1b}'));
        assert!(parse_search("nope").is_none());
    }

    #[test]
    fn pr_review_reads_state_reviewers_and_head() {
        let r = parse_pr_review(&fixture("pr_review")).unwrap();
        assert_eq!(r, PrReview { state: PrState::Merged, reviewed_by: vec!["john".into()], head: "feat/x".into() });
        assert!(parse_pr_review("nope").is_none());
    }

    #[test]
    fn fetch_without_gh_is_all_none() {
        let empty = std::env::temp_dir().join(format!("no-gh-{}", std::process::id()));
        std::fs::create_dir_all(&empty).unwrap();
        let p = fetch_with(&Store::open_in_memory(), &Config::from_env(), Some(empty.to_str().unwrap()));
        std::fs::remove_dir_all(&empty).ok();
        assert!(p.requested.is_none() && p.open_in_repos.is_none() && p.prs.is_empty() && p.me.is_empty());
    }

    #[test]
    fn fetch_with_failed_login_is_a_no_op() {
        let dir = std::env::temp_dir().join(format!("gh-fake-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let gh = dir.join("gh");
        std::fs::write(&gh, "#!/bin/sh\nif [ \"$1\" = api ]; then exit 0; fi\necho '[]'\n").unwrap();
        std::fs::set_permissions(&gh, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let p = fetch_with(&Store::open_in_memory(), &Config::from_env(), Some(dir.to_str().unwrap()));
        assert!(p.requested.is_none() && p.open_in_repos.is_none() && p.prs.is_empty());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn remote_urls() {
        assert_eq!(parse_remote("git@github.com:acme/delta.git").as_deref(), Some("acme/delta"));
        assert_eq!(parse_remote("https://github.com/acme/delta").as_deref(), Some("acme/delta"));
        assert_eq!(parse_remote("https://github.com/acme/delta.git").as_deref(), Some("acme/delta"));
        assert_eq!(parse_remote("/local/path"), None);
    }

    fn found(url: &str, repo: &str) -> Found {
        Found { url: url.into(), title: "T".into(), author: "al".into(), repo: repo.into(), number: 1 }
    }
    fn pr(state: PrState, by: &[&str]) -> PrReview {
        PrReview { state, reviewed_by: by.iter().map(|s| s.to_string()).collect(), head: "h".into() }
    }
    fn poll(req: Option<Vec<Found>>, open: Option<Vec<Found>>, prs: Vec<(&str, PrReview)>) -> Poll {
        Poll { requested: req, open_in_repos: open, prs: prs.into_iter().map(|(u, p)| (u.to_string(), p)).collect(), me: "me".into() }
    }
    const U: &str = "https://github.com/o/r/pull/1";
    fn ev(s: &Store, id: i64) -> String {
        s.events(id).unwrap().into_iter().rfind(|e| e.kind == "review").unwrap().detail
    }
    fn stage(s: &Store, id: i64) -> Stage {
        s.task(id).unwrap().unwrap().stage
    }
    fn imported(req: bool) -> (Store, i64) {
        let s = Store::open_in_memory();
        let f = vec![found(U, "o/r")];
        apply(&s, &poll(req.then(|| f), None, vec![])).unwrap();
        let id = s.review_task(U).unwrap().unwrap().id;
        (s, id)
    }
    fn go(s: &Store, id: i64, to: Stage) {
        s.transition(id, to, usize::MAX, &|_, _| vec![]).unwrap().unwrap();
    }

    #[test]
    fn requested_pr_becomes_a_queued_review_task() {
        let (s, id) = imported(true);
        let t = s.task(id).unwrap().unwrap();
        assert_eq!((t.title.as_str(), t.description.as_str(), t.stage), ("T", "PR by @al", Stage::Queue));
        assert_eq!(t.labels, vec!["r".to_string()]);
        assert_eq!(ev(&s, id), "requested");
    }

    #[test]
    fn open_only_pr_is_not_requested() {
        let s = Store::open_in_memory();
        apply(&s, &poll(Some(vec![]), Some(vec![found(U, "o/r")]), vec![])).unwrap();
        assert_eq!(ev(&s, s.review_task(U).unwrap().unwrap().id), "not requested");
    }

    #[test]
    fn same_pr_twice_makes_one_task() {
        let s = Store::open_in_memory();
        apply(&s, &poll(Some(vec![found(U, "o/r")]), Some(vec![found(U, "o/r")]), vec![])).unwrap();
        assert_eq!(s.tasks().unwrap().len(), 1);
        assert_eq!(ev(&s, s.tasks().unwrap()[0].id), "requested");
    }

    #[test]
    fn malformed_items_are_skipped() {
        let s = Store::open_in_memory();
        let mut z = found(U, "o/r");
        z.number = 0;
        apply(&s, &poll(Some(vec![z, found("https://x/2", "")]), None, vec![])).unwrap();
        assert!(s.tasks().unwrap().is_empty());
    }

    #[test]
    fn rerequest_unparks() {
        let (s, id) = imported(true);
        go(&s, id, Stage::Active);
        go(&s, id, Stage::Parked);
        s.replace_event(id, "review", "no longer requested").unwrap();
        apply(&s, &poll(Some(vec![found(U, "o/r")]), None, vec![])).unwrap();
        assert_eq!((stage(&s, id), ev(&s, id).as_str()), (Stage::Queue, "requested"));
    }

    #[test]
    fn dropped_request_is_noted_unless_reviewed() {
        let (s, id) = imported(true);
        let p = |by: &[&str]| poll(Some(vec![]), None, vec![(U, pr(PrState::Waiting, by))]);
        apply(&s, &p(&["me"])).unwrap();
        assert_eq!(ev(&s, id), "requested");
        apply(&s, &p(&[])).unwrap();
        assert_eq!(ev(&s, id), "no longer requested");
    }

    #[test]
    fn merged_is_done_and_closed_is_dropped() {
        for (st, want) in [(PrState::Merged, Stage::Done), (PrState::Closed, Stage::Dropped)] {
            let (s, id) = imported(true);
            apply(&s, &poll(None, None, vec![(U, pr(st, &[]))])).unwrap();
            assert_eq!(stage(&s, id), want);
        }
    }

    #[test]
    fn finished_task_is_not_reimported_or_revived() {
        let (s, id) = imported(true);
        go(&s, id, Stage::Done);
        apply(&s, &poll(Some(vec![found(U, "o/r")]), None, vec![])).unwrap();
        assert_eq!((s.tasks().unwrap().len(), stage(&s, id)), (1, Stage::Done));
    }

    #[test]
    fn reviewed_active_task_parks_once_session_ended() {
        let (s, id) = imported(true);
        go(&s, id, Stage::Active);
        let p = poll(None, None, vec![(U, pr(PrState::Waiting, &["me"]))]);
        s.link_session("s", id, "/w").unwrap();
        s.end_session("s").unwrap();
        apply(&s, &p).unwrap();
        assert_eq!(stage(&s, id), Stage::Parked);
    }

    #[test]
    fn reviewed_active_task_with_no_session_parks() {
        let (s, id) = imported(true);
        go(&s, id, Stage::Active);
        apply(&s, &poll(None, None, vec![(U, pr(PrState::Waiting, &["me"]))])).unwrap();
        assert_eq!(stage(&s, id), Stage::Parked);
    }

    #[test]
    fn submitting_a_review_keeps_a_live_session_in_reviewing() {
        let (s, id) = imported(true);
        go(&s, id, Stage::Active);
        s.link_session("s", id, "/w").unwrap();
        s.touch_session(crate::store::SessionUpdate { id: "s", cwd: "/w", state: AgentState::Waiting, zellij_session: None, zellij_pane: None }, false).unwrap();
        apply(&s, &poll(None, None, vec![(U, pr(PrState::Waiting, &["me"]))])).unwrap();
        assert_eq!(stage(&s, id), Stage::Active);
    }

    #[test]
    fn gh_failure_changes_nothing() {
        let (s, id) = imported(true);
        go(&s, id, Stage::Active);
        go(&s, id, Stage::Parked);
        apply(&s, &poll(None, None, vec![])).unwrap();
        assert_eq!((stage(&s, id), ev(&s, id).as_str()), (Stage::Parked, "requested"));
    }
}
