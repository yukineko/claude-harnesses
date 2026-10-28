// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Reproduction: blastguard's integration-test harness writes into the REAL
//! user's overwatch violation registry (`$HOME/.overwatch/<project_key>/...`).
//!
//! Every Deny/Ask goes through `record_violation` in `src/main.rs`, which calls
//! `overwatch::store::append_violation(&cwd, ..)`. The store root is
//! `harness_core::config::base_dir("overwatch")`, i.e. `$HOME/.overwatch`,
//! falling back to `dirs::home_dir()` when `HOME` is unset. So:
//!
//! * `tests/ask_hardening_invariant.rs` uses `Command::env_clear()` and
//!   documents that as "env hygiene" -- but with no `HOME` the child falls back
//!   to the passwd home, i.e. the real one. `env_clear` does NOT isolate it.
//! * `tests/integration.rs` (and others) inherit `HOME` unchanged.
//!
//! These tests spawn the binary exactly that way, but from a throwaway `git
//! init`'d temp repo (so they never touch the live harness registry key), and
//! assert the test-isolation property: no directory for that temp repo appears
//! under the real `~/.overwatch`. On current code they are RED.
//!
//! Anything they do create under the real `~/.overwatch` is removed by a drop
//! guard -- only the exact project-key dir derived from the temp repo.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[path = "support/git_env.rs"]
mod git_env;

/// Same Ask-producing command as `ask_hardening_invariant.rs`.
const ASK_PRODUCING_COMMAND: &str = "my-cleanup-wrapper rm -rf /some/path";

fn ask_payload() -> String {
    // No `cwd` in the payload, same as ask_hardening_invariant.rs: the binary
    // falls back to its process cwd.
    format!(
        r#"{{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{{"command":"{ASK_PRODUCING_COMMAND}"}}}}"#
    )
}

/// The real user home as seen by the test runner (the parent of the spawned
/// child). An absent/empty HOME is undetermined -> panic, never a vacuous pass.
fn real_home() -> PathBuf {
    let h = std::env::var("HOME").expect("test runner HOME must be set to locate the real home");
    assert!(
        !h.is_empty(),
        "test runner HOME is empty: cannot locate the real home"
    );
    PathBuf::from(h)
}

/// Removes exactly the project-key dir derived from the temp repo, and only if
/// it sits directly under `<real home>/.overwatch`.
struct LiveRegistryCleanup {
    overwatch_base: PathBuf,
    key_dir: PathBuf,
}

impl Drop for LiveRegistryCleanup {
    fn drop(&mut self) {
        if self.key_dir.parent() == Some(self.overwatch_base.as_path())
            && self.key_dir.file_name().is_some()
            && self.key_dir.exists()
        {
            if let Err(e) = std::fs::remove_dir_all(&self.key_dir) {
                eprintln!(
                    "WARNING: could not remove test residue {}: {e}",
                    self.key_dir.display()
                );
            }
        }
    }
}

fn git_init(dir: &Path) {
    let st = git_env::scrub(&mut Command::new("git"))
        .arg("init")
        .arg("-q")
        .arg(dir)
        .status()
        .expect("git init spawns");
    assert!(st.success(), "git init failed in {}", dir.display());
}

/// Spawn blastguard from `repo` with either a cleared env (no HOME) or the
/// inherited env, feed the Ask payload, and return its permissionDecision.
fn run_blastguard(repo: &Path, clear_env: bool) -> String {
    let bin = env!("CARGO_BIN_EXE_blastguard");
    let mut cmd = Command::new(bin);
    if clear_env {
        cmd.env_clear();
    } else {
        git_env::scrub(&mut cmd);
    }
    cmd.current_dir(repo)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove("CLAUDE_CODE_SESSION_ID");
    let mut child = cmd.spawn().expect("binary spawns");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(ask_payload().as_bytes())
        .expect("write payload");
    let out = child.wait_with_output().expect("binary runs");
    assert_eq!(out.status.code(), Some(0), "hook exits 0");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("expected hook JSON, got {stdout:?}: {e}"));
    v["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap_or_else(|| panic!("no permissionDecision in {v}"))
        .to_string()
}

fn assert_no_live_registry_write(clear_env: bool) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().to_path_buf();
    git_init(&repo);

    let overwatch_base = real_home().join(".overwatch");
    let key = harness_core::projkey::project_key(&repo);
    let key_dir = overwatch_base.join(&key);
    // Precondition: the key is fresh, so anything there afterwards is ours.
    assert!(
        !key_dir.exists(),
        "precondition: {} already exists before the run",
        key_dir.display()
    );
    let _cleanup = LiveRegistryCleanup {
        overwatch_base: overwatch_base.clone(),
        key_dir: key_dir.clone(),
    };

    let decision = run_blastguard(&repo, clear_env);
    // Guard against a vacuous pass: the payload must actually block (Ask or
    // its hardened Deny), i.e. reach record_violation.
    assert!(
        decision == "ask" || decision == "deny",
        "payload did not block (got {decision:?}); the test would observe nothing"
    );

    let violations = key_dir.join("overwatch").join("violations.jsonl");
    let lines = std::fs::read_to_string(&violations)
        .map(|s| s.lines().count())
        .unwrap_or(0);
    assert!(
        !key_dir.exists(),
        "test isolation broken (env_clear={clear_env}): spawning blastguard from temp repo {} \
         wrote into the REAL user registry {} (violations.jsonl lines: {lines})",
        repo.display(),
        key_dir.display()
    );
}

#[test]
fn env_clear_child_does_not_write_real_overwatch_registry() {
    assert_no_live_registry_write(true);
}

#[test]
fn inherited_home_child_does_not_write_real_overwatch_registry() {
    assert_no_live_registry_write(false);
}
