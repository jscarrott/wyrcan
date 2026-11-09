use crate::config::home;
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

fn backup(path: &Path) -> Result<()> {
    let mut bak = path.as_os_str().to_owned();
    bak.push(format!(".bak-{}", now()));
    std::fs::rename(path, &bak).with_context(|| format!("backing up {}", path.display()))
}

/// Point `link` at `target`, backing up whatever regular file/dir is there.
fn symlink(target: &Path, link: &Path) -> Result<()> {
    match std::fs::symlink_metadata(link) {
        Ok(m) if m.file_type().is_symlink() => {
            if std::fs::read_link(link)? == target {
                return Ok(());
            }
            std::fs::remove_file(link)?;
        }
        Ok(_) => backup(link)?,
        Err(_) => {}
    }
    std::os::unix::fs::symlink(target, link)
        .with_context(|| format!("linking {} -> {}", link.display(), target.display()))?;
    println!("linked {} -> {}", link.display(), target.display());
    Ok(())
}

pub fn run(repo_root: &Path) -> Result<()> {
    let root = repo_root.canonicalize()?;
    let claude = home().join(".claude-work");
    let script = claude.join("hooks/claude-board.sh");
    std::fs::create_dir_all(claude.join("hooks"))?;
    std::fs::create_dir_all(claude.join("skills"))?;
    symlink(&root.join("hooks/claude-board.sh"), &script)?;
    let mut skills: Vec<PathBuf> = std::fs::read_dir(root.join("skills"))?
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    skills.sort();
    for s in skills {
        symlink(&s, &claude.join("skills").join(s.file_name().unwrap()))?;
    }

    let settings_path = claude.join("settings.json");
    let old: Value = match std::fs::read_to_string(&settings_path) {
        Ok(s) => serde_json::from_str(&s).context("parsing settings.json")?,
        Err(_) => json!({}),
    };
    let new = merge_hooks(old.clone(), &script);
    if new != old {
        if settings_path.exists() {
            backup(&settings_path)?;
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
