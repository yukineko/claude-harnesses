#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (observed 2026-11-15, measuring point main a8e92b562, condukt
//! 0.7.413): the testscope rerun guard's tree identity covers the CONTENTS of
//! tracked changes but only the NAMES of untracked files.
//!
//! Mechanism (crates/condukt/src/testscope.rs `tree_identity`): the dirty hash
//! is `hash(git diff HEAD --binary) + hash(git ls-files --others
//! --exclude-standard)`. The second command lists untracked file names only,
//! so editing an untracked file (typically a brand-new test file that has not
//! been `git add`ed yet) leaves the identity unchanged, and the identical test
//! command is refused as "already ran on this exact tree" although the tree
//! under test differs. The same `tree_identity` body is on the unmerged
//! branch fix/testscope-cwd-20261115 (HEAD 501535dac), which fixes only which
//! directory is fingerprinted (the `cd` target), not this.
//!
//! The function's own docstring says it identifies "the tree a test runs
//! against"; an untracked file's contents are part of that tree (cargo
//! compiles an untracked `tests/*.rs`).
//!
//! Control (GREEN at HEAD, not ignored): editing a TRACKED file allows the
//! rerun, and an unchanged tree still refuses it.
//!
//! DOES NOT PROVE: gitignored files, or how the identity should be computed.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) {
    let st = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?} failed");
}

fn repo() -> TempDir {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q"]);
    std::fs::write(d.path().join("a.txt"), "one\n").unwrap();
    git(d.path(), &["add", "a.txt"]);
    git(d.path(), &["commit", "-q", "-m", "init"]);
    d
}

struct Fx {
    home: TempDir,
    ledger_dir: TempDir,
    tree: TempDir,
}

impl Fx {
    fn new() -> Self {
        let fx = Fx {
            home: tempfile::tempdir().unwrap(),
            ledger_dir: tempfile::tempdir().unwrap(),
            tree: repo(),
        };
        // A new, untracked test file (never `git add`ed).
        std::fs::write(fx.tree.path().join("new_test.rs"), "fn v1() {}\n").unwrap();
        fx
    }
    fn ledger(&self) -> PathBuf {
        self.ledger_dir.path().join("ledger.tsv")
    }
    fn go(&self) -> String {
        let cwd = self.tree.path();
        let payload = serde_json::json!({
            "session_id": "s",
            "tool_name": "Bash",
            "tool_input": {"command": "cargo test -p foo --test new_test"},
            "cwd": cwd.to_str().unwrap(),
        });
        let mut child = Command::new(env!("CARGO_BIN_EXE_condukt"))
            .arg("testscope")
            .env("HOME", self.home.path())
            .env("CONDUKT_TESTSCOPE_LEDGER", self.ledger())
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

fn denied(stdout: &str) -> bool {
    stdout.contains("\"deny\"")
}

// test-weakening-justified: new RED for an unfixed defect (backlog row filed
// 2026-11-15 with this test as its repro); the ignore is removed in the fixing
// commit.
#[test]
#[ignore = "RED: 2026-11-15 testscope tree identity hashes untracked file names, not contents — remove the ignore when fixed"]
fn rerun_after_editing_an_untracked_file_is_allowed() {
    let fx = Fx::new();
    let first = fx.go();
    assert!(!denied(&first), "first run must be allowed: {first}");
    // The untracked test file's contents change; its name does not.
    std::fs::write(fx.tree.path().join("new_test.rs"), "fn v2() {}\n").unwrap();
    let second = fx.go();
    assert!(
        !denied(&second),
        "an untracked file's contents changed but the rerun was refused \
         (identity hashes untracked names only): {second}"
    );
}

// Control: editing a tracked file changes the identity.
#[test]
fn control_editing_a_tracked_file_allows_rerun() {
    let fx = Fx::new();
    assert!(!denied(&fx.go()));
    std::fs::write(fx.tree.path().join("a.txt"), "two\n").unwrap();
    let second = fx.go();
    assert!(!denied(&second), "{second}");
}

// Control: nothing changed, identical command: refused.
#[test]
fn control_unchanged_tree_refuses_rerun() {
    let fx = Fx::new();
    assert!(!denied(&fx.go()));
    let second = fx.go();
    assert!(denied(&second), "same tree, same command: {second}");
}
