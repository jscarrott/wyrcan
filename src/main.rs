#[allow(dead_code)] // Config is consumed by later tasks
mod config;
mod install;

fn main() -> anyhow::Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("install") => install::run(std::path::Path::new(env!("CARGO_MANIFEST_DIR")), &config::home().join(".claude-work")),
        _ => anyhow::bail!("usage: claude-board install"),
    }
}
