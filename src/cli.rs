//! Subcommands the skills call. stdout is JSON where noted; refusals go to stderr with exit 1.
use crate::config::Config;
use crate::gate::{self, PrVerifier};
use crate::model::{Refusal, Stage, Task};
use crate::store::{NewTask, Store};
use anyhow::{bail, Result};
use serde_json::{json, Value};

const USAGE: &str = "usage: claude-board add TITLE [--desc D] [--dir DIR] [--label L].. [--kind K] [--link URL] | next | list [--stage S].. | claim SESSION TASK_ID CWD | register-pen SESSION CWD | status SESSION | gate SESSION [--pr ROOT=URL].. [--no-pr ROOT=REASON].. [--explainer FILE] [--summary TEXT] [--bookmark ROOT=NAME].. | edit TASK_ID [--title T] [--desc D] [--dir DIR] [--label L].. [--kind K] [--link URL] | shelve TASK_ID [--bookmark ROOT=NAME].. | requeue TASK_ID | drop TASK_ID | closed [--days N] | export | sync";

pub fn run(args: &[String], cfg: &Config) -> Result<i32> {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    if !matches!(a.first(), Some(&("add" | "next" | "list" | "claim" | "register-pen" | "status" | "gate" | "edit" | "shelve" | "requeue" | "drop" | "closed" | "export" | "sync"))) {
        eprintln!("{USAGE}");
        return Ok(2);
    }
    std::fs::create_dir_all(&cfg.state_dir)?;
    let store = Store::open(&cfg.db_path())?;
    if a[0] == "sync" {
        return sync(&store, cfg);
    }
    let (code, text) = match exec(&store, &a, cfg, &crate::pr::exists) {
        Ok(v) => v,
        Err(e) if e.downcast_ref::<Usage>().is_some() => (2, format!("{e}\n{USAGE}")),
        Err(e) => return Err(e),
    };
    if code == 0 {
        println!("{text}");
    } else {
        eprintln!("{text}");
    }
    // Here, not in exec, so tests stay offline: the board shows PR state now instead of after the next poll.
    if code == 0 && a[0] == "gate" {
        if let Some(t) = gate_task(&store, &a)? {
            crate::pr::refresh(&store, t.id);
        }
    }
    Ok(code)
}

fn sync(store: &Store, cfg: &Config) -> Result<i32> {
    if !cfg.todoist {
        eprintln!("Todoist sync is off (set todoist = true in ~/.config/claude-board/config.toml)");
        return Ok(1);
    }
    let (client, sections) = crate::todoist::connect(cfg)?;
    let r = crate::sync::sync_once(store, &client, &sections)?;
    println!("imported {}, pushed {}, missing remotely {}", r.imported, r.pushed, r.missing);
    Ok(0)
}

#[derive(Debug)]
struct Usage(String);
impl std::fmt::Display for Usage {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Usage {}
fn usage<T>(m: &str) -> Result<T> {
    Err(Usage(m.into()).into())
}

/// Splits `args` into positionals and `--flag value` pairs; a flag's value may start with `-`.
fn parse<'a>(args: &[&'a str], flags: &[&str]) -> Result<(Vec<&'a str>, Vec<(&'a str, &'a str)>)> {
    let (mut pos, mut fl) = (vec![], vec![]);
    let mut it = args.iter().copied();
    while let Some(x) = it.next() {
        if flags.contains(&x) {
            match it.next() {
                Some(v) => fl.push((x, v)),
                None => return usage(&format!("{x} needs a value")),
            }
        } else {
            pos.push(x);
        }
    }
    Ok((pos, fl))
}

fn task_json(t: &Task) -> Value {
    json!({"id": t.id, "title": t.title, "description": t.description, "dir": t.dir, "labels": t.labels, "kind": t.kind, "link": t.link})
}

/// `task_json` plus the bookmarks the last gate recorded.
fn task_full_json(store: &Store, t: &Task) -> Result<Value> {
    let mut v = task_json(t);
    let marks: Vec<Value> = store.bookmarks(t.id)?.into_iter().map(|(root, name)| json!({"root": root, "name": name})).collect();
    v["bookmarks"] = Value::Array(marks);
    Ok(v)
}

/// `was`: the task's kind now ("default" for a new task). Reviews are only imported, and stay reviews.
fn check_kind_link(kind: &str, was: &str, link: Option<&str>) -> Result<()> {
    if !crate::model::KINDS.contains(&kind) {
        return usage(&format!("--kind is one of {}", crate::model::KINDS.join(", ")));
    }
    if (kind == "review") != (was == "review") {
        return usage("review tasks are only imported from GitHub; --kind is default or experiment");
    }
    match link {
        Some(l) if !l.is_empty() && !l.starts_with("https://") => usage(&format!("--link wants an https:// URL, got {l}")),
        _ => Ok(()),
    }
}

fn refused(r: Refusal) -> Result<(i32, String)> {
    Ok((1, r.to_string()))
}

fn session_task(store: &Store, session: &str) -> Result<Option<Task>> {
    match store.session(session)?.and_then(|s| s.task_id) {
        Some(id) => store.task(id),
        None => Ok(None),
    }
}

const GATE_FLAGS: &[&str] = &["--pr", "--no-pr", "--explainer", "--summary", "--bookmark"];

/// The task a `gate` command line is about.
fn gate_task(store: &Store, a: &[&str]) -> Result<Option<Task>> {
    match parse(&a[1..], GATE_FLAGS)?.0[..] {
        [session] => session_task(store, session),
        _ => Ok(None),
    }
}

fn exec(store: &Store, a: &[&str], cfg: &Config, verify: &PrVerifier) -> Result<(i32, String)> {
    let ok = |s: String| Ok((0, s));
    let rest = &a[1..];
    match a[0] {
        "add" => {
            let (pos, fl) = parse(rest, &["--desc", "--dir", "--label", "--kind", "--link"])?;
            let [title] = pos[..] else { return usage("add takes one TITLE") };
            if title.trim().is_empty() {
                return usage("TITLE can't be blank");
            }
            let val = |k| fl.iter().rev().find(|f| f.0 == k).map(|f| f.1);
            let labels: Vec<String> = fl.iter().filter(|f| f.0 == "--label").map(|f| f.1.to_string()).collect();
            let (kind, link) = (val("--kind").unwrap_or("default"), val("--link").filter(|l| !l.is_empty()));
            check_kind_link(kind, "default", link)?;
            let id = store.add_task(NewTask { title, description: val("--desc").unwrap_or(""), dir: val("--dir"), labels: &labels, todoist_id: None })?;
            if kind != "default" || link.is_some() {
                store.set_kind(id, kind, link)?;
            }
            ok(id.to_string())
        }
        "next" => match store.tasks()?.into_iter().find(|t| t.stage == Stage::Queue && t.kind != "review") {
            Some(t) => ok(task_json(&t).to_string()),
            None => Ok((1, "Queue is empty".into())),
        },
        "list" => {
            let (pos, fl) = parse(rest, &["--stage"])?;
            if !pos.is_empty() {
                return usage("list takes only --stage S");
            }
            let mut stages = vec![];
            for f in &fl {
                let Some(st) = Stage::from_str(f.1) else { return usage("--stage is one of queue, active, review, shelved, parked, done, dropped") };
                stages.push(st);
            }
            // Closed tasks pile up, so they show only when asked for.
            let want = |st: Stage| if stages.is_empty() { !matches!(st, Stage::Done | Stage::Dropped) } else { stages.contains(&st) };
            let rows: Vec<Value> = store.tasks()?.iter().filter(|t| want(t.stage)).map(|t| json!({"id": t.id, "title": t.title, "stage": t.stage.as_str(), "dir": t.dir, "labels": t.labels, "kind": t.kind, "link": t.link})).collect();
            ok(Value::Array(rows).to_string())
        }
        "claim" => {
            let [session, id, cwd] = rest[..] else { return usage("claim takes SESSION TASK_ID CWD") };
            let Ok(id) = id.parse::<i64>() else { return usage("TASK_ID must be a number") };
            let Some(t) = store.task(id)? else { return refused(Refusal::NoSuchTask) };
            match t.stage {
                Stage::Active => {}
                Stage::Queue | Stage::Parked | Stage::Shelved => {
                    if let Err(r) = store.transition(id, Stage::Active, cfg.max_active, &|_, _| vec![])? {
                        return refused(r);
                    }
                }
                from => return refused(Refusal::NotAllowed { from, to: Stage::Active }),
            }
            store.link_session(session, id, cwd)?;
            ok(task_full_json(store, &store.task(id)?.expect("just read"))?.to_string())
        }
        "register-pen" => {
            let [session, cwd] = rest[..] else { return usage("register-pen takes SESSION CWD") };
            store.register_pen(session, cwd)?;
            ok("registered".into())
        }
        "status" => {
            let [session] = rest[..] else { return usage("status takes SESSION") };
            let Some(t) = session_task(store, session)? else { return Ok((1, "This session has no task.".into())) };
            let missing = gate::evaluate(store, &t, &cfg.gate_checks(&t.kind), verify);
            let prs: Vec<Value> = store.prs(t.id)?.iter().map(|p| json!({"root": p.root, "url": p.url, "waiver": p.waiver, "state": p.state.as_str()})).collect();
            ok(json!({"task": task_json(&t), "repos": store.task_repos(t.id)?, "prs": prs, "missing": missing}).to_string())
        }
        "gate" => {
            let (pos, fl) = parse(rest, GATE_FLAGS)?;
            let [session] = pos[..] else { return usage("gate takes one SESSION") };
            let Some(t) = session_task(store, session)? else { return Ok((1, "This session has no task.".into())) };
            for (k, v) in &fl {
                match *k {
                    "--pr" | "--no-pr" => {
                        let Some((root, val)) = v.split_once('=') else { return usage(&format!("{k} wants ROOT=VALUE")) };
                        // The board hands a PR value to xdg-open.
                        if *k == "--pr" && !val.starts_with("https://") {
                            return usage(&format!("--pr wants an https:// URL, got {val}"));
                        }
                        let (url, waiver) = if *k == "--pr" { (Some(val), None) } else { (None, Some(val)) };
                        store.set_pr(t.id, root, url, waiver)?;
                    }
                    "--bookmark" => {
                        if !v.split_once('=').is_some_and(|(r, n)| !r.is_empty() && !n.is_empty()) {
                            return usage("--bookmark wants ROOT=NAME");
                        }
                        store.add_event(t.id, "bookmark", v)?;
                    }
                    "--explainer" => {
                        if std::fs::metadata(v).is_ok_and(|m| m.len() == 0) {
                            return Ok((1, "explainer is empty".into()));
                        }
                        let dest = cfg.explainer_dir().join(format!("{}.html", t.id));
                        std::fs::create_dir_all(cfg.explainer_dir())?;
                        if let Err(e) = std::fs::copy(v, &dest) {
                            bail!("can't copy explainer {v}: {e}");
                        }
                        store.set_explainer(t.id, &dest.to_string_lossy())?;
                    }
                    _ => store.add_event(t.id, "summary", v)?,
                }
            }
            let checks = cfg.gate_checks(&t.kind);
            match gate::to_review(&store, t.id, cfg.max_active, &checks, verify)? {
                Ok(()) => ok(if gate::target(&t.kind) == Stage::Shelved { "Shelved." } else { "In review." }.into()),
                Err(Refusal::Gate(m)) => Ok((1, format!("Not ready for review. Missing:\n- {}", m.join("\n- ")))),
                Err(r) => refused(r),
            }
        }
        "edit" => {
            let (pos, fl) = parse(rest, &["--title", "--desc", "--dir", "--label", "--kind", "--link"])?;
            let [id] = pos[..] else { return usage("edit takes one TASK_ID") };
            let Ok(id) = id.parse::<i64>() else { return usage("TASK_ID must be a number") };
            let Some(t) = store.task(id)? else { return refused(Refusal::NoSuchTask) };
            let val = |k| fl.iter().rev().find(|f| f.0 == k).map(|f| f.1);
            let title = val("--title").unwrap_or(&t.title);
            if title.trim().is_empty() {
                return usage("TITLE can't be blank");
            }
            // Any --label replaces the whole set; --dir "" clears the dir.
            let labels: Vec<String> = fl.iter().filter(|f| f.0 == "--label").map(|f| f.1.to_string()).collect();
            let labels = if labels.is_empty() { t.labels.clone() } else { labels };
            let dir = match val("--dir") {
                Some(d) => Some(d).filter(|d| !d.is_empty()),
                None => t.dir.as_deref(),
            };
            // Each of --kind and --link keeps the other's current value; --link "" clears it.
            let kind = val("--kind").unwrap_or(&t.kind);
            let link = match val("--link") {
                Some(l) => Some(l).filter(|l| !l.is_empty()),
                None => t.link.as_deref(),
            };
            check_kind_link(kind, &t.kind, link)?;
            store.update_task(id, title, val("--desc").unwrap_or(&t.description), dir, &labels)?;
            store.set_kind(id, kind, link)?;
            ok(task_full_json(store, &store.task(id)?.expect("just read"))?.to_string())
        }
        "shelve" => {
            let (pos, fl) = parse(rest, &["--bookmark"])?;
            let [id] = pos[..] else { return usage("shelve takes one TASK_ID") };
            let Ok(id) = id.parse::<i64>() else { return usage("TASK_ID must be a number") };
            if fl.iter().any(|f| !f.1.split_once('=').is_some_and(|(r, n)| !r.is_empty() && !n.is_empty())) {
                return usage("--bookmark wants ROOT=NAME");
            }
            let Some(t) = store.task(id)? else { return refused(Refusal::NoSuchTask) };
            if t.kind == "review" {
                return Ok((1, "A review can't be shelved.".into()));
            }
            // Gated as the experiment it becomes; the kind only changes once the move succeeds.
            let checks = cfg.gate_checks("experiment");
            match store.transition(id, Stage::Shelved, cfg.max_active, &|s, t| gate::evaluate(s, t, &checks, verify))? {
                Ok(()) => {}
                Err(Refusal::Gate(m)) => return Ok((1, format!("Can't shelve yet. Missing:\n- {}", m.join("\n- ")))),
                Err(r) => return refused(r),
            }
            store.set_kind(id, "experiment", t.link.as_deref())?;
            for f in &fl {
                store.add_event(id, "bookmark", f.1)?;
            }
            ok("Shelved.".into())
        }
        "requeue" | "drop" => {
            let [id] = rest[..] else { return usage(&format!("{} takes TASK_ID", a[0])) };
            let Ok(id) = id.parse::<i64>() else { return usage("TASK_ID must be a number") };
            let (to, msg) = if a[0] == "drop" { (Stage::Dropped, "Dropped.") } else { (Stage::Queue, "Back in the queue.") };
            match store.transition(id, to, cfg.max_active, &|_, _| vec![])? {
                Ok(()) => ok(msg.into()),
                Err(r) => refused(r),
            }
        }
        "closed" => {
            let (pos, fl) = parse(rest, &["--days"])?;
            let days = match (pos.is_empty(), fl.last()) {
                (false, _) => return usage("closed takes only --days N"),
                (_, Some(f)) => match f.1.parse::<i64>() {
                    Ok(d) if d >= 0 => d,
                    _ => return usage("--days must be a number of days, 0 for everything"),
                },
                _ => 7,
            };
            let now = crate::store::now();
            let rows = crate::closed::closed(store, (days > 0).then(|| now - days * 86400))?;
            ok(crate::closed::report(store, &rows, days, now)?.trim_end().to_string())
        }
        _ => ok(store.export_json()?.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(max: usize) -> (Config, std::path::PathBuf) {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("cb-cli-{}-{n}", std::process::id()));
        let mut c = Config::from_env();
        c.state_dir = dir.clone();
        c.max_active = max;
        c.file = String::new();
        (c, dir)
    }
    fn go(s: &Store, c: &Config, a: &[&str], ok_pr: bool) -> (i32, String) {
        exec(s, a, c, &move |_| ok_pr).unwrap()
    }
    fn stage(s: &Store, id: i64) -> Stage {
        s.task(id).unwrap().unwrap().stage
    }
    /// A task claimed by session "s" with one edited repo.
    fn claimed(s: &Store, c: &Config) -> i64 {
        let id: i64 = go(s, c, &["add", "t"], true).1.parse().unwrap();
        go(s, c, &["claim", "s", &id.to_string(), "/w"], true);
        s.mark_edited("s", "/r/app", "git main").unwrap();
        id
    }

    #[test]
    fn add_accepts_dash_values() {
        let (c, _) = cfg(3);
        let s = Store::open_in_memory();
        let id: i64 = go(&s, &c, &["add", "t", "--desc", "- a list", "--label", "x", "--label", "-y", "--dir", "/d"], true).1.parse().unwrap();
        let t = s.task(id).unwrap().unwrap();
        assert_eq!((t.description.as_str(), t.dir.as_deref(), t.labels), ("- a list", Some("/d"), vec!["x".to_string(), "-y".to_string()]));
    }

    #[test]
    fn add_and_edit_set_kind_and_link() {
        let (c, _) = cfg(3);
        let s = Store::open_in_memory();
        let id = go(&s, &c, &["add", "t", "--kind", "experiment", "--link", "https://github.com/o/r/issues/4"], true).1;
        let j: Value = serde_json::from_str(&go(&s, &c, &["edit", &id], true).1).unwrap();
        assert_eq!((j["kind"].as_str(), j["link"].as_str()), (Some("experiment"), Some("https://github.com/o/r/issues/4")));
        let j: Value = serde_json::from_str(&go(&s, &c, &["edit", &id, "--kind", "default"], true).1).unwrap();
        assert_eq!((j["kind"].as_str(), j["link"].as_str()), (Some("default"), Some("https://github.com/o/r/issues/4")));
        let j: Value = serde_json::from_str(&go(&s, &c, &["edit", &id, "--link", ""], true).1).unwrap();
        assert_eq!((j["kind"].as_str(), j["link"].is_null()), (Some("default"), true));
        assert!(exec(&s, &["edit", &id, "--kind", "nope"], &c, &|_| true).unwrap_err().downcast_ref::<Usage>().is_some());
        assert!(exec(&s, &["add", "u", "--link", "http://x"], &c, &|_| true).unwrap_err().downcast_ref::<Usage>().is_some());
    }

    #[test]
    fn reviews_are_only_imported() {
        let (c, _) = cfg(3);
        let s = Store::open_in_memory();
        let usage = |a: &[&str]| exec(&s, a, &c, &|_| true).unwrap_err().downcast_ref::<Usage>().is_some();
        assert!(usage(&["add", "r", "--kind", "review"]));
        assert!(s.tasks().unwrap().is_empty());
        let d = go(&s, &c, &["add", "d"], true).1;
        assert!(usage(&["edit", &d, "--kind", "review"]));
        let r = s.add_task(NewTask { title: "r", description: "", dir: None, labels: &[], todoist_id: None }).unwrap();
        s.set_kind(r, "review", Some("https://github.com/o/r/pull/1")).unwrap();
        assert!(usage(&["edit", &r.to_string(), "--kind", "default"]));
        // Editing a review's other fields keeps it a review.
        let j: Value = serde_json::from_str(&go(&s, &c, &["edit", &r.to_string(), "--desc", "x"], true).1).unwrap();
        assert_eq!(j["kind"].as_str(), Some("review"));
    }

    #[test]
    fn next_skips_reviews_and_claim_resumes_shelved() {
        let (c, _) = cfg(3);
        let s = Store::open_in_memory();
        let r = s.add_task(NewTask { title: "r", description: "", dir: None, labels: &[], todoist_id: None }).unwrap();
        s.set_kind(r, "review", Some("https://github.com/o/r/pull/1")).unwrap();
        let d = go(&s, &c, &["add", "d"], true).1;
        let j: Value = serde_json::from_str(&go(&s, &c, &["next"], true).1).unwrap();
        assert_eq!(j["id"].to_string(), d);
        let e = go(&s, &c, &["add", "e", "--kind", "experiment"], true).1;
        go(&s, &c, &["claim", "s", &e, "/w"], true);
        s.mark_edited("s", "/r/app", "git main").unwrap();
        let ex = std::env::temp_dir().join(format!("cb-ex-{}.html", std::process::id()));
        std::fs::write(&ex, "<p>x</p>").unwrap();
        let g = go(&s, &c, &["gate", "s", "--pr", "/r/app=https://x/1", "--explainer", ex.to_str().unwrap(), "--bookmark", "/r/app=exp/1-try-sled"], true);
        assert_eq!(g, (0, "Shelved.".into()));
        assert_eq!(stage(&s, e.parse().unwrap()), Stage::Shelved);
        let j: Value = serde_json::from_str(&go(&s, &c, &["claim", "s2", &e, "/w"], true).1).unwrap();
        assert_eq!(stage(&s, e.parse().unwrap()), Stage::Active);
        assert_eq!(j["kind"], "default");
        assert_eq!(j["bookmarks"], json!([{"root": "/r/app", "name": "exp/1-try-sled"}]));
    }

    #[test]
    fn next_on_empty_queue_exits_1() {
        let (c, _) = cfg(3);
        assert_eq!(go(&Store::open_in_memory(), &c, &["next"], true), (1, "Queue is empty".into()));
    }

    #[test]
    fn claim_respects_the_cap() {
        let (c, _) = cfg(1);
        let s = Store::open_in_memory();
        claimed(&s, &c);
        let b = go(&s, &c, &["add", "b"], true).1;
        let (code, msg) = go(&s, &c, &["claim", "s2", &b, "/w"], true);
        assert_eq!((code, msg), (1, Refusal::CapFull { max: 1 }.to_string()));
        assert!(s.session("s2").unwrap().is_none());
    }

    #[test]
    fn claim_links_and_activates() {
        let (c, _) = cfg(3);
        let s = Store::open_in_memory();
        let id = claimed(&s, &c);
        assert_eq!(stage(&s, id), Stage::Active);
        assert_eq!(s.session("s").unwrap().unwrap().task_id, Some(id));
        // Claiming an Active task only links.
        assert_eq!(go(&s, &c, &["claim", "s9", &id.to_string(), "/w"], true).0, 0);
        assert_eq!(s.session("s9").unwrap().unwrap().task_id, Some(id));
        let done = go(&s, &c, &["add", "d"], true).1.parse().unwrap();
        s.transition(done, Stage::Dropped, 3, &|_, _| vec![]).unwrap().unwrap();
        assert_eq!(go(&s, &c, &["claim", "s3", &format!("{done}"), "/w"], true).0, 1);
    }

    #[test]
    fn claim_of_parked_task_resumes_it() {
        let (c, _) = cfg(3);
        let s = Store::open_in_memory();
        let id = claimed(&s, &c);
        s.transition(id, Stage::Parked, 3, &|_, _| vec![]).unwrap().unwrap();
        assert_eq!(go(&s, &c, &["claim", "s2", &id.to_string(), "/w"], true).0, 0);
        assert_eq!(stage(&s, id), Stage::Active);
    }

    #[test]
    fn gate_moves_to_review_and_copies_explainer() {
        let (c, dir) = cfg(3);
        let s = Store::open_in_memory();
        let id = claimed(&s, &c);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("e.html");
        std::fs::write(&f, "<p>x</p>").unwrap();
        let r = go(&s, &c, &["gate", "s", "--pr", "/r/app=https://gh/pr/1", "--explainer", f.to_str().unwrap(), "--summary", "- did it"], true);
        assert_eq!(r, (0, "In review.".into()));
        assert_eq!(stage(&s, id), Stage::Review);
        assert_eq!(std::fs::read_to_string(dir.join(format!("explainers/{id}.html"))).unwrap(), "<p>x</p>");
        assert!(s.events(id).unwrap().iter().any(|e| e.kind == "summary" && e.detail == "- did it"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn gate_lists_what_is_missing() {
        let (c, _) = cfg(3);
        let s = Store::open_in_memory();
        let id = claimed(&s, &c);
        let (code, msg) = go(&s, &c, &["gate", "s"], true);
        assert_eq!(code, 1);
        assert!(msg.contains("no PR for app") && msg.contains("no explainer"), "{msg}");
        assert_eq!(stage(&s, id), Stage::Active);
        // An unverifiable PR is refused too.
        let (code, msg) = go(&s, &c, &["gate", "s", "--pr", "/r/app=https://x/u"], false);
        assert!(code == 1 && msg.contains("couldn't verify PR https://x/u"), "{msg}");
    }

    #[test]
    fn gate_with_waiver_enters_review() {
        let (mut c, dir) = cfg(3);
        c.file = "gate_default = \"pr\"".into();
        let s = Store::open_in_memory();
        let id = claimed(&s, &c);
        assert_eq!(go(&s, &c, &["gate", "s", "--no-pr", "/r/app=docs only"], false).0, 0);
        assert_eq!(stage(&s, id), Stage::Review);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn gate_without_repos_needs_a_pr_or_reason_and_a_real_explainer() {
        let (c, dir) = cfg(3);
        let s = Store::open_in_memory();
        let id: i64 = go(&s, &c, &["add", "t"], true).1.parse().unwrap();
        go(&s, &c, &["claim", "s", &id.to_string(), "/w"], true);
        std::fs::create_dir_all(&dir).unwrap();
        let (empty, full) = (dir.join("empty.html"), dir.join("e.html"));
        std::fs::write(&empty, "").unwrap();
        std::fs::write(&full, "<p>x</p>").unwrap();
        assert_eq!(go(&s, &c, &["gate", "s", "--explainer", empty.to_str().unwrap()], true), (1, "explainer is empty".into()));
        let (code, msg) = go(&s, &c, &["gate", "s", "--explainer", full.to_str().unwrap()], true);
        assert!(code == 1 && msg.contains("no PR or reason recorded"), "{msg}");
        assert_eq!(stage(&s, id), Stage::Active);
        assert_eq!(go(&s, &c, &["gate", "s", "--no-pr", "/w=dotfiles only"], true), (0, "In review.".into()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn gate_task_finds_the_sessions_task() {
        let (c, _) = cfg(3);
        let s = Store::open_in_memory();
        let id = claimed(&s, &c);
        let t = |a: &[&str]| gate_task(&s, a).unwrap().map(|t| t.id);
        assert_eq!(t(&["gate", "s", "--summary", "x"]), Some(id));
        assert_eq!(t(&["gate", "--no-pr", "/r=-s", "s"]), Some(id));
        assert_eq!(t(&["gate", "nobody"]), None);
    }

    #[test]
    fn gate_refuses_a_pr_that_is_not_https() {
        let (c, _) = cfg(3);
        let s = Store::open_in_memory();
        claimed(&s, &c);
        let e = exec(&s, &["gate", "s", "--pr", "/r/app=--help"], &c, &|_| true).unwrap_err();
        assert!(e.downcast_ref::<Usage>().is_some(), "{e}");
        assert!(s.prs(1).unwrap().is_empty());
    }

    #[test]
    fn add_refuses_a_blank_title() {
        let (c, _) = cfg(3);
        let s = Store::open_in_memory();
        for t in ["", "  "] {
            assert!(exec(&s, &["add", t], &c, &|_| true).unwrap_err().downcast_ref::<Usage>().is_some());
        }
        assert!(s.tasks().unwrap().is_empty());
    }

    #[test]
    fn status_reports_missing() {
        let (c, _) = cfg(3);
        let s = Store::open_in_memory();
        claimed(&s, &c);
        let v: Value = serde_json::from_str(&go(&s, &c, &["status", "s"], true).1).unwrap();
        assert_eq!(v["repos"], json!(["/r/app"]));
        assert_eq!(v["missing"], json!(["no PR for app", "no explainer"]));
        assert_eq!(go(&s, &c, &["status", "nobody"], true).0, 1);
    }

    #[test]
    fn closed_prints_markdown_for_done_and_dropped() {
        use crate::closed::{date, tests::close};
        let (c, _) = cfg(3);
        let s = Store::open_in_memory();
        assert_eq!(go(&s, &c, &["closed"], true), (0, format!("# Closed work: {} to {}\n\nNothing closed in this period.", date(crate::store::now() - 7 * 86400), date(crate::store::now()))));
        let now = crate::store::now();
        let a = close(&s, "Ship\nthe thing", Stage::Done, Some(now - 3600 - 600), now - 600);
        crate::app::tests::session(&s, "x", Some(a), crate::model::AgentState::Ended);
        s.mark_edited("x", "/r/app", "git main").unwrap();
        s.set_pr(a, "/r/app", Some("https://github.com/acme/app/pull/3"), None).unwrap();
        s.set_pr(a, "/r/lib", None, Some("docs only")).unwrap();
        s.set_explainer(a, "/e/1.html").unwrap();
        s.add_event(a, "summary", "Did it.").unwrap();
        close(&s, "Old win", Stage::Done, None, now - 30 * 86400);
        close(&s, "Nope", Stage::Dropped, None, now - 86400);
        let (code, md) = go(&s, &c, &["closed"], true);
        let d = |t| date(t);
        let want = format!(
            "# Closed work: {} to {}\n\n1 done · 1 dropped\n\n## Done\n\n### Ship the thing\n- Closed: {} (took 1h 0m)\n- Repos: app\n- PR: https://github.com/acme/app/pull/3\n- No PR: docs only\n- Explainer: /e/1.html\n\nDid it.\n\n## Dropped\n\n### Nope\n- Dropped: {} (took —)",
            d(now - 7 * 86400), d(now), d(now - 600), d(now - 86400)
        );
        assert_eq!((code, md), (0, want));
        let (_, all) = go(&s, &c, &["closed", "--days", "0"], true);
        assert!(all.contains("2 done · 1 dropped") && all.contains("### Old win"));
        assert!(exec(&s, &["closed", "--days", "x"], &c, &|_| true).unwrap_err().downcast_ref::<Usage>().is_some());
    }

    #[test]
    fn edit_changes_fields_in_place_and_keeps_stage_and_position() {
        let (c, _) = cfg(3);
        let s = Store::open_in_memory();
        let a: i64 = go(&s, &c, &["add", "a", "--desc", "old", "--dir", "/d", "--label", "x"], true).1.parse().unwrap();
        go(&s, &c, &["add", "b"], true);
        let before = s.task(a).unwrap().unwrap();
        let (code, out) = go(&s, &c, &["edit", &a.to_string(), "--title", "A", "--label", "y", "--label", "z"], true);
        assert_eq!(code, 0, "{out}");
        let t = s.task(a).unwrap().unwrap();
        assert_eq!((t.title.as_str(), t.description.as_str(), t.dir.as_deref(), t.labels.clone()), ("A", "old", Some("/d"), vec!["y".to_string(), "z".into()]));
        assert_eq!((t.stage, t.position), (before.stage, before.position));
        go(&s, &c, &["edit", &a.to_string(), "--desc", "new", "--dir", ""], true);
        let t = s.task(a).unwrap().unwrap();
        assert_eq!((t.description.as_str(), t.dir), ("new", None));
        assert_eq!(go(&s, &c, &["edit", "999", "--title", "x"], true).0, 1);
        assert!(exec(&s, &["edit", &a.to_string(), "--title", " "], &c, &|_| true).unwrap_err().downcast_ref::<Usage>().is_some());
    }

    #[test]
    fn shelve_turns_a_finished_task_into_a_shelved_experiment() {
        let (c, dir) = cfg(3);
        let s = Store::open_in_memory();
        let id = claimed(&s, &c);
        s.transition(id, Stage::Review, 3, &|_, _| vec![]).unwrap().unwrap();
        let sid = id.to_string();
        let (code, msg) = go(&s, &c, &["shelve", &sid], true);
        assert!(code == 1 && msg.contains("no explainer"), "{msg}");
        assert_eq!((stage(&s, id), s.task(id).unwrap().unwrap().kind.as_str()), (Stage::Review, "default"));
        assert!(exec(&s, &["shelve", &sid, "--bookmark", "nope"], &c, &|_| true).unwrap_err().downcast_ref::<Usage>().is_some());
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("e.html");
        std::fs::write(&f, "<p>x</p>").unwrap();
        s.set_explainer(id, f.to_str().unwrap()).unwrap();
        assert_eq!(go(&s, &c, &["shelve", &sid, "--bookmark", "/r/app=feature/x"], true), (0, "Shelved.".into()));
        let t = s.task(id).unwrap().unwrap();
        assert_eq!((t.stage, t.kind.as_str()), (Stage::Shelved, "experiment"));
        assert_eq!(s.bookmarks(id).unwrap(), vec![("/r/app".to_string(), "feature/x".to_string())]);
        // Reviews are never experiments.
        let r: i64 = go(&s, &c, &["add", "r"], true).1.parse().unwrap();
        s.set_kind(r, "review", Some("https://github.com/o/r/pull/1")).unwrap();
        assert_eq!(go(&s, &c, &["shelve", &r.to_string()], true).0, 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn requeue_puts_a_claimed_task_back_at_the_bottom_of_the_queue() {
        let (c, _) = cfg(3);
        let s = Store::open_in_memory();
        let id = claimed(&s, &c);
        let other: i64 = go(&s, &c, &["add", "q"], true).1.parse().unwrap();
        assert_eq!(go(&s, &c, &["requeue", &id.to_string()], true), (0, "Back in the queue.".into()));
        assert_eq!(stage(&s, id), Stage::Queue);
        assert!(s.task(id).unwrap().unwrap().position > s.task(other).unwrap().unwrap().position);
        assert_eq!(go(&s, &c, &["requeue", &id.to_string()], true).0, 1);
    }

    #[test]
    fn list_shows_open_tasks_unless_a_stage_is_asked_for() {
        let (c, _) = cfg(3);
        let s = Store::open_in_memory();
        let a = claimed(&s, &c);
        let q: i64 = go(&s, &c, &["add", "q"], true).1.parse().unwrap();
        let d: i64 = go(&s, &c, &["add", "d"], true).1.parse().unwrap();
        go(&s, &c, &["drop", &d.to_string()], true);
        let ids = |args: &[&str]| -> Vec<i64> {
            let out: Value = serde_json::from_str(&go(&s, &c, args, true).1).unwrap();
            out.as_array().unwrap().iter().map(|t| t["id"].as_i64().unwrap()).collect()
        };
        let mut open = ids(&["list"]);
        open.sort();
        assert_eq!(open, vec![a, q]);
        assert_eq!(ids(&["list", "--stage", "dropped"]), vec![d]);
        assert_eq!(ids(&["list", "--stage", "queue", "--stage", "active"]).len(), 2);
        assert!(exec(&s, &["list", "--stage", "nope"], &c, &|_| true).unwrap_err().downcast_ref::<Usage>().is_some());
    }
}
