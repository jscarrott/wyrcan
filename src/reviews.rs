//! Imports PRs awaiting the user's review, moves them by what GitHub says, and makes and removes their jj workspaces.
use crate::config::Config;
use crate::model::{AgentState, PrState, Stage};
use crate::pr;
use crate::store::{NewTask, Store};
use crate::zellij::Runner;
use anyhow::Result;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

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

/// `https://host/o/r/pull/7` -> ("r", 7); `None` unless the repo name is plain enough to put in a path.
pub fn pr_ref(link: &str) -> Option<(&str, u64)> {
    let (base, n) = link.strip_prefix("https://")?.rsplit_once("/pull/")?;
    let repo = base.rsplit('/').next()?;
    let plain = !repo.is_empty() && !repo.starts_with(['-', '.']) && repo.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    plain.then_some((repo, n.parse().ok()?))
}

pub fn workspace_path(home: &Path, repo_name: &str, number: u64) -> PathBuf {
    home.join("git-work").join(format!("{repo_name}-{number}-review"))
}

/// Fetch the PR's head from origin, then add workspace `name` at it in `path`; both in the parent repo.
pub fn workspace_cmds(parent: &str, path: &str, name: &str, head: &str) -> Vec<Vec<String>> {
    let jj = |a: &[&str]| ["jj", "-R", parent].iter().chain(a).map(|s| s.to_string()).collect();
    vec![jj(&["git", "fetch", "--branch", head]), jj(&["workspace", "add", "--name", name, "-r", &format!("{head}@origin"), path])]
}

/// True only for the folder the board recorded making: a real directory (no symlink) holding `.jj`,
/// directly under `home/git-work`, named `*-review`, with no `..`. `apply` also refuses registered parents.
pub fn removable(path: &Path, recorded: Option<&str>, home: &Path) -> bool {
    let is_dir = |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir());
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    recorded.is_some()
        && recorded == path.to_str()
        && path.is_absolute()
        && path.components().all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
        && path.parent() == Some(&home.join("git-work"))
        && name.ends_with("-review")
        && !name.starts_with('-')
        && is_dir(path)
        && is_dir(&path.join(".jj"))
        // A secondary workspace points at its repo with a file; a main repo has a directory there.
        && std::fs::symlink_metadata(path.join(".jj/repo")).is_ok_and(|m| m.is_file())
}

/// Deletes a finished review's workspace once its session has ended and jj in its parent repo confirms, snapshots and forgets it.
/// Anything doubtful leaves the folder and says why in the task's "workspace" event.
fn remove_workspace(store: &Store, id: i64, link: &str, run: &dyn Runner, home: &Path) -> Result<()> {
    let Some(recorded) = store.events(id)?.into_iter().rfind(|e| e.kind == "workspace").map(|e| e.detail) else { return Ok(()) };
    let path = Path::new(&recorded);
    let canon = |p: &Path| std::fs::canonicalize(p).ok();
    // A registered parent at or inside the folder means the folder is not just a workspace.
    let inside = |p: &String| Path::new(p).starts_with(path) || canon(Path::new(p)).zip(canon(path)).is_some_and(|(p, w)| p.starts_with(w));
    if !removable(path, Some(&recorded), home) || store.review_parents()?.iter().any(inside) {
        return Ok(());
    }
    let left = |why: String| store.replace_event(id, "workspace", &format!("left {recorded}: {why}"));
    // A merge mid-review can finish the task under a live session; don't pull its folder away.
    if store.current_session(id)?.is_some_and(|s| s.state != AgentState::Ended) {
        return left("session still running".into());
    }
    let Some(parent) = pr_ref(link).map(|(repo, _)| store.review_parent(repo)).transpose()?.flatten() else {
        return left("no parent repo recorded".into());
    };
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let jj = |a: &[&str]| run.run(&["jj", "-R", &parent].iter().chain(a).map(|s| s.to_string()).collect::<Vec<_>>());
    // `forget` succeeds even for an unknown name, so first have jj confirm this folder is that workspace.
    match jj(&["workspace", "root", "--name", name]) {
        Ok(root) if canon(Path::new(root.trim())).is_some_and(|r| Some(r) == canon(path)) => {}
        Ok(root) => return left(format!("jj puts {name} at {}", root.trim())),
        Err(e) => return left(format!("{e:#}")),
    }
    // Snapshot first, so edits not yet recorded survive in the parent repo.
    if let Err(e) = run.run(&["jj", "-R", &recorded, "status"].map(String::from)) {
        return left(format!("{e:#}"));
    }
    if let Err(e) = jj(&["workspace", "forget", name]) {
        return left(format!("{e:#}"));
    }
    if let Err(e) = std::fs::remove_dir_all(path) {
        return left(e.to_string());
    }
    Ok(())
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
/// A review that moves to Done or Dropped loses its workspace under `home/git-work`.
pub fn apply(store: &Store, poll: &Poll, run: &dyn Runner, home: &Path) -> Result<()> {
    let go = |id: i64, to: Stage| store.transition(id, to, usize::MAX, &|_, _| vec![]).map(|r| r.is_ok());
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
        let latest = store.events(t.id)?.into_iter().rfind(|e| e.kind == "review").map(|e| e.detail);
        if asked == Some(true) {
            // Re-requested after my review; a parked review I never submitted stays parked.
            if stage == Stage::Parked && pr.is_some_and(|p| p.reviewed_by.contains(&poll.me)) {
                go(t.id, Stage::Queue)?;
            }
            if latest.as_deref() != Some("requested") {
                store.replace_event(t.id, "review", "requested")?;
            }
        } else if let (Some(false), Some(pr)) = (asked, pr) {
            if latest.as_deref() == Some("requested") && !pr.reviewed_by.contains(&poll.me) {
                store.replace_event(t.id, "review", "no longer requested")?;
            }
        }
        let Some(pr) = pr else { continue };
        match pr.state {
            PrState::Merged | PrState::Closed => {
                if go(t.id, if pr.state == PrState::Merged { Stage::Done } else { Stage::Dropped })? {
                    remove_workspace(store, t.id, url, run, home)?;
                }
            }
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
    fn none() -> Jj {
        Jj { calls: Default::default(), root: String::new(), fail: false }
    }
    fn imported(req: bool) -> (Store, i64) {
        let s = Store::open_in_memory();
        let f = vec![found(U, "o/r")];
        apply(&s, &poll(req.then(|| f), None, vec![]), &none(), Path::new("/nonexistent")).unwrap();
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
        apply(&s, &poll(Some(vec![]), Some(vec![found(U, "o/r")]), vec![]), &none(), Path::new("/nonexistent")).unwrap();
        assert_eq!(ev(&s, s.review_task(U).unwrap().unwrap().id), "not requested");
    }

    #[test]
    fn same_pr_twice_makes_one_task() {
        let s = Store::open_in_memory();
        apply(&s, &poll(Some(vec![found(U, "o/r")]), Some(vec![found(U, "o/r")]), vec![]), &none(), Path::new("/nonexistent")).unwrap();
        assert_eq!(s.tasks().unwrap().len(), 1);
        assert_eq!(ev(&s, s.tasks().unwrap()[0].id), "requested");
    }

    #[test]
    fn malformed_items_are_skipped() {
        let s = Store::open_in_memory();
        let mut z = found(U, "o/r");
        z.number = 0;
        apply(&s, &poll(Some(vec![z, found("https://x/2", "")]), None, vec![]), &none(), Path::new("/nonexistent")).unwrap();
        assert!(s.tasks().unwrap().is_empty());
    }

    #[test]
    fn rerequest_unparks() {
        let (s, id) = imported(true);
        go(&s, id, Stage::Active);
        go(&s, id, Stage::Parked);
        s.replace_event(id, "review", "no longer requested").unwrap();
        apply(&s, &poll(Some(vec![found(U, "o/r")]), None, vec![(U, pr(PrState::Waiting, &["me"]))]), &none(), Path::new("/nonexistent")).unwrap();
        assert_eq!((stage(&s, id), ev(&s, id).as_str()), (Stage::Queue, "requested"));
    }

    #[test]
    fn still_requested_but_unreviewed_stays_parked() {
        let (s, id) = imported(true);
        go(&s, id, Stage::Active);
        go(&s, id, Stage::Parked);
        apply(&s, &poll(Some(vec![found(U, "o/r")]), None, vec![(U, pr(PrState::Waiting, &[]))]), &none(), Path::new("/nonexistent")).unwrap();
        assert_eq!((stage(&s, id), ev(&s, id).as_str()), (Stage::Parked, "requested"));
    }

    #[test]
    fn request_upgrades_a_not_requested_note_in_any_open_stage() {
        let s = Store::open_in_memory();
        apply(&s, &poll(Some(vec![]), Some(vec![found(U, "o/r")]), vec![]), &none(), Path::new("/nonexistent")).unwrap();
        let id = s.review_task(U).unwrap().unwrap().id;
        assert_eq!(ev(&s, id), "not requested");
        apply(&s, &poll(Some(vec![found(U, "o/r")]), None, vec![]), &none(), Path::new("/nonexistent")).unwrap();
        assert_eq!((stage(&s, id), ev(&s, id).as_str()), (Stage::Queue, "requested"));
    }

    #[test]
    fn dropped_request_is_noted_unless_reviewed() {
        let (s, id) = imported(true);
        let p = |by: &[&str]| poll(Some(vec![]), None, vec![(U, pr(PrState::Waiting, by))]);
        apply(&s, &p(&["me"]), &none(), Path::new("/nonexistent")).unwrap();
        assert_eq!(ev(&s, id), "requested");
        apply(&s, &p(&[]), &none(), Path::new("/nonexistent")).unwrap();
        assert_eq!(ev(&s, id), "no longer requested");
    }

    #[test]
    fn merged_is_done_and_closed_is_dropped() {
        for (st, want) in [(PrState::Merged, Stage::Done), (PrState::Closed, Stage::Dropped)] {
            let (s, id) = imported(true);
            apply(&s, &poll(None, None, vec![(U, pr(st, &[]))]), &none(), Path::new("/nonexistent")).unwrap();
            assert_eq!(stage(&s, id), want);
        }
    }

    #[test]
    fn finished_task_is_not_reimported_or_revived() {
        let (s, id) = imported(true);
        go(&s, id, Stage::Done);
        apply(&s, &poll(Some(vec![found(U, "o/r")]), None, vec![]), &none(), Path::new("/nonexistent")).unwrap();
        assert_eq!((s.tasks().unwrap().len(), stage(&s, id)), (1, Stage::Done));
    }

    #[test]
    fn reviewed_active_task_parks_once_session_ended() {
        let (s, id) = imported(true);
        go(&s, id, Stage::Active);
        let p = poll(None, None, vec![(U, pr(PrState::Waiting, &["me"]))]);
        s.link_session("s", id, "/w").unwrap();
        s.end_session("s").unwrap();
        apply(&s, &p, &none(), Path::new("/nonexistent")).unwrap();
        assert_eq!(stage(&s, id), Stage::Parked);
    }

    #[test]
    fn reviewed_active_task_with_no_session_parks() {
        let (s, id) = imported(true);
        go(&s, id, Stage::Active);
        apply(&s, &poll(None, None, vec![(U, pr(PrState::Waiting, &["me"]))]), &none(), Path::new("/nonexistent")).unwrap();
        assert_eq!(stage(&s, id), Stage::Parked);
    }

    #[test]
    fn submitting_a_review_keeps_a_live_session_in_reviewing() {
        let (s, id) = imported(true);
        go(&s, id, Stage::Active);
        s.link_session("s", id, "/w").unwrap();
        s.touch_session(crate::store::SessionUpdate { id: "s", cwd: "/w", state: AgentState::Waiting, zellij_session: None, zellij_pane: None }, false).unwrap();
        apply(&s, &poll(None, None, vec![(U, pr(PrState::Waiting, &["me"]))]), &none(), Path::new("/nonexistent")).unwrap();
        assert_eq!(stage(&s, id), Stage::Active);
    }

    #[test]
    fn workspace_cmds_fetch_then_add() {
        let path = workspace_path(Path::new("/h"), "delta", 812);
        assert_eq!(path, Path::new("/h/git-work/delta-812-review"));
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(
            workspace_cmds("/h/git-work/delta", "/h/git-work/delta-812-review", "delta-812-review", "feat/x"),
            vec![
                s(&["jj", "-R", "/h/git-work/delta", "git", "fetch", "--branch", "feat/x"]),
                s(&["jj", "-R", "/h/git-work/delta", "workspace", "add", "--name", "delta-812-review", "-r", "feat/x@origin", "/h/git-work/delta-812-review"]),
            ]
        );
    }

    /// A fake home with git-work/{delta (parent), delta-812-review (workspace), main-review (a main repo), plain, plain-review, link-review -> workspace} and other-review outside.
    fn fake_home(tag: &str) -> PathBuf {
        let home = std::env::temp_dir().join(format!("wyrcan-ws-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&home).ok();
        let gw = home.join("git-work");
        for d in ["delta/.jj/repo", "delta-812-review/.jj", "main-review/.jj/repo", "plain/.jj", "plain-review", "../other-review/.jj"] {
            std::fs::create_dir_all(gw.join(d)).unwrap();
        }
        for f in ["delta-812-review/.jj/repo", "plain/.jj/repo", "../other-review/.jj/repo"] {
            std::fs::write(gw.join(f), "../delta/.jj/repo").unwrap();
        }
        std::os::unix::fs::symlink(gw.join("delta-812-review"), gw.join("link-review")).unwrap();
        home
    }

    #[test]
    fn removable_refuses_everything_but_the_recorded_review_folder() {
        let home = fake_home("rm");
        let gw = home.join("git-work");
        let ok = |p: &Path| removable(p, p.to_str(), &home);
        assert!(ok(&gw.join("delta-812-review")));
        assert!(!ok(&gw.join("delta")), "the parent repo");
        assert!(!ok(&gw.join("link-review")), "a symlink");
        assert!(!ok(&home.join("other-review")), "outside git-work");
        assert!(!ok(&gw.join("plain")), "no -review suffix");
        assert!(!ok(&gw.join("plain-review")), "not a jj workspace");
        assert!(!ok(&gw.join("main-review")), "a main repo, not a secondary workspace");
        assert!(!ok(&gw.join("delta/../delta-812-review")), "a .. component");
        assert!(!ok(&gw.join("missing-review")), "not there");
        assert!(!ok(&gw), "git-work itself");
        assert!(!removable(&gw.join("delta-812-review"), None, &home), "nothing recorded");
        assert!(!removable(&gw.join("delta-812-review"), Some("/elsewhere-review"), &home), "recorded something else");
        assert!(!removable(Path::new("git-work/delta-812-review"), Some("git-work/delta-812-review"), Path::new("")), "empty home");
        std::fs::remove_dir_all(home).ok();
    }

    /// Records commands; `workspace root` answers `root`, and `forget` fails when `fail` is set.
    struct Jj {
        calls: std::cell::RefCell<Vec<String>>,
        root: String,
        fail: bool,
    }
    impl Runner for Jj {
        fn run(&self, cmd: &[String]) -> Result<String> {
            let line = cmd.join(" ");
            self.calls.borrow_mut().push(line.clone());
            if line.contains("workspace root") {
                return Ok(format!("{}\n", self.root));
            }
            if self.fail && line.contains("forget") {
                anyhow::bail!("boom");
            }
            Ok(String::new())
        }
    }

    /// An imported review task for `U` (repo `r`) whose workspace was recorded at `ws`, with parent `parent`.
    fn with_workspace(ws: &Path, parent: &Path) -> (Store, i64) {
        let (s, id) = imported(true);
        s.set_review_parent("r", parent.to_str().unwrap()).unwrap();
        s.add_event(id, "workspace", ws.to_str().unwrap()).unwrap();
        (s, id)
    }

    #[test]
    fn merged_review_forgets_and_deletes_its_workspace() {
        let home = fake_home("merged");
        let (ws, parent) = (home.join("git-work/delta-812-review"), home.join("git-work/delta"));
        let (s, id) = with_workspace(&ws, &parent);
        let jj = Jj { calls: Default::default(), root: ws.to_str().unwrap().into(), fail: false };
        apply(&s, &poll(None, None, vec![(U, pr(PrState::Merged, &[]))]), &jj, &home).unwrap();
        assert_eq!(stage(&s, id), Stage::Done);
        let p = parent.to_str().unwrap();
        let w = ws.to_str().unwrap();
        assert_eq!(
            *jj.calls.borrow(),
            [format!("jj -R {p} workspace root --name delta-812-review"), format!("jj -R {w} status"), format!("jj -R {p} workspace forget delta-812-review")]
        );
        assert!(!ws.exists() && parent.exists());
        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn workspace_is_kept_when_forget_fails_or_it_is_not_the_parents() {
        let home = fake_home("kept");
        let (ws, parent) = (home.join("git-work/delta-812-review"), home.join("git-work/delta"));
        // forget fails
        let (s, id) = with_workspace(&ws, &parent);
        let jj = Jj { calls: Default::default(), root: ws.to_str().unwrap().into(), fail: true };
        apply(&s, &poll(None, None, vec![(U, pr(PrState::Closed, &[]))]), &jj, &home).unwrap();
        assert_eq!(stage(&s, id), Stage::Dropped);
        assert!(ws.exists());
        let e = s.events(id).unwrap().into_iter().rfind(|e| e.kind == "workspace").unwrap().detail;
        assert_eq!(e, format!("left {}: boom", ws.display()));
        // jj says the workspace lives elsewhere: no forget, no delete
        let (s, _) = with_workspace(&ws, &parent);
        let jj = Jj { calls: Default::default(), root: "/elsewhere".into(), fail: false };
        apply(&s, &poll(None, None, vec![(U, pr(PrState::Merged, &[]))]), &jj, &home).unwrap();
        assert!(ws.exists() && !jj.calls.borrow().iter().any(|c| c.contains("forget")));
        // the recorded folder is a registered parent: nothing runs
        let (s, _) = with_workspace(&ws, &parent);
        s.set_review_parent("other", ws.to_str().unwrap()).unwrap();
        let jj = Jj { calls: Default::default(), root: ws.to_str().unwrap().into(), fail: false };
        apply(&s, &poll(None, None, vec![(U, pr(PrState::Merged, &[]))]), &jj, &home).unwrap();
        assert!(ws.exists() && jj.calls.borrow().is_empty());
        // a registered parent inside the folder: nothing runs
        let (s, _) = with_workspace(&ws, &parent);
        s.set_review_parent("nested", ws.join("inner").to_str().unwrap()).unwrap();
        let jj = Jj { calls: Default::default(), root: ws.to_str().unwrap().into(), fail: false };
        apply(&s, &poll(None, None, vec![(U, pr(PrState::Merged, &[]))]), &jj, &home).unwrap();
        assert!(ws.exists() && jj.calls.borrow().is_empty());
        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn workspace_is_kept_while_its_session_runs() {
        let home = fake_home("live");
        let (ws, parent) = (home.join("git-work/delta-812-review"), home.join("git-work/delta"));
        let (s, id) = with_workspace(&ws, &parent);
        go(&s, id, Stage::Active);
        s.link_session("s", id, ws.to_str().unwrap()).unwrap();
        let jj = Jj { calls: Default::default(), root: ws.to_str().unwrap().into(), fail: false };
        apply(&s, &poll(None, None, vec![(U, pr(PrState::Merged, &[]))]), &jj, &home).unwrap();
        assert_eq!(stage(&s, id), Stage::Done);
        assert!(ws.exists() && jj.calls.borrow().is_empty(), "{:?}", jj.calls.borrow());
        let e = s.events(id).unwrap().into_iter().rfind(|e| e.kind == "workspace").unwrap().detail;
        assert_eq!(e, format!("left {}: session still running", ws.display()));
        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn gh_failure_changes_nothing() {
        let (s, id) = imported(true);
        go(&s, id, Stage::Active);
        go(&s, id, Stage::Parked);
        apply(&s, &poll(None, None, vec![]), &none(), Path::new("/nonexistent")).unwrap();
        assert_eq!((stage(&s, id), ev(&s, id).as_str()), (Stage::Parked, "requested"));
    }
}
