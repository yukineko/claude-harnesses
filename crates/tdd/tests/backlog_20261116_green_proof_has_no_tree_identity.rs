#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro: a `tdd green` proof carries no tree / commit identifier, so a
//! change made AFTER GREEN was recorded is still reported as a valid Fail->Pass
//! oracle by `tdd oracle`.
//!
//! Observation (2026-11-16, condukt run-20261116-164820-25374 task t4):
//! `.tdd/t4.green.json` had exactly the keys ["author","cmd","exit_code",
//! "passed","phase","red_proof","task","ts"]. The worker changed a caller
//! after recording GREEN, and `condukt state check-oracle` still returned
//! `valid_fp_oracle:true`.
//!
//! Two readings:
//!   (a) defect: the proof does not say which tree it saw, so "GREEN, then
//!       more edits" passes as verified. Asserted here.
//!   (b) intended: the oracle looks only at the RED->GREEN transition
//!       (design: transitions, not trees).
//! How to tell them apart: under (b) nobody can check that the GREEN proof
//! corresponds to the final commit, because the proof holds nothing to
//! compare against. Unless the proof carries a tree id, that correspondence
//! is uncheckable by anyone. This test accepts EITHER fix: the oracle stops
//! returning valid_fp_oracle:true after the tree changed, OR the proof holds
//! a tree/commit identifier (any of the keys tree, tree_sha, commit, head,
//! rev) that differs from the current tree.
//!
//! Shape: temp git repo; `tdd red` (script fails: impl.txt absent) -> create
//! impl.txt -> `tdd green` (script passes) -> modify an unrelated tracked file
//! -> `tdd oracle`. Controls: the RED and GREEN steps succeed, and before the
//! extra edit the oracle is valid (exit 0, valid_fp_oracle:true), so a false
//! RED cannot come from a broken setup.
//!
//! DOES NOT PROVE: the RED proof's identifier; that the identifier is
//! computed correctly (dirty tree vs HEAD); that the identifier, when present,
//! actually DIFFERS from the current tree (the test only checks that a key
//! exists); the `condukt state check-oracle` wrapper (only `tdd oracle` is
//! run); behaviour for committed (not just working-tree) changes after GREEN.
//!
//! Written by an independent test author, not the implementer.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Dir(PathBuf);
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tdd(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_tdd")))
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .stdin(Stdio::null())
        .output()
        .expect("tdd runs")
}

fn git(cwd: &Path, args: &[&str]) {
    let o = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git runs");
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

fn s(o: &Output) -> String {
    format!(
        "exit={:?}\nstdout: {}\nstderr: {}",
        o.status.code(),
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn valid(o: &Output) -> bool {
    let out = String::from_utf8_lossy(&o.stdout);
    o.status.code() == Some(0)
        && (out.contains("\"valid_fp_oracle\":true") || out.contains("\"valid_fp_oracle\": true"))
}

#[test]
fn oracle_detects_change_made_after_green() {
    let d = Dir(std::env::temp_dir().join(format!("tdd-green-tree-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&d.0);
    let home = d.0.join("home");
    let work = d.0.join("work");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    git(&work, &["init", "-q", "-b", "main"]);
    git(&work, &["config", "user.email", "t@example.invalid"]);
    git(&work, &["config", "user.name", "t"]);
    std::fs::write(work.join("caller.txt"), "v1\n").unwrap();
    git(&work, &["add", "caller.txt"]);
    git(&work, &["commit", "-q", "--no-verify", "-m", "init"]);

    // Test command: fails until impl.txt exists, then passes.
    let cmd = "test -f impl.txt";

    let red = tdd(&home, &work, &["red", "--task", "t1", "--cmd", cmd]);
    assert_eq!(
        red.status.code(),
        Some(0),
        "control: RED recorded: {}",
        s(&red)
    );
    std::fs::write(work.join("impl.txt"), "impl\n").unwrap();
    let green = tdd(&home, &work, &["green", "--task", "t1", "--cmd", cmd]);
    assert_eq!(
        green.status.code(),
        Some(0),
        "control: GREEN recorded: {}",
        s(&green)
    );

    let before = tdd(&home, &work, &["oracle", "--task", "t1"]);
    assert!(
        valid(&before),
        "control: oracle valid before the extra edit: {}",
        s(&before)
    );

    // The change after GREEN: a caller is modified.
    std::fs::write(work.join("caller.txt"), "v2 changed after GREEN\n").unwrap();

    let after = tdd(&home, &work, &["oracle", "--task", "t1"]);
    let green_json = std::fs::read_to_string(work.join(".tdd").join("t1.green.json")).unwrap();
    let keys = [
        "\"tree\"",
        "\"tree_sha\"",
        "\"commit\"",
        "\"head\"",
        "\"rev\"",
    ];
    let has_tree_id = keys.iter().any(|k| green_json.contains(k));

    assert!(
        !valid(&after) || has_tree_id,
        "GREEN proof has no tree/commit identifier and tdd oracle still says valid_fp_oracle:true after a change made post-GREEN.\ngreen proof: {green_json}\noracle: {}",
        s(&after)
    );
}
