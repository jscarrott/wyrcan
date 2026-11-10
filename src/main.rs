mod app;
mod board;
mod config;
mod install;
mod state;
mod todoist;
mod ui;
mod zellij;

use anyhow::{Context, Result};
use app::{App, Effect};
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

fn main() -> Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("install") => install::run(std::path::Path::new(env!("CARGO_MANIFEST_DIR")), &config::home().join(".claude-work")),
        None => tui(),
        _ => anyhow::bail!("usage: claude-board [install]"),
    }
}

fn tui() -> Result<()> {
    let cfg = config::Config::from_env();
    let client = Arc::new(todoist::Client::new(cfg.token()?));
    // ratatui::init also installs a panic hook that restores the terminal.
    let mut term = ratatui::init();
    let res = run(&mut term, &cfg, client);
    ratatui::restore();
    res
}

fn run(term: &mut ratatui::DefaultTerminal, cfg: &config::Config, client: Arc<todoist::Client>) -> Result<()> {
    let (pid, ids) = match client.resolve(&cfg.project) {
        Ok(v) => v,
        Err(e) => {
            // Show the failure full-screen; any key quits.
            term.draw(|f| f.render_widget(format!("{e:#}\n\nPress any key to quit."), f.area()))?;
            while !matches!(event::read()?, Event::Key(_)) {}
            return Ok(());
        }
    };

    let (task_tx, task_rx) = mpsc::channel();
    let (refresh_tx, refresh_rx) = mpsc::channel::<()>();
    {
        let client = client.clone();
        std::thread::spawn(move || loop {
            if task_tx.send(client.tasks(&pid)).is_err() {
                return;
            }
            // Wakes on a refresh request, or after 20 s.
            if let Err(mpsc::RecvTimeoutError::Disconnected) = refresh_rx.recv_timeout(Duration::from_secs(20)) {
                return;
            }
        });
    }

    let cwd = std::env::current_dir().context("cwd")?.to_string_lossy().into_owned();
    let mut app = App::new(ids, zellij::current_session());
    app.on_agents(state::read_dir(&cfg.state_dir));
    let mut last_read = Instant::now();

    loop {
        while let Ok(r) = task_rx.try_recv() {
            app.on_tasks(r);
        }
        if last_read.elapsed() >= Duration::from_secs(1) {
            app.on_agents(state::read_dir(&cfg.state_dir));
            last_read = Instant::now();
        }
        term.draw(|f| ui::draw(f, &app))?;
        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        let Event::Key(k) = event::read()? else { continue };
        if k.kind != KeyEventKind::Press {
            continue;
        }
        for e in app.on_key(k.code) {
            if matches!(e, Effect::Quit) {
                return Ok(());
            }
            if let Err(err) = handle(e, &client, &cwd, &refresh_tx) {
                app.message = Some(format!("{err:#}"));
            }
        }
    }
}

fn handle(e: Effect, client: &todoist::Client, cwd: &str, refresh: &mpsc::Sender<()>) -> Result<()> {
    match e {
        Effect::SpawnNext => zellij::spawn_claude("next", cwd, &["/next"])?,
        Effect::SpawnPen => zellij::spawn_claude("pen", cwd, &["/pen"])?,
        Effect::Resume { session, cwd } => zellij::spawn_claude("resume", &cwd, &["--resume", &session])?,
        Effect::Jump(zellij::Jump::Focus(pane)) => zellij::run(&zellij::focus_cmd(&pane))?,
        Effect::Jump(_) | Effect::Quit => {}
        Effect::OpenInTodoist(id) => {
            zellij::run(&["xdg-open".into(), format!("https://app.todoist.com/app/task/{id}")])?
        }
        Effect::Complete { task_id, session } => {
            client.complete(&task_id)?;
            if let Some(s) = session {
                let hook = config::home().join(".claude-work/hooks/claude-board.sh");
                zellij::run(&[hook.to_string_lossy().into_owned(), "close".into(), s])?;
            }
            let _ = refresh.send(());
        }
        Effect::Refresh => {
            let _ = refresh.send(());
        }
    }
    Ok(())
}
