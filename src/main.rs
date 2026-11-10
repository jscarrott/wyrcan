mod app;
mod cli;
mod config;
mod effects;
mod gate;
mod hook;
mod install;
mod model;
mod pr;
mod store;
mod todoist;
mod ui;
mod zellij;

use anyhow::{Context, Result};
use app::App;
use ratatui::crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind};
use ratatui::crossterm::execute;
use std::collections::HashSet;
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("install") => {
            let dir = config::Config::from_env().claude_dir;
            println!("installing into {}", dir.display());
            install::run(std::path::Path::new(env!("CARGO_MANIFEST_DIR")), &dir, &config::home().join(".local/bin"))
        }
        Some("hook") => {
            hook::run(args.get(1).map(String::as_str).unwrap_or_default(), &config::Config::from_env());
            Ok(())
        }
        None => tui(),
        Some(_) => std::process::exit(cli::run(&args, &config::Config::from_env())?),
    }
}

fn tui() -> Result<()> {
    let cfg = config::Config::from_env();
    let store = store::Store::open(&cfg.db_path())?;
    // ratatui::init also installs a panic hook that restores the terminal.
    let mut term = ratatui::init();
    // shortcut: a panic skips DisableMouseCapture; run `reset` if the terminal then echoes mouse codes.
    execute!(std::io::stdout(), EnableMouseCapture)?;
    let res = run(&mut term, &cfg, &store);
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    res
}

fn run(term: &mut ratatui::DefaultTerminal, cfg: &config::Config, store: &store::Store) -> Result<()> {
    let zj = zellij::Zellij::from_env();
    let cwd = std::env::current_dir().context("cwd")?.to_string_lossy().into_owned();
    let ctx = effects::Ctx { store, cfg, zj: &zj, run: &zellij::ShellRunner, verify: &pr::exists, cwd: &cwd };
    let mut panes = effects::Panes::new(store::now());
    let mut app = App::new(app::snapshot(store, cfg)?);
    let mut last_read = Instant::now();
    let (tx, rx) = mpsc::channel::<(i64, Option<String>)>();
    let mut checking: HashSet<i64> = HashSet::new();
    spawn_poller();

    loop {
        let mut done = false;
        while let Ok((id, refusal)) = rx.try_recv() {
            checking.remove(&id);
            app.message = refusal;
            done = true;
        }
        if done {
            refresh(&mut app, store, cfg);
        }
        if last_read.elapsed() >= Duration::from_secs(1) {
            if let Err(e) = effects::tick(&ctx, &mut panes) {
                app.message = Some(format!("{e:#}"));
            }
            refresh(&mut app, store, cfg);
            last_read = Instant::now();
        }
        term.draw(|f| ui::draw(f, &mut app))?;
        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        let effects = match event::read()? {
            Event::Key(k) if k.kind == KeyEventKind::Press => app.on_key_event(k),
            Event::Mouse(m) => app.on_mouse(m),
            _ => continue,
        };
        // A task view that just opened has nothing to show until refresh reads it.
        if effects.is_empty() && !app.modal.as_ref().is_some_and(|v| v.detail.is_none()) {
            continue;
        }
        // The gate shells out to gh, so a move into Review runs off the UI thread.
        let (reviews, effects): (Vec<_>, Vec<_>) = effects.into_iter().partition(|e| matches!(e, app::Effect::ToReview(_)));
        for e in reviews {
            if let app::Effect::ToReview(id) = e {
                if checking.insert(id) {
                    app.message = Some("Checking PRs…".into());
                    spawn_review(id, tx.clone());
                }
            }
        }
        if effects::run_all(effects, &ctx, &mut panes, &mut app) {
            return Ok(());
        }
        refresh(&mut app, store, cfg);
    }
}

fn refresh(app: &mut App, store: &store::Store, cfg: &config::Config) {
    match app::snapshot(store, cfg) {
        Ok(s) => app.set_snapshot(s),
        Err(e) => app.message = Some(format!("{e:#}")),
    }
    let Some(id) = app.modal.as_ref().map(|v| v.id) else { return };
    match app::detail(store, id) {
        Ok(d) => app.modal.as_mut().expect("checked above").detail = d,
        Err(e) => app.message = Some(format!("{e:#}")),
    }
}

/// Each worker opens its own Store: rusqlite connections stay on one thread.
fn spawn_review(id: i64, tx: mpsc::Sender<(i64, Option<String>)>) {
    std::thread::spawn(move || {
        let r = (|| -> Result<Option<String>> {
            let cfg = config::Config::from_env();
            let store = store::Store::open(&cfg.db_path())?;
            Ok(match effects::transition(&store, &cfg, id, model::Stage::Review, &pr::exists)? {
                Ok(()) => {
                    pr::refresh(&store, id);
                    None
                }
                Err(r) => Some(r.to_string()),
            })
        })();
        let _ = tx.send((id, r.unwrap_or_else(|e| Some(format!("{e:#}")))));
    });
}

/// Refreshes every Review task's PRs now and every 120 s.
fn spawn_poller() {
    std::thread::spawn(|| loop {
        let cfg = config::Config::from_env();
        if let Ok(store) = store::Store::open(&cfg.db_path()) {
            for t in store.tasks().unwrap_or_default().iter().filter(|t| t.stage == model::Stage::Review) {
                pr::refresh(&store, t.id);
            }
        }
        std::thread::sleep(Duration::from_secs(120));
    });
}
