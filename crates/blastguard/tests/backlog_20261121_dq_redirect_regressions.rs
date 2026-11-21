#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Regressions for backlog bfbb50ac: a `>` / `>&` inside ONE double-quoted word
//! that follows a backslash-escaped quote (`\\\"`) is not a redirect.
//!
//! PROVES: for the RED commands (each accepted by `bash -n`, every `>`/`>&`
//! inside one double-quoted word), the decision reason names neither
//! "> overwrites" nor "RELATIVE redirect", for subagent=false and true.
//!
//! DOES NOT PROVE: that the RED commands are allowed (they may legitimately Ask
//! for other reasons), real shell behaviour (nothing is executed), or anything
//! about unterminated quotes (bash refuses them at parse time; out of scope).

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

fn assert_no_redirect_reason(cmd: &str, tag: &str) {
    let home = unique_home(tag);
    for subagent in [false, true] {
        let (d, r) = decide(cmd, &home, subagent);
        assert!(
            !r.contains("> overwrites") && !r.contains("RELATIVE redirect"),
            "subagent={subagent} decision={d} redirect-based reason: {r}"
        );
    }
}

#[test]
#[ignore = "RED(bfbb50ac): > after backslash-quote inside double quotes read as redirect"]
fn echo_gt_after_escaped_quote_is_one_word() {
    assert_no_redirect_reason(r#"echo "x \\\" y > z""#, "echo");
}

#[test]
#[ignore = "RED(bfbb50ac): > after backslash-quote inside double quotes read as redirect"]
fn cd_then_backlog_title_gt_after_escaped_quote() {
    assert_no_redirect_reason(
        r#"cd crates/x && backlog add --title "after \\\" refused > FP" --scope "crates/blastguard""#,
        "cdgt",
    );
}

#[test]
#[ignore = "RED(bfbb50ac): >& after backslash-quote inside double quotes read as redirect"]
fn cd_then_echo_gt_amp_after_escaped_quote() {
    assert_no_redirect_reason(r#"cd crates/x && echo "x \\\" y >& z""#, "cdgtamp");
}

/// Orchestrator reconstruction of the driver observation; allowed at 139adef60.
#[test]
fn guard_reconstructed_backlog_add_with_gt_amp_in_title() {
    assert_no_redirect_reason(
        r#"cd crates/x && backlog add --title "blastguard bfbb50ac: sed >& in double-quoted arg after \\\" refused as redirect (FP)" --scope "crates/blastguard" --done-criteria "backlog_20261121_dq_redirect_in_sed passes""#,
        "guard",
    );
}

fn assert_restrictive(cmd: &str, tag: &str) {
    let home = unique_home(tag);
    for subagent in [false, true] {
        let (d, r) = decide(cmd, &home, subagent);
        assert!(
            d != "allow",
            "subagent={subagent} expected non-allow, got allow ({r})"
        );
    }
}

/// Control: `\\` is an escaped backslash, the quote closes, `>` IS a redirect
/// onto command-running configuration.
#[test]
fn control_escaped_backslash_closes_quote_then_redirect_to_settings() {
    assert_restrictive(r#"echo "x \\" > .claude/settings.json"#, "ctl1");
}

/// Control: `\"b\"` stays inside the quotes, the quote then closes, real redirect.
#[test]
fn control_closed_quote_then_redirect_to_settings() {
    assert_restrictive(r#"echo "a \"b\"" > .claude/settings.json"#, "ctl2");
}

#[test]
fn control_after_cd_escaped_backslash_redirect_to_settings() {
    assert_restrictive(
        r#"cd crates/x && echo "x \\" > ../../.claude/settings.json"#,
        "ctl3",
    );
}
