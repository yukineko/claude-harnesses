#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive, observed 2026-11-13 at main d37ae072): a QUOTED
//! heredoc fed to `python3 -` whose body holds the text `>> ` followed by a
//! backtick span, e.g. `print("x >> `y`")`, is read as a shell append redirect
//! to a run-time path: main session Ask, subagent Deny, with the verbatim
//! reason
//!
//!   '>> `y`' appends to a path that only exists at run time — blastguard
//!   cannot tell whether it is a protected gate/config file or a system directory
//!
//! (crates/blastguard/src/detect.rs, the `has_unresolvable_expansion` append
//! arm). A quoted-delimiter heredoc body is not expanded or parsed by the shell;
//! it is the Python program on stdin and writes nothing.
//!
//! Not a duplicate of a2d36aa2 (backlog_20261113_heredoc_body_fp.rs FP-A):
//! that one needs an unresolvable `cd` and quotes `> t,` as a RELATIVE redirect;
//! this one has no cd at all and goes through the run-time-path append arm.
//! Probed at HEAD: `print("x >> y")` (no backticks), `print(1 >> 2)` and
//! `print("x << `y`")` are all allow — the trigger is `>>` + a backtick target.
//!
//! Controls (GREEN at HEAD, not ignored): a real `>> "$var"` append redirect is
//! not allowed; the same python body without backticks is allowed.
//!
//! DOES NOT PROVE: which stage misreads the body as shell; anything about
//! unquoted heredocs (where the shell does expand backticks).

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
        "bg-pyhd-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook. Returns (decision, reason).
fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "py-heredoc-append-20261113",
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

const BODY_WITH_BACKTICK: &str = "python3 - <<'EOF'\nprint(\"x >> `y`\")\nEOF";
const BODY_PLAIN: &str = "python3 - <<'EOF'\nprint(\"x >> y\")\nEOF";

// test-weakening-justified: new RED for an unfixed false positive (backlog
// pending, 2026-11-13); the ignore is removed in the fixing commit.
#[test]
#[ignore = "RED: <backlog-pending> 2026-11-13 quoted python heredoc body `>> `y`` read as run-time append redirect — remove the ignore when fixed"]
fn quoted_python_heredoc_body_append_text_is_not_a_redirect() {
    let home = unique_home("red");
    for subagent in [false, true] {
        let (d, r) = run_as(BODY_WITH_BACKTICK, &home, subagent);
        assert!(
            !r.contains("appends to a path"),
            "subagent={subagent}: heredoc BODY text read as an append redirect: {d} | {r}"
        );
        assert_ne!(d, "deny", "subagent={subagent}: {d} | {r}");
    }
}

// Control: without the backtick span the same body is allowed.
#[test]
fn control_same_body_without_backticks_is_allowed() {
    let home = unique_home("ctl1");
    for subagent in [false, true] {
        let (d, r) = run_as(BODY_PLAIN, &home, subagent);
        assert_eq!(d, "allow", "subagent={subagent}: {d} | {r}");
    }
}

// Control: a real append redirect to a run-time path stays not-Allow.
#[test]
fn control_real_append_to_runtime_path_is_not_allowed() {
    let home = unique_home("ctl2");
    for subagent in [false, true] {
        let (d, r) = run_as("echo hi >> \"$var\"", &home, subagent);
        assert_ne!(d, "allow", "subagent={subagent}: {d} | {r}");
    }
}
