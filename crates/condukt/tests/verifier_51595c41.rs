// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Independent verifier tests for backlog 51595c41 (option c): `worktree
//! merge` / `worktree resolve-merge` integrate a task branch into
//! `default_branch` only when the branch's recorded creation base is an
//! ancestor of `default_branch`; every non-determination refuses and leaves
//! `default_branch` where it was. Driven through the real binary with `HOME`
//! pinned to a tempdir for every child (the real ~/.condukt is never touched).
//!
//! Written by the verifier, not the implementer.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Mutex;

/// Serializes the tests in this file: one of them must point this process's
/// own `HOME` at its tempdir to record an overwatch resolution in-process, and
/// no other thread may read the environment while it does.
static SERIAL: Mutex<()> = Mutex::new(());

struct Fx {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    home: PathBuf,
}

fn git_out(dir: &Path, args: &[&str]) -> Output {
    harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("spawn git")
}

fn git(dir: &Path, args: &[&str]) -> String {
    let o = git_out(dir, args);
    assert!(
        o.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn commit_file(dir: &Path, file: &str, content: &str, msg: &str) -> String {
    std::fs::write(dir.join(file), content).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-q", "-m", msg]);
    git(dir, &["rev-parse", "HEAD"])
}

/// A task worker's commit: carries the `Condukt-Task: <task>` trailer that
/// `worktree merge` demands on every landed commit.
fn commit_task_file(dir: &Path, task: &str, file: &str, content: &str, msg: &str) -> String {
    std::fs::write(dir.join(file), content).unwrap();
    git(dir, &["add", file]);
    let trailer = format!("Condukt-Task: {task}");
    git(dir, &["commit", "-q", "-m", msg, "--trailer", &trailer]);
    git(dir, &["rev-parse", "HEAD"])
}

impl Fx {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@t.t"]);
        git(&repo, &["config", "user.name", "t"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        commit_file(&repo, "base.txt", "base\n", "init");
        Fx {
            _tmp: tmp,
            repo,
            home,
        }
    }

    fn condukt_with_path(&self, args: &[&str], path: Option<&str>) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_condukt"));
        harness_core::git_env::scrub(&mut c);
        c.args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_SESSION_ID", "sess-verifier-51595c41")
            .env("CONDUKT_DEFAULT_BRANCH", "main")
            .env_remove("CONDUKT_DISABLE");
        if let Some(p) = path {
            c.env("PATH", p);
        }
        c.output().expect("spawn condukt")
    }

    fn condukt(&self, args: &[&str]) -> Output {
        self.condukt_with_path(args, None)
    }

    /// `condukt worktree create` → (worktree path, task branch name).
    fn create(&self, run: &str, topic: &str, branch: &str) -> (PathBuf, String) {
        let out = self.condukt(&[
            "worktree", "create", "--run", run, "--topic", topic, "--branch", branch,
        ]);
        assert!(out.status.success(), "worktree create: {out:?}");
        let wt = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string());
        let b = git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]);
        (wt, b)
    }

    fn main_sha(&self) -> String {
        git(&self.repo, &["rev-parse", "main"])
    }
}

fn s(o: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&o.stdout).to_string(),
        String::from_utf8_lossy(&o.stderr).to_string(),
    )
}

/// Happy path is unchanged even after `main` has advanced past the recorded
/// base: the base is still an ancestor of main, so the merge lands.
#[test]
fn create_from_main_then_main_advances_still_merges() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let fx = Fx::new();
    let base = fx.main_sha();
    let (wt, branch) = fx.create("runH", "t1", "b1");
    assert_eq!(
        git(
            &fx.repo,
            &["config", "--get", &format!("branch.{branch}.conduktBase")]
        ),
        base,
        "create must record the commit it cut the task branch from"
    );
    let task_commit = commit_task_file(&wt, "runH/t1", "task.txt", "task\n", "task");
    // main advances after create (another task landed meanwhile).
    commit_file(&fx.repo, "other.txt", "other\n", "main moved on");
    let out = fx.condukt(&["worktree", "merge", "--branch", &branch]);
    let (so, se) = s(&out);
    assert!(
        out.status.success(),
        "base on main must still merge: stdout={so} stderr={se}"
    );
    assert!(
        git_out(
            &fx.repo,
            &["merge-base", "--is-ancestor", &task_commit, "main"]
        )
        .status
        .success(),
        "task commit must be on main after a successful merge"
    );
}

/// A branch cut by hand (no record) is refused with a named diagnostic and
/// main does not move.
#[test]
fn hand_cut_branch_without_record_is_refused_and_main_unchanged() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let fx = Fx::new();
    git(&fx.repo, &["checkout", "-q", "-b", "handmade"]);
    commit_file(&fx.repo, "h.txt", "h\n", "handmade");
    git(&fx.repo, &["checkout", "-q", "main"]);
    let before = fx.main_sha();
    let out = fx.condukt(&["worktree", "merge", "--branch", "handmade"]);
    let (so, se) = s(&out);
    assert!(
        !out.status.success(),
        "unrecorded base must refuse: stdout={so} stderr={se}"
    );
    assert!(se.contains("merge-base-unrecorded"), "stderr={se}");
    assert_eq!(fx.main_sha(), before, "refusal must leave main untouched");
}

/// A record naming an existing object that is NOT a commit (a blob) is
/// undetermined and refused.
#[test]
fn record_pointing_at_a_blob_is_refused_and_main_unchanged() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let fx = Fx::new();
    let (wt, branch) = fx.create("runB", "t1", "b1");
    commit_task_file(&wt, "runB/t1", "task.txt", "task\n", "task");
    std::fs::write(fx.repo.join("blob.txt"), "i am a blob\n").unwrap();
    let blob = git(&fx.repo, &["hash-object", "-w", "blob.txt"]);
    std::fs::remove_file(fx.repo.join("blob.txt")).unwrap();
    git(
        &fx.repo,
        &[
            "config",
            "--replace-all",
            &format!("branch.{branch}.conduktBase"),
            &blob,
        ],
    );
    let before = fx.main_sha();
    let out = fx.condukt(&["worktree", "merge", "--branch", &branch]);
    let (so, se) = s(&out);
    assert!(
        !out.status.success(),
        "non-commit record must refuse: stdout={so} stderr={se}"
    );
    assert!(se.contains("merge-base-unrecorded"), "stderr={se}");
    assert_eq!(fx.main_sha(), before);
}

/// `git merge-base --is-ancestor` exiting with a code other than 0/1 is
/// undetermined and refuses (shimmed git; every other call is real git).
#[test]
fn ancestry_probe_error_exit_is_refused_and_main_unchanged() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let fx = Fx::new();
    let (wt, branch) = fx.create("runU", "t1", "b1");
    commit_task_file(&wt, "runU/t1", "task.txt", "task\n", "task");
    let real = String::from_utf8_lossy(
        &Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .to_string();
    assert!(!real.is_empty(), "no git on PATH: fixture is void");
    let shim_dir = fx.home.join("shim");
    std::fs::create_dir_all(&shim_dir).unwrap();
    let shim = shim_dir.join("git");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\nfor a in \"$@\"; do if [ \"$a\" = \"--is-ancestor\" ]; then echo 'shim: probe failed' >&2; exit 128; fi; done\nexec '{real}' \"$@\"\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        shim_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let before = fx.main_sha();
    let out = fx.condukt_with_path(&["worktree", "merge", "--branch", &branch], Some(&path));
    let (so, se) = s(&out);
    assert!(
        !out.status.success(),
        "ancestry probe exit 128 must refuse: stdout={so} stderr={se}"
    );
    assert!(se.contains("merge-base-undetermined"), "stderr={se}");
    assert_eq!(fx.main_sha(), before);
}

/// The twin path: a conflict recorded by `merge`, then resolved `theirs`, then
/// `resolve-merge` — refused when the recorded base is off main.
#[test]
fn resolve_merge_refuses_base_off_main_and_main_unchanged() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let fx = Fx::new();
    commit_file(&fx.repo, "shared.txt", "l1\nl2\nl3\n", "shared");
    let (wt, branch) = fx.create("runR", "t1", "b1");
    commit_task_file(
        &wt,
        "runR/t1",
        "shared.txt",
        "l1\nbranch\nl3\n",
        "branch edit",
    );
    commit_file(&fx.repo, "shared.txt", "l1\nmain\nl3\n", "main edit");
    let out = fx.condukt(&["worktree", "merge", "--branch", &branch]);
    let (so, se) = s(&out);
    assert!(
        out.status.success() && so.contains("CONFLICT"),
        "fixture precondition: the merge must record a conflict: stdout={so} stderr={se}"
    );
    let id = so
        .split("merge conflict recorded (")
        .nth(1)
        .and_then(|r| r.split(')').next())
        .expect("conflict id in stdout")
        .to_string();

    // A commit that exists only off main, recorded as the creation base.
    git(&fx.repo, &["checkout", "-q", "-b", "side"]);
    let side = commit_file(&fx.repo, "side.txt", "s\n", "side");
    git(&fx.repo, &["checkout", "-q", "main"]);
    git(
        &fx.repo,
        &[
            "config",
            "--replace-all",
            &format!("branch.{branch}.conduktBase"),
            &side,
        ],
    );

    // Record a `theirs` resolution in the pinned HOME's overwatch store.
    let prev_home = std::env::var_os("HOME");
    std::env::set_var("HOME", &fx.home);
    let rec = overwatch::store::append_merge_conflict_resolution(
        &fx.repo,
        &overwatch::merge_conflict::MergeConflictResolution {
            conflict_id: id.clone(),
            choice: overwatch::merge_conflict::ResolveChoice::Theirs,
            decided_by: overwatch::merge_conflict::DecidedBy::Human,
            note: None,
            ts: overwatch::store::now(),
        },
    );
    match prev_home {
        Some(h) => std::env::set_var("HOME", h),
        None => std::env::remove_var("HOME"),
    }
    rec.expect("record resolution");

    let before = fx.main_sha();
    let out = fx.condukt(&["worktree", "resolve-merge", "--id", &id]);
    let (so, se) = s(&out);
    assert!(
        !out.status.success(),
        "resolve-merge with base off main must refuse: stdout={so} stderr={se}"
    );
    assert!(
        se.contains("merge-base-not-on-default-branch"),
        "stderr={se}"
    );
    assert_eq!(fx.main_sha(), before);
    assert!(
        !git_out(&fx.repo, &["merge-base", "--is-ancestor", &side, "main"])
            .status
            .success(),
        "side commit must not reach main"
    );
}
