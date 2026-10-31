//! Test-only fixture for the inherited-`GIT_*` scrub tests (shared by the
//! `specguard` and `specforge` binaries via `#[path]`).
//!
//! A production git spawn that forgets `harness_core::git_env::scrub` follows an
//! inherited `GIT_DIR` instead of its `-C <repo_root>`. Setting `GIT_DIR` on the
//! test process itself would race every other test, so each scrub test is split:
//! the parent builds two repositories (A = target, B = decoy with different
//! content) and re-executes this test binary on exactly one child test with
//! `GIT_DIR=<B>/.git` in its environment. The child calls the production
//! function on A and asserts on the result; the parent requires that the child
//! actually ran (exactly one test passed) and succeeded.

use harness_core::verdict::Determination;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Env var that tells a child test it is being driven by its parent.
pub const CHILD: &str = "SPECGUARD_GITENV_CHILD";
/// Env var carrying repository A (the target) to the child.
pub const REPO_A: &str = "SPECGUARD_GITENV_REPO_A";

/// Run `cmd` through `harness_core::boundary::run` and return
/// `(exit code, stdout, stderr)`. A spawn that cannot be observed to the end
/// fails the test (it is never read as an empty success).
fn run(cmd: &mut Command) -> (i32, String, String) {
    let out = match harness_core::boundary::run(cmd) {
        Determination::Known(out) => Ok(out),
        Determination::Undetermined(u) => Err(format!("{u:?}")),
    }
    .expect("subprocess observed to completion");
    let code = out.code();
    let stderr = out.stderr().to_string();
    let stdout = match out.stdout_allowing(&[code]) {
        Determination::Known(s) => Ok(s),
        Determination::Undetermined(u) => Err(format!("{u:?}")),
    }
    .expect("stdout readable");
    (code, stdout, stderr)
}

/// `git -C dir <args>` for fixtures, itself scrubbed so the fixture can never
/// be redirected by whatever the test process inherited.
pub fn git(dir: &Path, args: &[&str]) -> String {
    let mut cmd = Command::new("git");
    harness_core::git_env::scrub(&mut cmd)
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args);
    let (code, stdout, stderr) = run(&mut cmd);
    assert_eq!(
        code,
        0,
        "fixture git {args:?} in {} failed: {stderr}",
        dir.display()
    );
    stdout
}

/// A repo with two commits: `<tag>1.txt` then `<tag>2.txt`.
pub fn make_repo(dir: &Path, tag: &str) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q"]);
    for n in ["1", "2"] {
        let f = format!("{tag}{n}.txt");
        std::fs::write(dir.join(&f), format!("{tag} {n}\n")).unwrap();
        git(dir, &["add", &f]);
        git(dir, &["commit", "-q", "-m", &f]);
    }
}

/// Two distinguishable repos `a` (files a1/a2) and `b` (files b1/b2).
pub fn two_repos() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let a = root.path().join("repo_a");
    let b = root.path().join("repo_b");
    make_repo(&a, "a");
    make_repo(&b, "b");
    (root, a, b)
}

/// The child's repo A, or `None` when this is not a driven child run.
pub fn child_repo_a() -> Option<PathBuf> {
    if std::env::var(CHILD).as_deref() != Ok("1") {
        return None;
    }
    Some(PathBuf::from(std::env::var(REPO_A).unwrap()))
}

/// Re-exec this test binary on `child_test` with `GIT_DIR=<b>/.git` inherited.
pub fn run_child(child_test: &str, a: &Path, b: &Path) {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", child_test, "--test-threads=1", "--nocapture"])
        .env(CHILD, "1")
        .env(REPO_A, a)
        .env("GIT_DIR", b.join(".git"))
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t");
    let (code, stdout, stderr) = run(&mut cmd);
    assert_eq!(
        code, 0,
        "child {child_test} failed under inherited GIT_DIR=<B>:\n{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("test result: ok. 1 passed"),
        "child {child_test} did not run exactly one test:\n{stdout}"
    );
}
