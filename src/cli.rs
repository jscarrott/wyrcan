//! Subcommands the skills call. stdout is JSON where noted; refusals go to stderr with exit 1.
use crate::config::Config;
use crate::gate::{self, PrVerifier};
use crate::model::{Refusal, Stage, Task};
use crate::store::{NewTask, Store};
use anyhow::{bail, Result};
use serde_json::{json, Value};

const USAGE: &str = "usage: claude-board add TITLE [--desc D] [--dir DIR] [--label L].. | next | claim SESSION TASK_ID CWD | register-pen SESSION CWD | status SESSION | gate SESSION [--pr ROOT=URL].. [--no-pr ROOT=REASON].. [--explainer FILE] [--summary TEXT] | drop TASK_ID | export | sync";

pub fn run(args: &[String], cfg: &Config) -> Result<i32> {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    if !matches!(a.first(), Some(&("add" | "next" | "claim" | "register-pen" | "status" | "gate" | "drop" | "export" | "sync"))) {
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
    json!({"id": t.id, "title": t.title, "description": t.description, "dir": t.dir, "labels": t.labels})
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

const GATE_FLAGS: &[&str] = &["--pr", "--no-pr", "--explainer", "--summary"];

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
            let (pos, fl) = parse(rest, &["--desc", "--dir", "--label"])?;
            let [title] = pos[..] else { return usage("add takes one TITLE") };
            if title.trim().is_empty() {
                return usage("TITLE can't be blank");
            }
            let val = |k| fl.iter().rev().find(|f| f.0 == k).map(|f| f.1);
            let labels: Vec<String> = fl.iter().filter(|f| f.0 == "--label").map(|f| f.1.to_string()).collect();
            let id = store.add_task(NewTask { title, description: val("--desc").unwrap_or(""), dir: val("--dir"), labels: &labels, todoist_id: None })?;
            ok(id.to_string())
        }
        "next" => match store.tasks()?.into_iter().find(|t| t.stage == Stage::Queue) {
            Some(t) => ok(task_json(&t).to_string()),
            None => Ok((1, "Queue is empty".into())),
        },
        "claim" => {
            let [session, id, cwd] = rest[..] else { return usage("claim takes SESSION TASK_ID CWD") };
            let Ok(id) = id.parse::<i64>() else { return usage("TASK_ID must be a number") };
            let Some(t) = store.task(id)? else { return refused(Refusal::NoSuchTask) };
            match t.stage {
                Stage::Active => {}
                Stage::Queue | Stage::Parked => {
                    if let Err(r) = store.transition(id, Stage::Active, cfg.max_active, &|_, _| vec![])? {
                        return refused(r);
                    }
                }
                from => return refused(Refusal::NotAllowed { from, to: Stage::Active }),
            }
            store.link_session(session, id, cwd)?;
            ok(task_json(&store.task(id)?.expect("just read")).to_string())
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
                Ok(()) => ok("In review.".into()),
                Err(Refusal::Gate(m)) => Ok((1, format!("Not ready for review. Missing:\n- {}", m.join("\n- ")))),
                Err(r) => refused(r),
            }
        }
        "drop" => {
            let [id] = rest[..] else { return usage("drop takes TASK_ID") };
            let Ok(id) = id.parse::<i64>() else { return usage("TASK_ID must be a number") };
            match store.transition(id, Stage::Dropped, cfg.max_active, &|_, _| vec![])? {
                Ok(()) => ok("Dropped.".into()),
                Err(r) => refused(r),
            }
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
}
