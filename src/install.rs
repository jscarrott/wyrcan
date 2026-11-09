use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// (event, matcher, script arg, async)
pub const HOOKS: &[(&str, Option<&str>, &str, bool)] = &[
    ("PostToolUse", Some("Edit|Write|MultiEdit|NotebookEdit"), "edit", true),
    ("Stop", None, "stop", true),
];

fn is_board(entry: &Value) -> bool {
    entry["hooks"].as_array().is_some_and(|hs| {
        hs.iter().any(|h| h["command"].as_str().is_some_and(|c| c.contains("claude-board.sh")))
    })
}

pub fn merge_hooks(mut settings: Value, script: &Path) -> Value {
    if !settings["hooks"].is_object() {
        settings["hooks"] = json!({});
    }
    let hooks = settings["hooks"].as_object_mut().unwrap();
    // Drop every board entry first so stale paths/args don't linger.
    for list in hooks.values_mut().filter_map(Value::as_array_mut) {
        list.retain(|e| !is_board(e));
    }
    hooks.retain(|_, v| v.as_array().is_none_or(|a| !a.is_empty()));
    for (event, matcher, arg, is_async) in HOOKS {
        let mut entry = json!({"hooks": [{
            "type": "command",
            "command": format!("bash {} {arg}", script.display()),
            "timeout": 30,
            "async": is_async,
        }]});
        if let Some(m) = matcher {
            entry["matcher"] = json!(m);
        }
        hooks.entry(event.to_string()).or_insert_with(|| json!([])).as_array_mut().unwrap().push(entry);
    }
    settings
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Backups live outside hooks/ and skills/ so Claude Code never loads them.
fn backup_path(claude: &Path, path: &Path, ts: u64) -> PathBuf {
    let rel = path.strip_prefix(claude).unwrap_or(path);
    claude.join("backups/claude-board").join(ts.to_string()).join(rel)
}

fn backup(claude: &Path, path: &Path) -> Result<()> {
    let dest = backup_path(claude, path, now());
    std::fs::create_dir_all(dest.parent().unwrap())?;
    std::fs::rename(path, &dest).with_context(|| format!("backing up {}", path.display()))
}

/// Point `link` at `target`, backing up whatever regular file/dir is there.
fn symlink(claude: &Path, target: &Path, link: &Path) -> Result<()> {
    match std::fs::symlink_metadata(link) {
        Ok(m) if m.file_type().is_symlink() => {
            if std::fs::read_link(link)? == target {
                return Ok(());
            }
            std::fs::remove_file(link)?;
        }
        Ok(_) => backup(claude, link)?,
        Err(_) => {}
    }
    std::os::unix::fs::symlink(target, link)
        .with_context(|| format!("linking {} -> {}", link.display(), target.display()))?;
    println!("linked {} -> {}", link.display(), target.display());
    Ok(())
}

pub fn run(repo_root: &Path, claude: &Path) -> Result<()> {
    let root = repo_root.canonicalize()?;
    // Validate before touching anything under the target dir.
    for need in ["hooks/claude-board.sh", "skills"] {
        anyhow::ensure!(root.join(need).exists(), "{} not found in {}", need, root.display());
    }
    let claude = claude.to_path_buf();
    let script = claude.join("hooks/claude-board.sh");
    std::fs::create_dir_all(claude.join("hooks"))?;
    std::fs::create_dir_all(claude.join("skills"))?;
    symlink(&claude, &root.join("hooks/claude-board.sh"), &script)?;
    let mut skills: Vec<PathBuf> = std::fs::read_dir(root.join("skills"))?
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    skills.sort();
    for s in skills {
        symlink(&claude, &s, &claude.join("skills").join(s.file_name().unwrap()))?;
    }

    let settings_path = claude.join("settings.json");
    let old: Value = match std::fs::read_to_string(&settings_path) {
        Ok(s) => serde_json::from_str(&s).context("parsing settings.json")?,
        Err(_) => json!({}),
    };
    let new = merge_hooks(old.clone(), &script);
    if new != old {
        if settings_path.exists() {
            backup(&claude, &settings_path)?;
        }
        std::fs::write(&settings_path, serde_json::to_string_pretty(&new)? + "\n")?;
        println!("updated {}", settings_path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::Path;

    #[test]
    fn backups_go_outside_skills_and_hooks() {
        let c = Path::new("/h/.claude-work");
        let b = backup_path(c, &c.join("skills/next"), 7);
        assert_eq!(b, Path::new("/h/.claude-work/backups/claude-board/7/skills/next"));
    }

    #[test]
    fn run_refuses_bad_root_without_touching_target() {
        let t = std::env::temp_dir().join(format!("cb-test-{}", std::process::id()));
        let (root, claude) = (t.join("root"), t.join("claude"));
        std::fs::create_dir_all(root.join("skills")).unwrap();
        std::fs::create_dir_all(claude.join("hooks")).unwrap();
        std::fs::write(claude.join("hooks/claude-board.sh"), "live").unwrap();
        assert!(run(&root, &claude).is_err());
        assert_eq!(std::fs::read_to_string(claude.join("hooks/claude-board.sh")).unwrap(), "live");
        assert!(!claude.join("backups").exists());
        std::fs::remove_dir_all(&t).unwrap();
    }

    const P: &str = "/h/.claude-work/hooks/claude-board.sh";

    #[test]
    fn merge_adds_hooks_to_empty() {
        let v = merge_hooks(json!({}), Path::new(P));
        let e = &v["hooks"]["PostToolUse"][0];
        assert_eq!(e["matcher"], "Edit|Write|MultiEdit|NotebookEdit");
        assert_eq!(e["hooks"][0]["command"], format!("bash {P} edit"));
        assert_eq!(e["hooks"][0]["async"], true);
        assert_eq!(v["hooks"]["Stop"][0]["hooks"][0]["command"], format!("bash {P} stop"));
    }

    #[test]
    fn merge_is_idempotent() {
        let once = merge_hooks(json!({"model": "x"}), Path::new(P));
        assert_eq!(merge_hooks(once.clone(), Path::new(P)), once);
    }

    #[test]
    fn merge_keeps_unrelated_hooks() {
        let herdr = json!({"hooks":[{"type":"command","command":"bash herdr.sh session"}]});
        let other = json!({"matcher":"Bash","hooks":[{"type":"command","command":"other.sh"}]});
        let s = json!({"theme":"dark","hooks":{"SessionStart":[herdr],"PostToolUse":[other]}});
        let v = merge_hooks(s, Path::new(P));
        assert_eq!(v["theme"], "dark");
        assert_eq!(v["hooks"]["SessionStart"], json!([herdr]));
        assert_eq!(v["hooks"]["PostToolUse"][0], other);
        assert_eq!(v["hooks"]["PostToolUse"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn merge_replaces_stale_board_entries() {
        let stale = json!({"hooks":[{"type":"command","command":"bash /old/claude-board.sh edit"}]});
        let s = json!({"hooks":{"Stop":[stale]}});
        let v = merge_hooks(s, Path::new(P));
        assert_eq!(v["hooks"]["Stop"].as_array().unwrap().len(), 1);
        assert_eq!(v["hooks"]["Stop"][0]["hooks"][0]["command"], format!("bash {P} stop"));
    }
}
