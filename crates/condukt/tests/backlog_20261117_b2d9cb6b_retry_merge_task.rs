#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Issue b2d9cb6b (fix commit 5b2905e3f): a retry worktree created with a
//! suffixed topic (`<task>-r1`) now has its recorded task identity resolved
//! to the real task. This test covers the merge half: `worktree merge --task
//! <task>` must succeed for a retry worktree whose topic is `<task>-r1`.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

fn run_git(dir: &Path, args: &[&str]) {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

fn condukt(repo: &Path, home: &Path, bin: &Path, args: &[&str]) -> Output {
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_condukt")))
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("PATH", path)
        .env("CLAUDE_CODE_SESSION_ID", "sess-b2d9cb6b-merge-20261117")
        .env("CONDUKT_DEFAULT_BRANCH", "main")
        .env_remove("CONDUKT_DISABLE")
        .output()
        .expect("spawn condukt")
}

fn ok(out: &Output, what: &str) {
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}: stdout={:?} stderr={:?}",
        what,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The real `tdd` binary next to this test's profile dir (built on demand).
/// The mock `tdd` on condukt's PATH is left as it was.
fn real_tdd() -> std::path::PathBuf {
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
    bin_path
}

const SCRIPT: &str = "mode=none; [ -f mode.txt ] && mode=$(cat mode.txt)
case $mode in
  none) echo 'test m::a ... ok'; echo 'test m::b ... FAILED'; echo 'test result: FAILED. 1 passed; 1 failed; 0 ignored'; exit 1;;
  good) echo 'test m::a ... ok'; echo 'test m::b ... ok'; echo 'test result: ok. 2 passed; 0 failed; 0 ignored'; exit 0;;
esac
";

/// Real `tdd red` (mode none) then `tdd green` (mode good) for `task` in
/// `wt`, committing `t.sh` / `mode.txt` with `Condukt-Task: <identity>`
/// before each phase.
fn record_real_proofs(wt: &Path, home: &Path, task: &str, identity: &str) {
    let tdd = |phase: &str| {
        let out = harness_core::git_env::scrub(&mut Command::new(real_tdd()))
            .args([phase, "--task", task, "--cmd", "sh t.sh"])
            .current_dir(wt)
            .env("HOME", home)
            .env("CLAUDE_CODE_SESSION_ID", "sess-b2d9cb6b-merge-20261117")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .expect("spawn tdd");
        assert!(
            out.status.success(),
            "tdd {phase}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    };
    let trailer = format!("Condukt-Task: {identity}");
    let commit = |msg: &str| {
        run_git(wt, &["add", "t.sh", "mode.txt"]);
        run_git(wt, &["commit", "-q", "-m", msg, "--trailer", &trailer]);
    };
    std::fs::write(wt.join("t.sh"), SCRIPT).unwrap();
    std::fs::write(wt.join("mode.txt"), "none").unwrap();
    commit("add test");
    tdd("red");
    std::fs::write(wt.join("mode.txt"), "good").unwrap();
    commit("make it pass");
    tdd("green");
}

#[test]
fn retry_worktree_merge_with_resolved_task_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let home = tmp.path().join("home");
    let bin = tmp.path().join("bin");
    for d in [&repo, &home, &bin] {
        std::fs::create_dir_all(d).unwrap();
    }

    // Create a mock tdd binary that returns success
    let tdd = bin.join("tdd");
    std::fs::write(
        &tdd,
        "#!/bin/sh\necho '{\"has_green\":true,\"has_red\":true,\"transition\":\"fail_to_pass\",\"valid_fp_oracle\":true}'\nexit 0\n",
    )
    .unwrap();
    std::fs::set_permissions(&tdd, std::fs::Permissions::from_mode(0o755)).unwrap();

    // Initialize git repo
    run_git(&repo, &["init", "-q", "-b", "main"]);
    run_git(&repo, &["config", "user.email", "t@t.t"]);
    run_git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("base.txt"), "base\n").unwrap();
    run_git(&repo, &["add", "."]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);

    // Create a run with one task
    let dec = tmp.path().join("dec.json");
    std::fs::write(
        &dec,
        r#"{"goal":"g","tasks":[{"id":"task123","title":"a","kind":"fix","reproduction_tests":"cargo test -p x","touched_files":["a.txt"],"deps":[],"class":"parallel","done_criteria":"d"}]}"#,
    )
    .unwrap();

    ok(
        &condukt(
            &repo,
            &home,
            &bin,
            &[
                "state",
                "init",
                "--run",
                "runX",
                "--file",
                dec.to_str().unwrap(),
            ],
        ),
        "state init",
    );

    // Create a retry worktree with --topic task123-r1
    let out = condukt(
        &repo,
        &home,
        &bin,
        &[
            "worktree",
            "create",
            "--run",
            "runX",
            "--topic",
            "task123-r1",
            "--branch",
            "condukt/task123-r1",
        ],
    );
    ok(&out, "worktree create (suffixed topic)");
    let wt = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(Path::new(&wt).is_dir(), "created worktree {wt} exists");

    // Make a commit in the worktree
    let work_file = Path::new(&wt).join("work.txt");
    std::fs::write(&work_file, "work\n").unwrap();
    run_git(Path::new(&wt), &["add", "work.txt"]);
    run_git(
        Path::new(&wt),
        &[
            "commit",
            "-q",
            "-m",
            "do work",
            "--trailer",
            "Condukt-Task: runX/task123",
        ],
    );

    // Re-anchored per user ruling 2026-11-20 (spec
    // condukt-merge-proof-contract-7768aeaf R8, round 4 Q-B): task123 is a
    // `fix` task, so the merge now requires real tdd proofs (proof contract
    // 7768aeaf). Record a real RED -> GREEN with the files tdd runs on
    // committed before GREEN, so GREEN's tree is the branch tip's tree. The
    // kind stays `fix`; the merge assert below is unchanged.
    record_real_proofs(Path::new(&wt), &home, "task123", "runX/task123");

    // The fix: worktree merge --run runX --task task123 --branch condukt/runX/task123-r1 should succeed
    // (before the fix, it would fail with merge-task-mismatch because the recorded
    // identity was runX/task123-r1, which doesn't match task123)
    let merge = condukt(
        &repo,
        &home,
        &bin,
        &[
            "worktree",
            "merge",
            "--branch",
            "condukt/runX/task123-r1",
            "--run",
            "runX",
            "--task",
            "task123",
        ],
    );
    assert_eq!(
        merge.status.code(),
        Some(0),
        "merge of retry worktree must succeed after identity resolution: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&merge.stdout),
        String::from_utf8_lossy(&merge.stderr)
    );
}

#[test]
fn unknown_topic_worktree_merge_still_refuses() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let home = tmp.path().join("home");
    let bin = tmp.path().join("bin");
    for d in [&repo, &home, &bin] {
        std::fs::create_dir_all(d).unwrap();
    }

    let tdd = bin.join("tdd");
    std::fs::write(
        &tdd,
        "#!/bin/sh\necho '{\"has_green\":true,\"has_red\":true,\"transition\":\"fail_to_pass\",\"valid_fp_oracle\":true}'\nexit 0\n",
    )
    .unwrap();
    std::fs::set_permissions(&tdd, std::fs::Permissions::from_mode(0o755)).unwrap();

    run_git(&repo, &["init", "-q", "-b", "main"]);
    run_git(&repo, &["config", "user.email", "t@t.t"]);
    run_git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("base.txt"), "base\n").unwrap();
    run_git(&repo, &["add", "."]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);

    let dec = tmp.path().join("dec.json");
    std::fs::write(
        &dec,
        r#"{"goal":"g","tasks":[{"id":"task456","title":"b","kind":"fix","reproduction_tests":"cargo test -p x","touched_files":["b.txt"],"deps":[],"class":"parallel","done_criteria":"d"}]}"#,
    )
    .unwrap();

    ok(
        &condukt(
            &repo,
            &home,
            &bin,
            &[
                "state",
                "init",
                "--run",
                "runY",
                "--file",
                dec.to_str().unwrap(),
            ],
        ),
        "state init",
    );

    // Create a worktree with a topic that doesn't map to any task
    let out = condukt(
        &repo,
        &home,
        &bin,
        &[
            "worktree",
            "create",
            "--run",
            "runY",
            "--topic",
            "nope-r1",
            "--branch",
            "condukt/nope-r1",
        ],
    );
    ok(&out, "worktree create (unknown topic)");
    let wt = String::from_utf8_lossy(&out.stdout).trim().to_string();

    // Make a commit in the worktree
    let work_file = Path::new(&wt).join("work.txt");
    std::fs::write(&work_file, "work\n").unwrap();
    run_git(Path::new(&wt), &["add", "work.txt"]);
    run_git(
        Path::new(&wt),
        &[
            "commit",
            "-q",
            "-m",
            "do work",
            "--trailer",
            "Condukt-Task: runY/nope",
        ],
    );

    // Merge should refuse: nope-r1 stays unmapped (nope is not a task of runY)
    let merge = condukt(
        &repo,
        &home,
        &bin,
        &[
            "worktree",
            "merge",
            "--branch",
            "condukt/runY/nope-r1",
            "--run",
            "runY",
            "--task",
            "task456",
        ],
    );
    assert_ne!(
        merge.status.code(),
        Some(0),
        "merge must refuse when topic doesn't map to run's tasks: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&merge.stdout),
        String::from_utf8_lossy(&merge.stderr)
    );
}
