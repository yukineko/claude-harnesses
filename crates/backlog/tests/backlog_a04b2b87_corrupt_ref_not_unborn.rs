#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog a04b2b87: `backlog next` skips pending tasks whose fix commit
//! `(backlog <id>)` is in HEAD's history. An UNBORN HEAD (valid repo, no
//! commits) is a known-empty set of landed fixes; a CORRUPT current-branch ref
//! is NOT unborn -- the landed-fix set cannot be determined, so plain `next`
//! must refuse rather than hand out the task as if no fix had landed
//! (repo CLAUDE.md section 3: cannot-determine resolves to refuse).

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const TASK_ID: &str = "a04b0001";

fn git(repo: &Path, args: &[&str]) {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn pending_block(id: &str, title: &str, project: &Path) -> String {
    let project = project.display();
    format!(
        "[[task]]\nid = \"{id}\"\ntitle = \"{title}\"\nproject = \"{project}\"\n\
         tags = [\"p1\"]\nstatus = \"pending\"\nnotes = \"\"\n\
         created_at = 1000\nupdated_at = 1000\nweight = 0.0\n[task.repro]\noutcome = \"reproduced\"\ncmd = \"cargo test -p fixture\"\nobserved_at = 1000\ndetail = \"fixture\"\n\n"
    )
}

/// Plain (primary-checkout) git repo with a repo store holding one pending task.
fn repo_with_task(td: &tempfile::TempDir, name: &str) -> (PathBuf, PathBuf) {
    let home = td.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = td.path().join(name);
    std::fs::create_dir_all(&repo).unwrap();
    let repo = repo.canonicalize().unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(repo.join(".backlog")).unwrap();
    std::fs::write(
        repo.join(".backlog/tasks.toml"),
        pending_block(TASK_ID, "pending fixture task", &repo),
    )
    .unwrap();
    (home, repo)
}

fn run_next(home: &Path, cwd: &Path) -> common::Out {
    let mut c = Command::new(env!("CARGO_BIN_EXE_backlog"));
    harness_core::git_env::scrub(&mut c);
    c.arg("next")
        .env("HOME", home)
        .env("PATH", common::path_with_condukt_shim())
        .env_remove("BACKLOG_DISABLE")
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = c.spawn().unwrap().wait_with_output().unwrap();
    common::Out {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

#[test]
fn corrupt_branch_ref_is_not_misread_as_unborn() {
    let td = tempfile::tempdir().unwrap();
    let (home, repo) = repo_with_task(&td, "corrupt");
    git(
        &repo,
        &["commit", "-q", "--allow-empty", "--no-verify", "-m", "init"],
    );

    // Non-vacuity: before corruption `next` hands the task out.
    let ok = run_next(&home, &repo);
    assert_eq!(ok.code, 0, "setup control: {}", ok.both());
    assert!(
        ok.stdout.contains(TASK_ID),
        "setup control: next names the task: {}",
        ok.both()
    );

    // Corrupt the current branch ref with literal garbage.
    let ref_file = repo.join(".git/refs/heads/main");
    assert!(
        ref_file.is_file(),
        "setup: loose ref file expected at {}",
        ref_file.display()
    );
    std::fs::write(&ref_file, "not-a-sha\n").unwrap();

    let o = run_next(&home, &repo);
    assert_ne!(
        o.code,
        0,
        "corrupt branch ref (`refs/heads/main` = `not-a-sha`) was misread as an unborn HEAD: \
         `backlog next` exited 0 instead of refusing. output: {}",
        o.both()
    );
    assert!(
        !o.stdout.contains(TASK_ID),
        "corrupt branch ref: `backlog next` handed out {TASK_ID} although the landed-fix set \
         cannot be determined. stdout: {}",
        o.stdout
    );
}

#[test]
fn unborn_head_has_no_landed_fixes_and_hands_out_the_task() {
    let td = tempfile::tempdir().unwrap();
    let (home, repo) = repo_with_task(&td, "unborn");
    // Non-vacuity: HEAD really is unborn (no commits).
    let rp = harness_core::git_env::scrub(&mut Command::new("git"))
        .arg("-C")
        .arg(&repo)
        .args(["rev-parse", "--verify", "-q", "HEAD"])
        .output()
        .unwrap();
    assert!(!rp.status.success(), "setup: HEAD must be unborn");

    let o = run_next(&home, &repo);
    assert_eq!(
        o.code,
        0,
        "unborn HEAD is a valid empty history; next must not refuse: {}",
        o.both()
    );
    assert!(
        o.stdout.contains(TASK_ID),
        "unborn HEAD: next must hand out the pending task: {}",
        o.both()
    );
}
