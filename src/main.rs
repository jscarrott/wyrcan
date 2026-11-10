mod app;
mod cli;
mod config;
mod effects;
mod gate;
mod hook;
mod install;
mod model;
mod store;
mod todoist;
mod ui;
mod zellij;

use anyhow::{Context, Result};
use app::App;
use ratatui::crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind};
use ratatui::crossterm::execute;
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
    // shortcut: the gate's `gh pr view` runs on the UI thread when a card is moved into Review by key; Task 8 adds the background poller.
    let ctx = effects::Ctx { store, cfg, zj: &zj, run: &zellij::ShellRunner, verify: &cli::pr_exists, cwd: &cwd };
    let mut panes = effects::Panes::new(store::now());
    let mut app = App::new(app::snapshot(store, cfg)?);
    let mut last_read = Instant::now();

    loop {
        if last_read.elapsed() >= Duration::from_secs(1) {
            if let Err(e) = effects::tick(&ctx, &mut panes) {
                app.message = Some(format!("{e:#}"));
            }
            refresh(&mut app, store, cfg);
            last_read = Instant::now();
        }
        app.wide = term.size()?.width >= ui::WIDE_MIN;
        let mut hits = vec![];
        term.draw(|f| hits = ui::draw(f, &app))?;
        app.hits = hits;
        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        let effects = match event::read()? {
            Event::Key(k) if k.kind == KeyEventKind::Press => app.on_key_event(k),
            Event::Mouse(m) => app.on_mouse(m),
            _ => continue,
        };
        if effects.is_empty() {
            continue;
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
}
