#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog 02e5a922 — `condukt state set --status done` took >2 minutes with
//! successive `git diff <base>...HEAD` children and no output.
//!
//! Measured root cause (2026-10-15): `worktree::run_git_bounded_with` waits for
//! git to EXIT (`wait_timeout`, 45s) before it starts reading git's stdout pipe.
//! A diff larger than the pipe buffer blocks git on write, so git never exits,
//! the 45s timeout fires, and `diffrisk_record::worktree_diff` retries the next
//! fallback form — the same deadlock again. The task's diff is then reported
//! as "NOT INSPECTED (no-diff)" although it is a large, real diff. (Fixed by
//! c00c3506, 0.7.269.)
//!
//! Second cause, measured 2026-10-20 at 59b9fa02 (this test still took 318s
//! with no git child alive; `sample` put the time in
//! `harness_core::callgraph::enumerate_callers` under
//! `record_post_execution_diff_risk`): the caller scan re-extracted symbols and
//! rescanned every line once per changed name — 8000 names here. Fixed by the
//! single-pass scan in harness-core 0.2.62.
//!
//! Pinned: with a ~400 KB committed diff, `state set --status done` returns
//! well inside one git timeout, and does not call the diff "no-diff".

use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

fn git(dir: &Path, args: &[&str]) {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t.t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t.t")
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn condukt(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_condukt")))
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("CONDUKT_DEFAULT_BRANCH", "main")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("CONDUKT_DISABLE")
        .output()
        .expect("spawn condukt")
}

#[test]
fn state_set_done_on_a_large_diff_does_not_stall_on_the_git_pipe() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("seed.txt"), "seed\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "seed"]);
    let wt = tmp.path().join("wt");
    git(
        &repo,
        &["worktree", "add", "-q", "-b", "feat", wt.to_str().unwrap()],
    );
    // ~400 KB of added lines: far past a 64 KB pipe buffer.
    let big: String = (0..8000)
        .map(|i| format!("pub const C{i}: &str = \"line {i} of a large diff\";\n"))
        .collect();
    std::fs::create_dir_all(wt.join("src")).unwrap();
    std::fs::write(wt.join("src").join("big.rs"), &big).unwrap();
    git(&wt, &["add", "."]);
    git(&wt, &["commit", "-q", "-m", "big"]);

    let dec = tmp.path().join("d.json");
    std::fs::write(
        &dec,
        r#"{"goal":"g","tasks":[{"id":"t1","title":"x","touched_files":["src/big.rs"],"deps":[],"class":"serial","done_criteria":"d"}]}"#,
    )
    .unwrap();
    let wt_s = wt.to_str().unwrap();
    let out = condukt(
        &repo,
        &home,
        &[
            "state",
            "init",
            "--run",
            "r02e5",
            "--file",
            dec.to_str().unwrap(),
        ],
    );
    assert!(out.status.success(), "init: {out:?}");
    let out = condukt(
        &repo,
        &home,
        &[
            "state",
            "set",
            "--run",
            "r02e5",
            "--task",
            "t1",
            "--status",
            "running",
            "--worktree",
            wt_s,
            "--branch",
            "feat",
        ],
    );
    assert!(out.status.success(), "set running: {out:?}");

    let t0 = Instant::now();
    let out = condukt(
        &repo,
        &home,
        &[
            "state",
            "set",
            "--run",
            "r02e5",
            "--task",
            "t1",
            "--status",
            "done",
            "--worktree",
            wt_s,
            "--branch",
            "feat",
        ],
    );
    let elapsed = t0.elapsed();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        elapsed < Duration::from_secs(30),
        "state set --status done took {elapsed:?} on a ~400 KB diff (git's 45s \
         timeout is being hit: stdout is read only after git exits); stderr:\n{err}"
    );
    assert!(
        !err.contains("(no-diff)"),
        "a ~400 KB committed diff was reported as no-diff; stderr:\n{err}"
    );
}
