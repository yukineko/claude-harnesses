#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog 19ed3ae9: an uncommitted `backlog add` left in the main checkout
//! physically stops integration — `git merge <branch>` aborts with "Your local
//! changes to the following files would be overwritten by merge:
//! .backlog/tasks.toml" — and the filer cannot commit it on main (section 8
//! gate), so sessions waiting for the file to become clean deadlock.
//!
//! Fixture: `main` with a committed store; a branch that files a task and
//! commits; back on main, an uncommitted `backlog add`; then the integration
//! `git merge feat`. Property asserted: filing a task on main does not block
//! the merge. RED = the merge aborts on `.backlog/tasks.toml`.
//!
//! The deadlock between two waiting sessions itself is timing; this pins the
//! deterministic half the ticket observed verbatim (the aborted merge).
//!
//! Written by an independent auditor, not an implementer.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn unique(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "backlog-19ed3ae9-{}-{}-{}",
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

fn must(repo: &Path, home: &Path, args: &[&str]) {
    let (ok, o) = git(repo, home, args);
    assert!(ok, "precondition git {args:?}: {o}");
}

fn add(repo: &Path, home: &Path, title: &str) {
    let out = Command::new(env!("CARGO_BIN_EXE_backlog"))
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
#[ignore = "backlog 19ed3ae9: open defect, remove ignore when fixed"]
fn uncommitted_filing_on_main_does_not_block_integration() {
    let root = unique("t");
    let (home, repo) = (root.join("home"), root.join("repo"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    must(&repo, &home, &["init", "-q", "-b", "main"]);
    must(&repo, &home, &["config", "user.email", "t@example.com"]);
    must(&repo, &home, &["config", "user.name", "t"]);
    must(&repo, &home, &["config", "commit.gpgsign", "false"]);
    add(&repo, &home, "seed task");
    must(&repo, &home, &["add", ".backlog"]);
    must(&repo, &home, &["commit", "-q", "-m", "seed store"]);

    must(&repo, &home, &["checkout", "-q", "-b", "feat"]);
    add(&repo, &home, "filed on the feature branch");
    must(&repo, &home, &["add", ".backlog"]);
    must(&repo, &home, &["commit", "-q", "-m", "feat filing"]);
    must(&repo, &home, &["checkout", "-q", "main"]);

    add(&repo, &home, "filed on main by a parallel session");

    let (ok, out) = git(&repo, &home, &["merge", "--no-edit", "feat"]);
    assert!(
        ok,
        "an uncommitted `backlog add` on main blocked integrating `feat`: {out}"
    );
}
