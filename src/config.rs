use std::path::PathBuf;

pub struct Config {
    pub token_file: PathBuf,
    pub project: String,
    pub state_dir: PathBuf,
}

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

impl Config {
    /// Same env vars and defaults as hooks/claude-board.sh.
    pub fn from_env() -> Config {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        Config {
            token_file: var("CLAUDE_BOARD_TOKEN_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|| home().join(".config/todoist/token")),
            project: var("CLAUDE_BOARD_PROJECT").unwrap_or_else(|| "Claude Board".into()),
            state_dir: var("CLAUDE_BOARD_STATE")
                .map(PathBuf::from)
                .unwrap_or_else(|| home().join(".local/state/claude-board")),
        }
    }

    pub fn token(&self) -> anyhow::Result<String> {
        use anyhow::Context;
        let t = std::fs::read_to_string(&self.token_file)
            .with_context(|| format!("no Todoist token at {}", self.token_file.display()))?;
        Ok(t.trim().to_string())
    }
}
