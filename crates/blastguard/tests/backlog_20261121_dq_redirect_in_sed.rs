#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED: `>&` inside a double-quoted sed argument (after `\"`) is not a
//! redirect. blastguard refused it with "> overwrites ...", a redirect reason.
//!
//! DOES NOT PROVE: real sed behaviour (nothing is executed). The `sed -i`
//! case may legitimately Ask for other reasons; it only asserts the reason is
//! not the redirect one.

#[path = "support/bounded.rs"]
mod bounded;

use std::process::{Command, Stdio};

struct Home(std::path::PathBuf);
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn unique_home(tag: &str) -> Home {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "bg-dqredir-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt").join("crates").join("x")).unwrap();
    Home(dir)
}

/// Returns (decision, reason).
fn decide(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "dq-redirect-sed-20261121",
        "cwd": home.0.join("wt"),
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    if subagent {
        p["agent_id"] = serde_json::json!("test-subagent");
    }
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    cmd.env_clear()
        .env("HOME", &home.0)
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut cmd, p.to_string().as_bytes(), bounded::HOOK_DEADLINE);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".into(), String::new());
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let h = &v["hookSpecificOutput"];
    (
        h["permissionDecision"].as_str().unwrap_or("?").to_string(),
        h["permissionDecisionReason"]
            .as_str()
            .unwrap_or("")
            .to_string(),
    )
}

#[test]
#[ignore = "RED(bfbb50ac): > inside double-quoted sed arg after backslash-quote read as redirect"]
fn sed_i_with_gt_amp_in_double_quotes_is_not_a_redirect_refusal() {
    let home = unique_home("verbatim");
    let cmd = r#"cd crates/x && sed -i "s|if matches!(op, \"<&\" \| \">&\") && target.contains('\$') {|if matches!(op, \"<\&\" \| \">\&\") \&\& target.contains('\$') \&\& decode_literal_word(target).is_err() {|" src/detect/payload/exec_fds.rs && sed -n 1009p src/detect/payload/exec_fds.rs"#;
    for subagent in [false, true] {
        let (d, r) = decide(cmd, &home, subagent);
        assert!(
            !r.contains("> overwrites"),
            "subagent={subagent} decision={d} redirect-based reason: {r}"
        );
    }
}
