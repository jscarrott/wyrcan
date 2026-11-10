#[allow(dead_code)] // Config is consumed by later tasks
mod config;
#[allow(dead_code)] // consumed by Task 6
mod board;
#[allow(dead_code)]
mod state;
#[allow(dead_code)]
mod todoist;
mod install;
#[allow(dead_code)]
mod zellij;

fn main() -> anyhow::Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("install") => install::run(std::path::Path::new(env!("CARGO_MANIFEST_DIR")), &config::home().join(".claude-work")),
        _ => anyhow::bail!("usage: claude-board install"),
    }
}
