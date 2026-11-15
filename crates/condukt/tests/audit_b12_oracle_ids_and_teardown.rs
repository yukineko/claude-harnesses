// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Audit B item 12 (memory `condukt-operator-gotchas`, section "F→P oracle:
//! decomposition task id, cwd, teardown order"; originally
//! `condukt-fp-oracle-task-id`), 2026-11-07.
//!
//! Two operator traps the memory asks to mechanize:
//!
//! 1. Proofs recorded under the BACKLOG id instead of the decomposition id
//!    are invisible to the gate. The refusal says "no valid fail-to-pass
//!    transition" but never names the proof ids it did find, so the operator
//!    cannot see the mislabel. RED: the refusal must name the `.tdd/*.red.json`
//!    id present in the worktree.
//! 2. Removing the task worktree before verifying deletes the proofs the
//!    oracle reads (ENOENT afterwards). RED: `condukt worktree remove` must
//!    refuse a worktree that run state records for a task that is not yet
//!    verified. Control: the same remove proceeds once the task is verified.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

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

fn git(dir: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn condukt(dir: &Path, home: &Path, args: &[&str]) -> Output {
    common::prime_if_running(args, home, "sess-b12");
    let mut paths = vec![tdd_bin_dir()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_condukt")))
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("CONDUKT_TESTSCOPE_LEDGER", common::testscope_ledger(home))
        .env("PATH", std::env::join_paths(paths).unwrap())
        .env("CLAUDE_CODE_SESSION_ID", "sess-b12")
        .env("CONDUKT_DEFAULT_BRANCH", "main")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("CONDUKT_DISABLE")
        .stdin(Stdio::null())
        .output()
        .expect("condukt")
}

fn ok(out: &Output, what: &str) -> String {
    assert!(
        out.status.success(),
        "{what}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn init(dir: &Path, home: &Path, run: &str, kind: Option<&str>) {
    let mut task = serde_json::json!({"id": "t1", "title": "fix it"});
    if let Some(k) = kind {
        task["kind"] = k.into();
        task["reproduction_tests"] = "cargo test -p demo".into();
    }
    let dec = dir.join("dec.json");
    std::fs::write(
        &dec,
        serde_json::json!({"goal": "g", "tasks": [task]}).to_string(),
    )
    .unwrap();
    let out = condukt(
        dir,
        home,
        &[
            "state",
            "init",
            "--run",
            run,
            "--file",
            dec.to_str().unwrap(),
        ],
    );
    ok(&out, "state init");
}

/// RED 1: proofs exist under the backlog id; the refusal must say so.
#[test]
fn oracle_refusal_names_the_proof_ids_it_found() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("run");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(dir.join(".tdd")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    init(&dir, &home, "run-b12-ids", Some("fix"));
    for (kind, passed) in [("red", false), ("green", true)] {
        std::fs::write(
            dir.join(".tdd").join(format!("bl-9f3a17c2.{kind}.json")),
            serde_json::json!({"passed": passed, "cmd": "cargo test"}).to_string(),
        )
        .unwrap();
    }
    let out = condukt(
        &dir,
        &home,
        &[
            "state",
            "set",
            "--run",
            "run-b12-ids",
            "--task",
            "t1",
            "--status",
            "verified",
        ],
    );
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    // Control: it IS refused (proofs under the wrong id are no proof).
    assert!(
        !out.status.success() && stderr.contains("refusing to verify"),
        "precondition: the mislabelled proofs are refused; stderr: {stderr}"
    );
    assert!(
        stderr.contains("bl-9f3a17c2"),
        "the oracle refusal does not name the proof id it found in .tdd/ \
         (bl-9f3a17c2.red.json), so a backlog-id mislabel is invisible. stderr: {stderr}"
    );
}

/// Repo + a task worktree created through the CLI and recorded on the task.
fn worktree_fixture(tmp: &Path, run: &str) -> (PathBuf, PathBuf, PathBuf) {
    let repo = tmp.join("repo");
    let home = tmp.join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@t.t"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("base.txt"), "base\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    init(&repo, &home, run, None);
    let wt = PathBuf::from(ok(
        &condukt(
            &repo,
            &home,
            &[
                "worktree", "create", "--run", run, "--topic", "t1", "--branch", "t1",
            ],
        ),
        "worktree create",
    ));
    ok(
        &condukt(
            &repo,
            &home,
            &[
                "state",
                "set",
                "--run",
                run,
                "--task",
                "t1",
                "--status",
                "running",
                "--worktree",
                wt.to_str().unwrap(),
            ],
        ),
        "state set running",
    );
    (repo, home, wt)
}

fn remove(repo: &Path, home: &Path, wt: &Path) -> Output {
    condukt(
        repo,
        home,
        &["worktree", "remove", "--path", wt.to_str().unwrap()],
    )
}

/// Control: once the task is verified, its worktree may be torn down.
#[test]
fn worktree_remove_of_a_verified_task_proceeds() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, home, wt) = worktree_fixture(tmp.path(), "run-b12-ver");
    ok(
        &condukt(
            &repo,
            &home,
            &[
                "state",
                "set",
                "--run",
                "run-b12-ver",
                "--task",
                "t1",
                "--status",
                "verified",
            ],
        ),
        "state set verified",
    );
    let out = remove(&repo, &home, &wt);
    assert!(
        out.status.success(),
        "control must remove; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!wt.exists(), "control: worktree dir is gone");
}

/// RED 2: tearing down an unverified task's worktree destroys its proofs.
#[test]
fn worktree_remove_refuses_while_the_task_is_unverified() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, home, wt) = worktree_fixture(tmp.path(), "run-b12-unv");
    let out = remove(&repo, &home, &wt);
    assert!(
        !out.status.success() && wt.exists(),
        "condukt worktree remove deleted the worktree of task t1 (status running, \
         not verified); the .tdd proofs the oracle reads went with it. exit {:?}, \
         exists afterwards: {}, stderr: {}",
        out.status.code(),
        wt.exists(),
        String::from_utf8_lossy(&out.stderr)
    );
}
