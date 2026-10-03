#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog 28b6df2c: the store is a TRACKED file (`.backlog/tasks.toml`) in the
//! checkout's own tree, so `backlog add` run from the main checkout leaves the
//! main working tree dirty (CLAUDE.md section 8 forbids editing it there, and
//! the Stop gates then see a foreign modification).
//!
//! Fixture: a repo on `main` with a committed store; one more `backlog add`
//! from that checkout. Property asserted: filing a task does not leave the
//! main working tree dirty. RED = `git status --porcelain` names the store.
//!
//! Written by an independent auditor, not an implementer.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn unique(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "backlog-28b6df2c-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(&dir).unwrap()
}

fn git(repo: &Path, home: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .stdin(Stdio::null())
        .output()
        .expect("git runs");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn add(repo: &Path, home: &Path, title: &str) {
    let out = Command::new(env!("CARGO_BIN_EXE_backlog"))
        .env("PATH", common::path_with_condukt_shim())
        .args(["add", "--title", title, "--project", repo.to_str().unwrap()])
        .env("HOME", home)
        .current_dir(repo)
        .stdin(Stdio::null())
        .output()
        .expect("binary runs");
    assert!(
        out.status.success(),
        "precondition: add must succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
#[ignore = "backlog 28b6df2c: open defect, remove ignore when fixed"]
fn backlog_add_from_the_main_checkout_leaves_main_clean() {
    let root = unique("t");
    let (home, repo) = (root.join("home"), root.join("repo"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    for a in [
        &["init", "-q", "-b", "main"][..],
        &["config", "user.email", "t@example.com"],
        &["config", "user.name", "t"],
        &["config", "commit.gpgsign", "false"],
    ] {
        let (ok, o) = git(&repo, &home, a);
        assert!(ok, "git {a:?}: {o}");
    }
    add(&repo, &home, "seed task");
    for a in [
        &["add", ".backlog"][..],
        &["commit", "-q", "-m", "seed store"],
    ] {
        let (ok, o) = git(&repo, &home, a);
        assert!(ok, "git {a:?}: {o}");
    }
    let (ok, before) = git(&repo, &home, &["status", "--porcelain"]);
    assert!(
        ok && before.trim().is_empty(),
        "precondition: clean main: {before:?}"
    );

    add(&repo, &home, "filed from the main checkout");

    let (ok, after) = git(&repo, &home, &["status", "--porcelain"]);
    assert!(ok, "git status failed: {after}");
    assert!(
        after.trim().is_empty(),
        "`backlog add` run in the main checkout dirtied main's working tree: {after:?}"
    );
}
