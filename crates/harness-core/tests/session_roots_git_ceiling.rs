// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro: `GIT_CEILING_DIRECTORIES` in the hook's environment makes a write
//! INSIDE a real repository look "outside every repository", so
//! `session_roots::resolve` silently drops that repository from the roots
//! (fail-open: the Stop gate never judges the checkout the session edited).
//!
//! Observed (2026-10-26, git 2.50.1 Apple Git-155), with `<R>` a fresh
//! `git init` repo and `<R>/sub` a subdirectory:
//!
//! ```text
//! $ GIT_CEILING_DIRECTORIES=<R> git -C <R>/sub rev-parse --show-toplevel
//! fatal: not a git repository (or any of the parent directories): .git
//! exit=128
//! $ git -C <R>/sub rev-parse --show-toplevel
//! <R>
//! exit=0
//! ```
//!
//! That is exactly the exit code and phrase `toplevel()` maps to
//! `TopErr::OutsideAnyRepo`, and `toplevel()` does not clear the variable for
//! its `git` child. The required property: a write inside a real repository is
//! never excluded — the roots are either Known and contain that repository, or
//! Undetermined (CLAUDE.md section 3).
//!
//! Isolation: this binary holds exactly ONE test, so mutating the process
//! environment cannot race another test; the guard restores it on drop.

use harness_core::session_roots::{resolve, SessionRoots};
use harness_core::verdict::Determination;
use std::path::{Path, PathBuf};
use std::process::Command;

const CEILING: &str = "GIT_CEILING_DIRECTORIES";

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    harness_core::git_env::scrub(&mut Command::new("git"))
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git must be runnable for this test")
}

/// Scrubbed, then the process-env `GIT_CEILING_DIRECTORIES` this test sets
/// itself is passed through explicitly: the precondition below needs the
/// ceiling to reach the git child, but an inherited `GIT_DIR` (a hook-driven
/// `cargo test`) would override discovery and mask the ceiling.
fn git_inheriting_env(dir: &Path, args: &[&str]) -> std::process::Output {
    let mut cmd = Command::new("git");
    harness_core::git_env::scrub(&mut cmd);
    if let Some(v) = std::env::var_os(CEILING) {
        cmd.env(CEILING, v);
    }
    cmd.arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git must be runnable for this test")
}

fn init_repo(dir: &Path) {
    let out = git(dir, &["init", "-q"]);
    assert!(out.status.success(), "git init failed: {out:?}");
}

fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap()
}

/// A JSONL transcript recording a `Write` of `written`.
fn transcript(dir: &Path, written: &Path) -> String {
    let line = serde_json::json!({
        "message": {"role": "assistant", "content": [{
            "type": "tool_use", "id": "toolu_0", "name": "Write",
            "input": {"file_path": written.to_str().unwrap(), "content": "x"}
        }]}
    });
    let path = dir.join("session.jsonl");
    std::fs::write(&path, format!("{line}\n")).unwrap();
    path.to_string_lossy().into_owned()
}

/// Sets an env var for the scope and restores the previous value on drop.
struct EnvGuard {
    key: &'static str,
    prev: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, val: &Path) -> Self {
        let prev = std::env::var_os(key);
        std::env::set_var(key, val);
        EnvGuard { key, prev }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => std::env::set_var(self.key, v),
            None => std::env::remove_var(self.key),
        }
    }
}

fn roots_include(got: &Determination<SessionRoots>, repo: &Path) -> Result<(), String> {
    match got {
        Determination::Known(sr) if sr.roots.iter().any(|r| r == repo) => Ok(()),
        Determination::Known(sr) => Err(format!(
            "Known roots {:?} exclude the repository {repo:?} the session wrote into",
            sr.roots
        )),
        Determination::Undetermined(_) => Ok(()),
    }
}

#[test]
fn ceiling_env_does_not_exclude_an_in_repo_write() {
    let tmp = tempfile::tempdir().unwrap();
    let base = canon(tmp.path());
    // Hook cwd: the primary tree. The session wrote into a DIFFERENT repo, so
    // the cwd being roots[0] cannot satisfy the assertion by accident.
    let primary = base.join("primary");
    let other = base.join("other");
    let sub = other.join("sub");
    let tdir = base.join("t");
    for d in [&primary, &sub, &tdir] {
        std::fs::create_dir_all(d).unwrap();
    }
    init_repo(&primary);
    init_repo(&other);
    let written = sub.join("a.rs");
    std::fs::write(&written, "x").unwrap();
    let t = transcript(&tdir, &written);
    let other_c = canon(&other);

    // Control (no ceiling): the repo IS found — the assertion is not vacuous.
    assert!(
        std::env::var_os(CEILING).is_none(),
        "precondition: {CEILING} must be unset in the test environment"
    );
    let control = resolve(&primary, &t);
    match &control {
        Determination::Known(sr) => assert!(
            sr.roots.contains(&other_c),
            "control: without the ceiling the written repo must be a root: {:?}",
            sr.roots
        ),
        Determination::Undetermined(why) => {
            panic!(
                "control: expected Known roots, got Undetermined: {}",
                why.as_str()
            )
        }
    }

    let _g = EnvGuard::set(CEILING, &other_c);
    // Precondition: git now says the exact "outside" phrase for an in-repo dir.
    let out = git_inheriting_env(&sub, &["rev-parse", "--show-toplevel"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(128), "precondition: {out:?}");
    assert!(
        err.contains("not a git repository (or any of the parent directories)"),
        "precondition: git under the ceiling must print the outside phrase: {err}"
    );

    let got = resolve(&primary, &t);
    if let Err(msg) = roots_include(&got, &other_c) {
        panic!("with {CEILING}={other_c:?}: {msg}");
    }
}
