use std::path::PathBuf;

pub struct Config {
    pub token_file: PathBuf,
    pub project: String,
    /// `todoist = true` in the file turns the sync on.
    pub todoist: bool,
    pub state_dir: PathBuf,
    /// Claude Code config dir: hooks, skills and settings.json live here.
    pub claude_dir: PathBuf,
    /// Cap on tasks in the Active stage.
    pub max_active: usize,
    /// Also import open PRs in active repos that nobody asked you to review.
    pub reviews_all_open: bool,
    /// Raw config file text, for keys read on demand.
    pub(crate) file: String,
}

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

/// `~` and `~/…` mean $HOME.
pub fn expand_home(p: &str) -> String {
    match p.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("{}{rest}", home().to_string_lossy()),
        _ => p.to_string(),
    }
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
        Some(expand_home(v.trim().trim_matches('"')))
    })
}

fn reviews_all_open_in(file: &str) -> bool {
    file_value(file, "reviews_all_open").is_some_and(|v| v == "true")
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
            project: var("CLAUDE_BOARD_PROJECT")
                .or_else(|| file_value(&file, "todoist_project"))
                .unwrap_or_else(|| "Claude Board".into()),
            todoist: file_value(&file, "todoist").is_some_and(|v| v == "true"),
            state_dir: var("CLAUDE_BOARD_STATE")
                .map(PathBuf::from)
                .unwrap_or_else(|| home().join(".local/state/claude-board")),
            claude_dir: var("CLAUDE_CONFIG_DIR")
                .or_else(|| file_value(&file, "claude_dir"))
                .map(PathBuf::from)
                .unwrap_or_else(|| home().join(".claude")),
            max_active: file_value(&file, "max_active").and_then(|v| v.parse().ok()).unwrap_or(3),
            reviews_all_open: reviews_all_open_in(&file),
            file,
        }
    }

    /// Review-gate checks for a task kind: `gate_<kind>` in the file, comma-separated.
    /// An experiment is shelved, not merged, so by default it needs only an explainer.
    pub fn gate_checks(&self, kind: &str) -> Vec<String> {
        let dflt = if kind == "experiment" { "explainer" } else { "pr,explainer" };
        let v = file_value(&self.file, &format!("gate_{kind}")).unwrap_or_else(|| dflt.into());
        v.split(',').map(|c| c.trim().to_string()).filter(|c| !c.is_empty()).collect()
    }

    pub fn db_path(&self) -> PathBuf {
        self.state_dir.join("board.db")
    }

    pub fn explainer_dir(&self) -> PathBuf {
        self.state_dir.join("explainers")
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
        assert_eq!(file_value("max_active = 5", "max_active").and_then(|v| v.parse::<usize>().ok()), Some(5));
    }

    #[test]
    fn config_reads_reviews_all_open() {
        assert!(reviews_all_open_in("reviews_all_open = true"));
        assert!(!reviews_all_open_in("reviews_all_open = false"));
        assert!(!reviews_all_open_in("max_active = 3"));
    }

    #[test]
    fn gate_checks_default_and_override() {
        let mut c = Config::from_env();
        c.file = "gate_spike = \"pr, explainer ,\"\ngate_bug = \"explainer\"".into();
        assert_eq!(c.gate_checks("task"), ["pr", "explainer"]);
        assert_eq!(c.gate_checks("bug"), ["explainer"]);
        assert_eq!(c.gate_checks("spike"), ["pr", "explainer"]);
        assert_eq!(c.gate_checks("experiment"), ["explainer"]);
    }
}
