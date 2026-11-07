// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Audit B item 2 (memory `green-tests-prove-worktree-not-head`, 2026-11-07).
//!
//! A worker committed a RED-disabling stub (`if false && missing > 0`) and
//! left the real fix UNCOMMITTED in its worktree. Its tdd RED/GREEN proofs
//! were a valid Fail→Pass — against the working tree, not against HEAD. A
//! worktree merge integrates COMMITTED state, so the fail-open would ship.
//!
//! `condukt state set --status verified` reads the F→P proofs from the task's
//! recorded worktree but never looks at whether that worktree's tree IS its
//! HEAD. This test pins the missing refusal: a task whose worktree has
//! uncommitted changes to tracked source must not be settable to `verified`.
//! The control (same fixture, clean tree) must still verify, so the test is
//! not satisfied by a gate that refuses everything.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn tdd_bin_dir() -> PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    let profile_dir = exe
        .parent()
        .and_then(Path::parent)
        .expect("profile dir")
        .to_path_buf();
    let bin_path = profile_dir.join("tdd");
    if !bin_path.exists() {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
        let mut cmd = Command::new(&cargo);
        cmd.args(["build", "-p", "tdd", "--bin", "tdd"]);
        if profile_dir.file_name().and_then(|s| s.to_str()) == Some("release") {
            cmd.arg("--release");
        }
        assert!(cmd.status().expect("cargo build -p tdd").success());
    }
    assert!(bin_path.exists(), "no tdd at {}", bin_path.display());
    profile_dir
}

fn git(dir: &Path, args: &[&str]) {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn condukt(dir: &Path, home: &Path, tdd: &Path, args: &[&str]) -> (i32, String) {
    let mut paths = vec![tdd.to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let out = harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_condukt")))
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("PATH", std::env::join_paths(paths).unwrap())
        .env_remove("CONDUKT_DISABLE")
        .stdin(Stdio::null())
        .output()
        .expect("condukt");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Run checkout + a task worktree (a git repo) with a committed stub, valid
/// F→P proofs in `<wt>/.tdd` (ignored), and — when `dirty` — the real fix
/// left uncommitted. Returns the `state set --status verified` outcome.
fn verify(tag: &str, dirty: bool) -> (i32, String) {
    let tmp = tempfile::tempdir().unwrap();
    let run_dir = tmp.path().join("run");
    let home = tmp.path().join("home");
    let wt = tmp.path().join("wt");
    for d in [&run_dir, &home, &wt] {
        std::fs::create_dir_all(d).unwrap();
    }
    let tdd = tdd_bin_dir();

    git(&wt, &["init", "-q", "-b", "main"]);
    git(&wt, &["config", "user.email", "t@t.t"]);
    git(&wt, &["config", "user.name", "t"]);
    std::fs::write(wt.join(".gitignore"), ".tdd/\n").unwrap();
    std::fs::write(
        wt.join("gate.rs"),
        "fn gate(missing: u32) -> bool { if false && missing > 0 { return true; } false }\n",
    )
    .unwrap();
    git(&wt, &["add", "."]);
    git(&wt, &["commit", "-q", "-m", "stub"]);
    if dirty {
        // The real fix: present in the tree the tests ran on, absent from HEAD.
        std::fs::write(
            wt.join("gate.rs"),
            "fn gate(missing: u32) -> bool { if missing > 0 { return true; } false }\n",
        )
        .unwrap();
    }
    std::fs::create_dir_all(wt.join(".tdd")).unwrap();
    for (kind, passed) in [("red", false), ("green", true)] {
        std::fs::write(
            wt.join(".tdd").join(format!("t1.{kind}.json")),
            serde_json::json!({ "passed": passed, "cmd": "cargo test" }).to_string(),
        )
        .unwrap();
    }

    let dec = run_dir.join("dec.json");
    std::fs::write(
        &dec,
        serde_json::json!({"goal": "g", "tasks": [{"id": "t1", "title": "fix gate",
            "kind": "fix", "reproduction_tests": "cargo test -p demo"}]})
        .to_string(),
    )
    .unwrap();
    let run = format!("run-b02-{tag}");
    let (c, e) = condukt(
        &run_dir,
        &home,
        &tdd,
        &[
            "state",
            "init",
            "--run",
            &run,
            "--file",
            dec.to_str().unwrap(),
        ],
    );
    assert_eq!(c, 0, "state init: {e}");
    condukt(
        &run_dir,
        &home,
        &tdd,
        &[
            "state",
            "set",
            "--run",
            &run,
            "--task",
            "t1",
            "--status",
            "verified",
            "--worktree",
            wt.to_str().unwrap(),
        ],
    )
}

/// Control: a clean task worktree with a valid F→P oracle verifies.
#[test]
fn clean_worktree_with_valid_oracle_verifies() {
    let (code, stderr) = verify("clean", false);
    assert_eq!(code, 0, "control must verify; stderr: {stderr}");
}

/// RED: the committed tree is not the tree the green proof ran on.
#[test]
fn verify_refuses_task_worktree_with_uncommitted_source_changes() {
    let (code, stderr) = verify("dirty", true);
    assert_ne!(
        code, 0,
        "state set --status verified accepted a task whose worktree has an UNCOMMITTED \
         change to tracked source (gate.rs): the green proof describes the working tree, \
         not HEAD, so the merge would integrate the committed stub. stderr: {stderr}"
    );
}
