use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// (Claude hook, matcher, `claude-board hook` event); all sync with a 5 s timeout.
pub const HOOKS: &[(&str, Option<&str>, &str)] = &[
    ("UserPromptSubmit", None, "working"),
    ("PostToolUse", Some("*"), "working"),
    ("PostToolUse", Some("Edit|Write|MultiEdit|NotebookEdit"), "edit"),
    ("Stop", None, "waiting"),
    ("Notification", Some("permission_prompt|elicitation_dialog"), "blocked"),
    ("SessionEnd", None, "ended"),
];

/// Also matches the retired bash hook so old entries are replaced.
fn is_board(entry: &Value) -> bool {
    entry["hooks"].as_array().is_some_and(|hs| {
        hs.iter().any(|h| {
            h["command"].as_str().is_some_and(|c| {
                c.contains("claude-board.sh") || c.split_whitespace().next().is_some_and(|b| b.ends_with("/claude-board")) && c.contains(" hook ")
            })
        })
    })
}

pub fn merge_hooks(mut settings: Value, bin: &Path) -> Value {
    if !settings["hooks"].is_object() {
        settings["hooks"] = json!({});
    }
    let hooks = settings["hooks"].as_object_mut().unwrap();
    // Drop every board entry first so stale paths/args don't linger.
    for list in hooks.values_mut().filter_map(Value::as_array_mut) {
        list.retain(|e| !is_board(e));
    }
    hooks.retain(|_, v| v.as_array().is_none_or(|a| !a.is_empty()));
    for (event, matcher, arg) in HOOKS {
        let mut entry = json!({"hooks": [{
            "type": "command",
            "command": format!("{} hook {arg}", bin.display()),
            "timeout": 5,
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

pub fn run(repo_root: &Path, claude: &Path, bin_dir: &Path) -> Result<()> {
    let root = repo_root.canonicalize()?;
    // Validate before touching anything under the target dirs.
    anyhow::ensure!(root.join("skills").exists(), "skills not found in {}", root.display());
    let target = root.join("target/release/claude-board");
    anyhow::ensure!(target.exists(), "{} is missing; build first: cargo build --release", target.display());
    let claude = claude.to_path_buf();
    let bin = bin_dir.join("claude-board");
    std::fs::create_dir_all(bin_dir)?;
    std::fs::create_dir_all(claude.join("skills"))?;
    symlink(&claude, &target, &bin)?;
    // The bash hook is gone; its link would dangle.
    let stale = claude.join("hooks/claude-board.sh");
    if std::fs::symlink_metadata(&stale).is_ok_and(|m| m.file_type().is_symlink()) {
        std::fs::remove_file(&stale)?;
        println!("removed {}", stale.display());
    }
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
    let new = merge_hooks(old.clone(), &bin);
    if new != old {
        // Copy, not move: a failed write below must still leave settings.json in place.
        if settings_path.exists() {
            let dest = backup_path(&claude, &settings_path, now());
            std::fs::create_dir_all(dest.parent().unwrap())?;
            std::fs::copy(&settings_path, &dest).context("backing up settings.json")?;
        }
        let tmp = settings_path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&new)? + "\n")?;
        std::fs::rename(&tmp, &settings_path).context("replacing settings.json")?;
        println!("updated {}", settings_path.display());
    }
    println!("Add this to the `keybinds` block of your zellij config (not edited for you):\n{}", crate::zellij::RETURN_KEYBIND);
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
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&claude).unwrap();
        assert!(run(&root, &claude, &t.join("bin")).is_err());
        assert!(!claude.join("skills").exists() && !t.join("bin").exists());
        std::fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn run_refuses_an_unbuilt_binary_without_touching_target() {
        let t = std::env::temp_dir().join(format!("cb-unbuilt-{}", std::process::id()));
        let (root, claude) = (t.join("root"), t.join("claude"));
        std::fs::create_dir_all(root.join("skills/next")).unwrap();
        std::fs::create_dir_all(&claude).unwrap();
        let e = run(&root, &claude, &t.join("bin")).unwrap_err();
        assert!(e.to_string().contains("build first: cargo build --release"), "{e}");
        assert!(!claude.join("skills").exists() && !t.join("bin").exists());
        std::fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn run_keeps_settings_key_order_and_backs_up() {
        let t = std::env::temp_dir().join(format!("cb-order-{}", std::process::id()));
        let (root, claude) = (t.join("root"), t.join("claude"));
        std::fs::create_dir_all(root.join("skills/next")).unwrap();
        std::fs::create_dir_all(root.join("target/release")).unwrap();
        std::fs::write(root.join("target/release/claude-board"), "").unwrap();
        std::fs::create_dir_all(&claude).unwrap();
        let orig = r#"{"zeta": 1, "alpha": {"y": 1, "b": 2}}"#;
        std::fs::write(claude.join("settings.json"), orig).unwrap();
        run(&root, &claude, &t.join("bin")).unwrap();
        let out = std::fs::read_to_string(claude.join("settings.json")).unwrap();
        let pos = |k: &str| out.find(k).unwrap();
        assert!(pos("zeta") < pos("alpha") && pos("alpha") < pos("hooks") && pos("\"y\"") < pos("\"b\""), "{out}");
        let backups: Vec<_> = std::fs::read_dir(claude.join("backups/claude-board")).unwrap().collect();
        let b = backups[0].as_ref().unwrap().path().join("settings.json");
        assert_eq!(std::fs::read_to_string(b).unwrap(), orig);
        std::fs::remove_dir_all(&t).unwrap();
    }

    const P: &str = "/h/.local/bin/claude-board";

    #[test]
    fn merge_adds_hooks_to_empty() {
        let v = merge_hooks(json!({}), Path::new(P));
        let e = &v["hooks"]["PostToolUse"][1];
        assert_eq!(e["matcher"], "Edit|Write|MultiEdit|NotebookEdit");
        assert_eq!(e["hooks"][0]["command"], format!("{P} hook edit"));
        assert_eq!(e["hooks"][0]["timeout"], 5);
        assert_eq!(v["hooks"]["Stop"][0]["hooks"][0]["command"], format!("{P} hook waiting"));
        assert_eq!(v["hooks"]["Notification"][0]["matcher"], "permission_prompt|elicitation_dialog");
        let n: usize = v["hooks"].as_object().unwrap().values().map(|l| l.as_array().unwrap().len()).sum();
        assert_eq!(n, HOOKS.len());
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
        assert_eq!(v["hooks"]["PostToolUse"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn merge_replaces_old_bash_entries() {
        let old = |a: &str| json!({"hooks":[{"type":"command","command":format!("bash /old/hooks/claude-board.sh {a}")}]});
        let s = json!({"hooks":{"Stop":[old("stop"), old("state waiting")]}});
        let v = merge_hooks(s, Path::new(P));
        assert_eq!(v["hooks"]["Stop"].as_array().unwrap().len(), 1);
        assert_eq!(v["hooks"]["Stop"][0]["hooks"][0]["command"], format!("{P} hook waiting"));
        assert!(!v.to_string().contains("claude-board.sh"));
    }
}
