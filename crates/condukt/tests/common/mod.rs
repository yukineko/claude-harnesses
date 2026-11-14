//! Shared fixture helper: the testscope session marker that `condukt state set
//! --status running` requires (user ruling 2026-11-14, rule A in
//! `src/testscope.rs`).
//!
//! A fixture that moves a task to `running` must (1) point
//! `CONDUKT_TESTSCOPE_LEDGER` at a temp location, (2) pin
//! `CLAUDE_CODE_SESSION_ID`, and (3) have the marker for that session. The
//! marker is produced the public way — one harmless PreToolUse(Bash) payload
//! handed to `condukt testscope` — so these fixtures never depend on where the
//! implementation keeps it.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The session id fixtures pin when they do not name their own.
pub const TESTSCOPE_SESSION: &str = "sess-testscope-fixture";

/// The testscope ledger for a fixture whose `$HOME` is `home` (a temp dir).
pub fn testscope_ledger(home: impl AsRef<Path>) -> PathBuf {
    home.as_ref().join("testscope-fixture").join("ledger.tsv")
}

/// Run `condukt testscope` once with a harmless payload carrying `session`, so
/// the hook records that it is loaded in that session.
pub fn prime_testscope(home: impl AsRef<Path>, session: &str) {
    let home = home.as_ref();
    let payload = serde_json::json!({
        "session_id": session,
        "tool_name": "Bash",
        "tool_input": {"command": "true"},
        "cwd": home.to_str().unwrap(),
    });
    let mut child = harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_condukt")))
        .arg("testscope")
        .current_dir(home)
        .env("HOME", home)
        .env("CONDUKT_TESTSCOPE_LEDGER", testscope_ledger(home))
        .env_remove("CONDUKT_DISABLE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn condukt testscope");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success() && out.stdout.is_empty(),
        "fixture: priming the testscope session marker failed: {out:?}"
    );
}

/// [`prime_testscope`] when `args` is a `state set` that moves a task to
/// `running`; a no-op for every other invocation.
pub fn prime_if_running(args: &[&str], home: impl AsRef<Path>, session: &str) {
    if is_running_transition(args) {
        prime_testscope(home, session);
    }
}

/// Whether `args` is a `state set` that moves a task to `running`.
pub fn is_running_transition(args: &[&str]) -> bool {
    let running = args
        .iter()
        .any(|a| a.eq_ignore_ascii_case("running") || a.eq_ignore_ascii_case("--status=running"));
    running && args.contains(&"set")
}
