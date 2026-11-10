use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn hook(state: &std::path::Path, stdin: &str) {
    let t = Instant::now();
    let mut c = Command::new(env!("CARGO_BIN_EXE_claude-board"))
        .args(["hook", "edit"])
        .env("CLAUDE_BOARD_STATE", state)
        .env("HOME", state)
        .env("XDG_CONFIG_HOME", state)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    c.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let o = c.wait_with_output().unwrap();
    assert!(o.status.success());
    assert!(o.stdout.is_empty());
    assert!(t.elapsed() < Duration::from_secs(2));
}

#[test]
fn hook_survives_unusable_db() {
    let d = std::env::temp_dir().join(format!("cb-cli-{}", std::process::id()));
    std::fs::create_dir_all(d.join("board.db")).unwrap();
    hook(&d, r#"{"session_id":"s","cwd":"/","tool_input":{"file_path":"/tmp/x"}}"#);
    hook(&d, "not json {{{");
    std::fs::remove_dir_all(&d).unwrap();
}
