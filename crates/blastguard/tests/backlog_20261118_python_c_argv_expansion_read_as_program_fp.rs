#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED pin (2026-11-18, relay bgr-32cea6e4ce95): a shell expansion in an
//! ARGV operand after `python3 -c PROGRAM` is reported as an expansion IN
//! the program, and a read-only program is refused.
//!
//! Relay row (verbatim reason):
//!
//! > `python3` is invoked with an inline-eval flag, and that program contains
//! > a shell expansion (`$D/blocks.jsonl`) — the text on the command line is
//! > a template, not the program that will run, so blastguard cannot tell
//! > whether anything here is irreversible
//!
//! for `… python3 -c "import json,sys;[print(…) for l in open(sys.argv[1])]"
//! "$D/blocks.jsonl"`. The program text holds no `$` and no backtick;
//! `$D/blocks.jsonl` is `sys.argv[1]`, data the program only reads.
//!
//! Cause (read, not guessed): `detect/payload.rs::interpreter_payloads_after`
//! returns EVERY shell word after the `-c` flag as a candidate program, and
//! `detect/interpreter.rs` (the "TEXT THAT WILL BE SUBSTITUTED IS NOT THE
//! PROGRAM EITHER" check) asks for any candidate containing `$`.
//!
//! Expected: Allow. Grounds:
//!   * user ruling 2026-11-12 (memory `python-c-expansion-in-literal-deny`):
//!     the Ask is correct when the expansion is INSIDE the program text, and
//!     probes should "pass the path via argv or env" instead — the shape
//!     pinned here is that recommended shape.
//!   * the literal-argv twin of the same program is Allow today (control).
//!   * memory `false-positives-not-tolerated`: a gate FP is a bug.
//!
//! Observed at origin/main 1dfa60681 (blastguard 0.2.699), debug binary,
//! main mode `ask`, subagent mode `deny`, for each RED case below.
//!
//! Content rule: the programs only read; every path is a sentinel.
//!
//! DOES NOT PROVE: argv expansions for a program that WRITES `sys.argv[n]`
//! (that stays not-Allow: control below); other interpreters (`node -e`,
//! `perl -e`, which are out of the subset anyway, ruling 4); `bash -c PROG
//! arg0 args`.

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Fx {
    base: PathBuf,
    home: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

impl Fx {
    fn proj(&self) -> PathBuf {
        self.base.join("src").join("proj")
    }
}

fn fixture(tag: &str) -> Fx {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let base = neutral_base::neutral_base().join(format!(
        "py-argv-{tag}-{}",
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&base);
    let home = base.join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(base.join("src").join("proj")).unwrap();
    std::fs::create_dir_all(base.join("store")).unwrap();
    Fx { base, home }
}

fn run_as(fx: &Fx, cwd: &Path, command: &str, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "py-argv-20261118",
        "cwd": cwd,
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    if subagent {
        p["agent_id"] = serde_json::json!("test-subagent");
    }
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    cmd.env_clear()
        .env("HOME", &fx.home)
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env("CLAUDE_PROJECT_DIR", cwd)
        .env("BLASTGUARD_APPROVALS_DIR", fx.base.join("store"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut cmd, p.to_string().as_bytes(), bounded::HOOK_DEADLINE);
    assert!(
        out.status.success(),
        "hook exited {:?} for {command:?}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".into(), String::new());
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("hook stdout for {command:?} is not JSON ({e}): {stdout:?}"));
    let h = &v["hookSpecificOutput"];
    (
        h["permissionDecision"].as_str().unwrap_or("?").to_string(),
        h["permissionDecisionReason"]
            .as_str()
            .unwrap_or("")
            .to_string(),
    )
}

fn allow(tag: &str, command: &str) {
    let fx = fixture(tag);
    for subagent in [false, true] {
        let (d, r) = run_as(&fx, &fx.proj(), command, subagent);
        assert_eq!(
            d, "allow",
            "subagent={subagent}: {command:?} expected Allow, got {d} ({r})"
        );
    }
}

fn not_allow(tag: &str, command: &str) {
    let fx = fixture(tag);
    for subagent in [false, true] {
        let (d, r) = run_as(&fx, &fx.proj(), command, subagent);
        assert!(
            d == "ask" || d == "deny",
            "subagent={subagent}: {command:?} expected Ask or Deny, got {d} ({r})"
        );
    }
}

const READ_PROG: &str = r#"python3 -c "import sys;print(open(sys.argv[1]).read())""#;

// ---------------------------------------------------------------- controls

/// Control: the same read-only program with a literal argv operand is Allow.
#[test]
fn control_literal_argv_allows() {
    allow("lit", &format!("{READ_PROG} /tmp/bg-sentinel-x"));
}

/// Control (ruling 2026-11-12): an expansion INSIDE the program text stays
/// not-Allow.
/// DOES NOT PROVE: which expansions inside a program a fix may resolve.
#[test]
fn control_expansion_inside_program_not_allow() {
    not_allow("inside", r#"python3 -c "print(open('$F').read())""#);
}

/// Control: a program that WRITES an argv path it cannot resolve stays
/// not-Allow (the "open on a path blastguard cannot resolve" Ask).
#[test]
fn control_write_to_unresolved_argv_not_allow() {
    not_allow(
        "write",
        r#"python3 -c "import sys;open(sys.argv[1],'w').write('x')" "$1""#,
    );
}

// --------------------------------------------------------------------- RED

#[test]
fn argv_home_expansion_allows() {
    allow("home", &format!("{READ_PROG} \"$HOME/bg-sentinel-x\""));
}

#[test]
fn argv_line_bound_var_allows() {
    allow(
        "bound",
        &format!("D=/tmp/bg-sentinel-d; {READ_PROG} \"$D/blocks.jsonl\""),
    );
}

#[test]
fn argv_positional_allows() {
    allow("pos", &format!("{READ_PROG} \"$1\""));
}
