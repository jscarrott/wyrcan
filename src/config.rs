use std::path::PathBuf;

pub struct Config {
    pub token_file: PathBuf,
    pub project: String,
    pub state_dir: PathBuf,
    /// Claude Code config dir: hooks, skills and settings.json live here.
    pub claude_dir: PathBuf,
}

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

fn config_file() -> PathBuf {
    std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"))
        .join("claude-board/config.toml")
}

/// Reads `key = "value"` from a flat TOML file; `~/` expands to $HOME.
// shortcut: no tables, arrays or escapes; switch to the toml crate if the file grows.
fn file_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let (k, v) = l.split_once('=')?;
        if l.trim_start().starts_with('#') || k.trim() != key {
            return None;
        }
        let v = v.trim().trim_matches('"');
        Some(match v.strip_prefix("~/") {
            Some(rest) => home().join(rest).to_string_lossy().into_owned(),
            None => v.to_string(),
        })
    })
}

impl Config {
    /// Same env vars and defaults as hooks/claude-board.sh, plus claude_dir.
    pub fn from_env() -> Config {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let file = std::fs::read_to_string(config_file()).unwrap_or_default();
        Config {
            token_file: var("CLAUDE_BOARD_TOKEN_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|| home().join(".config/todoist/token")),
            project: var("CLAUDE_BOARD_PROJECT").unwrap_or_else(|| "Claude Board".into()),
            state_dir: var("CLAUDE_BOARD_STATE")
                .map(PathBuf::from)
                .unwrap_or_else(|| home().join(".local/state/claude-board")),
            claude_dir: var("CLAUDE_CONFIG_DIR")
                .or_else(|| file_value(&file, "claude_dir"))
                .map(PathBuf::from)
                .unwrap_or_else(|| home().join(".claude")),
        }
    }

    pub fn hook(&self) -> PathBuf {
        self.claude_dir.join("hooks/claude-board.sh")
    }

    pub fn token(&self) -> anyhow::Result<String> {
        use anyhow::Context;
        let t = std::fs::read_to_string(&self.token_file)
            .with_context(|| format!("no Todoist token at {}", self.token_file.display()))?;
        Ok(t.trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_value_reads_key() {
        let t = "# claude_dir = \"/nope\"\nproject = \"x\"\nclaude_dir = \"/opt/claude\"\n";
        assert_eq!(file_value(t, "claude_dir").as_deref(), Some("/opt/claude"));
        assert_eq!(file_value(t, "missing"), None);
        let tilde = file_value("claude_dir = \"~/.claude-work\"", "claude_dir").unwrap();
        assert_eq!(PathBuf::from(tilde), home().join(".claude-work"));
    }
}
