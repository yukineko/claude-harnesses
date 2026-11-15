#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (observed 2026-11-14, run run-20261114-163341-69351, task e70b6a71):
//! a retry worktree created with a SUFFIXED topic
//! (`condukt worktree create --run R --topic <task>-r1 --branch condukt/<task>-r1`)
//! records branch identity `R/<task>-r1` (worktree.rs create_namespaced:
//! `record_task_identity(.., "{run}/{topic}")`), a task the run does not have.
//! Consequences seen live: `worktree merge --run R --task <task>` refused with
//! merge-task-mismatch, and `worktree remove` of the worktree of a VERIFIED
//! task refuses with
//!   "cannot determine whether the task that owns it is verified (branch
//!   identity names task '<task>-r1' of run 'R', which that run does not have)"
//! (teardown_gate.rs refuse_unverified_teardown).
//!
//! Two readings (this test does not choose between them):
//!  (a) binary defect: the teardown gate should fall back to the task whose
//!      recorded `worktree` is this path (state set --worktree) when the branch
//!      identity names no such task; or `worktree create` should not mint an
//!      identity for a task the run lacks.
//!  (b) skill/operator defect: SKILL.md (Phase 8 retry, "新しい per-task
//!      worktree") does not say how to name a retry worktree; the correct
//!      invocation keeps `--topic <task>` and some other way to avoid the
//!      path collision. Distinguish: if there is a documented invocation that
//!      yields a fresh worktree whose identity is `R/<task>` for a retry, (b);
//!      if no such invocation exists, (a).
//!
//! Asserted: the verified task whose recorded worktree is that retry worktree
//! can have it removed. DOES NOT PROVE: which reading is right; the merge
//! refusal (only the remove refusal is reproduced).

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
        "git {args:?} failed: {}",
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
        .env("CLAUDE_CODE_SESSION_ID", "sess-retry-suffix-20261114")
        .env("CONDUKT_DEFAULT_BRANCH", "main")
        .env_remove("CONDUKT_DISABLE")
        .output()
        .expect("spawn condukt")
}

fn ok(out: &Output, what: &str) {
    assert_eq!(
        out.status.code(),
        Some(0),
        "{what}: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

// test-weakening-justified: new RED for an unfixed defect (backlog pending,
// 2026-11-14); the ignore is removed in the fixing commit.
#[test]
#[ignore = "RED: <backlog-pending> 2026-11-14 verified task's retry worktree (suffixed topic) cannot be removed; remove the ignore when fixed"]
fn verified_task_with_suffixed_topic_retry_worktree_can_be_removed() {
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
        r#"{"goal":"g","tasks":[
            {"id":"e70b6a71","title":"a","kind":"fix","reproduction_tests":"cargo test -p x","touched_files":["a.txt"],"deps":[],"class":"parallel","done_criteria":"d"}]}"#,
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
                "runR",
                "--file",
                dec.to_str().unwrap(),
            ],
        ),
        "state init",
    );

    // The retry worktree, created the way the live run did it.
    let out = condukt(
        &repo,
        &home,
        &bin,
        &[
            "worktree",
            "create",
            "--run",
            "runR",
            "--topic",
            "e70b6a71-r1",
            "--branch",
            "condukt/e70b6a71-r1",
        ],
    );
    ok(&out, "worktree create (suffixed topic)");
    let wt = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(Path::new(&wt).is_dir(), "created worktree {wt} exists");

    ok(
        &condukt(
            &repo,
            &home,
            &bin,
            &[
                "state",
                "set",
                "--run",
                "runR",
                "--task",
                "e70b6a71",
                "--status",
                "done",
                "--worktree",
                &wt,
            ],
        ),
        "state set done",
    );
    ok(
        &condukt(
            &repo,
            &home,
            &bin,
            &[
                "state", "set", "--run", "runR", "--task", "e70b6a71", "--status", "verified",
            ],
        ),
        "state set verified (task is verified with this worktree recorded)",
    );

    let rm = condukt(
        &repo,
        &home,
        &bin,
        &[
            "worktree",
            "remove",
            "--path",
            &wt,
            "--branch",
            "condukt/runR/e70b6a71-r1",
        ],
    );
    assert_eq!(
        rm.status.code(),
        Some(0),
        "verified task's recorded worktree must be removable: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&rm.stdout),
        String::from_utf8_lossy(&rm.stderr)
    );
}
