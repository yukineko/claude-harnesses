// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog 8d2429aa: after a successful `worktree merge --run --task`, the
//! `merge_completed_at` stamp goes through `let _ = state::with_run_locked(..)`.
//! When the run state cannot be read (here: corrupt JSON), the stamp silently
//! fails — the merge reports success and nothing says the phase timestamp was
//! lost. A failure must be distinguishable from a no-op: stderr must name the
//! run whose stamp could not be written.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn run_git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
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

fn condukt(repo: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_condukt"))
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("CLAUDE_CODE_SESSION_ID", "sess-8d2429aa")
        .env("CONDUKT_DEFAULT_BRANCH", "main")
        .env_remove("CONDUKT_DISABLE")
        .output()
        .expect("spawn condukt")
}

fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if let Some(hit) = find_file(&p, name) {
                return Some(hit);
            }
        } else if p.file_name().and_then(|n| n.to_str()) == Some(name) {
            return Some(p);
        }
    }
    None
}

#[test]
#[ignore = "backlog 8d2429aa: open defect, remove ignore when fixed"]
fn failed_merge_completed_stamp_is_not_silent() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    run_git(&repo, &["init", "-q", "-b", "main"]);
    run_git(&repo, &["config", "user.email", "t@t.t"]);
    run_git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("base.txt"), "base\n").unwrap();
    run_git(&repo, &["add", "."]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);

    let dec = repo.join("dec.json");
    std::fs::write(
        &dec,
        r#"{"goal":"g","tasks":[{"id":"t1","title":"edit d","touched_files":["d.txt"],"deps":[],"class":"parallel","done_criteria":"d"}]}"#,
    )
    .unwrap();
    let out = condukt(
        &repo,
        &home,
        &[
            "state",
            "init",
            "--run",
            "runX",
            "--file",
            dec.to_str().unwrap(),
        ],
    );
    assert_eq!(out.status.code(), Some(0), "state init: {out:?}");

    // Corrupt the run state so the post-merge stamp cannot load it.
    let state = find_file(&home.join(".condukt").join("state"), "runX.json")
        .expect("run state written by state init");
    std::fs::write(&state, "{ this is not json").unwrap();

    run_git(&repo, &["checkout", "-q", "-b", "condukt/runX-t1"]);
    std::fs::write(repo.join("d.txt"), "work\n").unwrap();
    run_git(&repo, &["add", "."]);
    run_git(&repo, &["commit", "-q", "-m", "task work"]);
    run_git(&repo, &["checkout", "-q", "main"]);

    let out = condukt(
        &repo,
        &home,
        &[
            "worktree",
            "merge",
            "--branch",
            "condukt/runX-t1",
            "--run",
            "runX",
            "--task",
            "t1",
        ],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stdout.contains("merged"),
        "fixture precondition: the merge itself must succeed; stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        std::fs::read_to_string(&state).unwrap() == "{ this is not json",
        "fixture precondition: the run state must still be unreadable (nothing stamped)"
    );
    assert!(
        stderr.contains("runX") && stderr.contains("merge_completed_at"),
        "the merge_completed_at stamp failed (run state unreadable) but nothing \
         said so — failure is indistinguishable from a no-op. stderr={stderr:?}"
    );
}
