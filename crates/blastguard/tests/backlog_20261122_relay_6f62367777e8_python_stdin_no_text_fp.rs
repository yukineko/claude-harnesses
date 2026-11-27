//! RED probe (false positive; verbatim relay replay; independent test author,
//! 2026-11-22). Observed in condukt run run-20261122-035502-88763 (t1 worker,
//! subagent), relay bgr-6f62367777e8 (Deny), on the blastguard then running
//! (pre-5e99d94d4). The command was
//!   cd <task worktree>/crates/blastguard/src && python3 - <<'EOF'
//!   ... s=open(p).read() ... s.replace(old,new) ... open(p,'w').write(s)
//!   EOF
//!   grep -n "..." detect/interpreter/python_file.rs | head
//! Verbatim reason: "`python3 - <<'EOF'` takes its program from stdin, and
//! that program is not on the command line — blastguard has no text to read,
//! so it cannot tell whether anything here is irreversible" -- the program IS
//! on the command line (a quoted heredoc body).
//!
//! Fixture `support/relay_bgr_6f62367777e8_20261122_exact.txt` is the exact
//! relayed command; the only rewrite is the worktree prefix of the `cd`
//! target -> this test's cwd.
//!
//! Readings: (a) FP -- the body is text, its writes are literal relative .rs
//! paths in the session cwd; (b) intended -- some construct is outside the
//! supported Python subset (undetermined). Even under (b) "no text to read"
//! misstates what blastguard saw.
//!
//! DOES NOT PROVE: which part of the body triggers it; real Python execution.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/git_env.rs"]
mod git_env;

use std::path::Path;
use std::process::{Command, Stdio};

fn git(dir: &Path, args: &[&str]) {
    let mut cmd = git_env::pinned(dir);
    cmd.args([
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@example.invalid",
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "init.defaultBranch=main",
    ])
    .args(args)
    .env("GIT_CONFIG_GLOBAL", "/dev/null")
    .env("GIT_CONFIG_NOSYSTEM", "1");
    let out =
        bounded::run_bounded_stdin(&mut cmd, bounded::ChildStdin::Null, bounded::TOOL_DEADLINE);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

const EXACT: &str = include_str!("support/relay_bgr_6f62367777e8_20261122_exact.txt");
const ORIG_CD: &str = "/Users/yuki/.condukt/worktrees/run-20261122-035502-88763-t1";

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
        "bg-relay6f623-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let src = dir.join("wt/crates/blastguard/src");
    std::fs::create_dir_all(src.join("detect/interpreter")).unwrap();
    std::fs::write(
        src.join("detect/interpreter/python_file.rs"),
        "// fixture\n",
    )
    .unwrap();
    std::fs::write(src.join("detect/payload.rs"), "// fixture\n").unwrap();
    // The real cwd was inside a condukt task worktree: a git work tree in
    // which the edited files are tracked.
    let wt = dir.join("wt");
    git(&wt, &["init", "-q"]);
    git(&wt, &["add", "crates"]);
    git(&wt, &["commit", "-q", "-m", "fixture"]);
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "relay-6f62367777e8-20261122",
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

fn unallowed(cmd: &str, tag: &str) -> Vec<String> {
    let home = unique_home(tag);
    let wt = home.0.join("wt");
    let cmd = cmd.replace(ORIG_CD, wt.to_str().unwrap());
    let mut bad = Vec::new();
    for subagent in [false, true] {
        let (d, r) = run_as(&cmd, &home, subagent);
        if d != "allow" {
            bad.push(format!("subagent={subagent} => {d} ({r})"));
        }
    }
    bad
}

#[test]
#[ignore = "RED(c5202092, narrowed): the misleading 'no text to read' reason is fixed (the body's backtick/&&/; literal text was read as shell); remaining reason is S5 'open on a path the program text does not determine' (p rebound across two files) -- Allow here is a ruling question"]
fn relay_6f62367777e8_exact_command_is_allow() {
    assert!(EXACT.contains(ORIG_CD), "fixture: cd target present");
    let bad = unallowed(EXACT.trim_end_matches('\n'), "exact");
    assert!(
        bad.is_empty(),
        "expected Allow for the verbatim relayed command:\n{}",
        bad.join("\n")
    );
}

/// c5202092: whatever the verdict, the reason must not claim the program has
/// no text when it is a quoted heredoc body.
#[test]
fn relay_6f62367777e8_reason_does_not_claim_no_text_to_read() {
    let bad: Vec<String> = unallowed(EXACT.trim_end_matches('\n'), "reason")
        .into_iter()
        .filter(|b| b.contains("no text to read"))
        .collect();
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
