mod app;
mod board;
mod cli;
mod config;
mod gate;
mod hook;
mod install;
mod model;
mod state;
mod store;
mod todoist;
mod ui;
mod zellij;

use anyhow::{Context, Result};
use app::{App, Effect};
use ratatui::crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind};
use ratatui::crossterm::execute;
use std::sync::{mpsc, Arc};
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
    let client = Arc::new(todoist::Client::new(cfg.token()?));
    // ratatui::init also installs a panic hook that restores the terminal.
    let mut term = ratatui::init();
    // shortcut: a panic skips DisableMouseCapture; run `reset` if the terminal then echoes mouse codes.
    execute!(std::io::stdout(), EnableMouseCapture)?;
    let res = run(&mut term, &cfg, client);
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
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

    let (upd_tx, upd_rx) = mpsc::channel::<Update>();
    let ctx = Ctx {
        client,
        cwd: std::env::current_dir().context("cwd")?.to_string_lossy().into_owned(),
        claude_dir: cfg.claude_dir.to_string_lossy().into_owned(),
        refresh: refresh_tx,
        upd: upd_tx,
    };
    let mut app = App::new(ids, zellij::current_session());
    app.on_agents(state::read_dir(&cfg.state_dir));
    let mut last_read = Instant::now();

    loop {
        while let Ok(r) = task_rx.try_recv() {
            app.on_tasks(r);
        }
        while let Ok(u) = upd_rx.try_recv() {
            u(&mut app);
        }
        if last_read.elapsed() >= Duration::from_secs(1) {
            app.on_agents(state::read_dir(&cfg.state_dir));
            last_read = Instant::now();
        }
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
        for e in effects {
            if matches!(e, Effect::Quit) {
                return Ok(());
            }
            if let Err(err) = handle(e, &ctx) {
                app.message = Some(format!("{err:#}"));
            }
        }
    }
}

/// A change to the app computed off the UI thread.
type Update = Box<dyn FnOnce(&mut App) + Send>;

struct Ctx {
    client: Arc<todoist::Client>,
    cwd: String,
    claude_dir: String,
    refresh: mpsc::Sender<()>,
    upd: mpsc::Sender<Update>,
}

/// Runs a Todoist call on its own thread so a slow API never freezes the board.
/// Errors land in the footer; `refresh` re-fetches afterwards (which also undoes a failed move).
fn background(ctx: &Ctx, refresh: bool, work: impl FnOnce(&todoist::Client) -> Result<Update> + Send + 'static) {
    let (client, upd, tx) = (ctx.client.clone(), ctx.upd.clone(), ctx.refresh.clone());
    std::thread::spawn(move || {
        let u = work(&client).unwrap_or_else(|e| {
            let m = format!("{e:#}");
            Box::new(move |a: &mut App| a.message = Some(m))
        });
        let _ = upd.send(u);
        if refresh {
            let _ = tx.send(());
        }
    });
}

fn handle(e: Effect, ctx: &Ctx) -> Result<()> {
    let dir = ctx.claude_dir.as_str();
    match e {
        Effect::SpawnNext(id) => zellij::spawn_claude("next", &ctx.cwd, dir, &[&format!("/next {id}")])?,
        Effect::SpawnPen => zellij::spawn_claude("pen", &ctx.cwd, dir, &["/pen"])?,
        Effect::Resume { session, cwd } => zellij::spawn_claude("resume", &cwd, dir, &["--resume", &session])?,
        Effect::Jump(zellij::Jump::Focus(pane)) => zellij::run(&zellij::focus_cmd(&pane))?,
        Effect::Jump(_) | Effect::Quit => {}
        Effect::OpenInTodoist(id) => {
            // Detached: Command::output() would block until the browser closes its inherited pipes.
            // shortcut: the child is never waited on (zombie until exit), fine for a short-lived TUI.
            std::process::Command::new("xdg-open")
                .arg(format!("https://app.todoist.com/app/task/{id}"))
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .context("running xdg-open")?;
        }
        Effect::Complete { task_id, .. } => background(ctx, true, move |c| {
            c.complete(&task_id)?;
            Ok(Box::new(|_| {}))
        }),
        Effect::Move { task_id, section_id } => background(ctx, true, move |c| {
            c.move_task(&task_id, &section_id)?;
            Ok(Box::new(|_| {}))
        }),
        Effect::LoadComment(id) => background(ctx, false, move |c| {
            let comment = c.latest_comment(&id)?;
            Ok(Box::new(move |a: &mut App| a.on_comment(id, comment)))
        }),
        Effect::Refresh => {
            let _ = ctx.refresh.send(());
        }
    }
    Ok(())
}
