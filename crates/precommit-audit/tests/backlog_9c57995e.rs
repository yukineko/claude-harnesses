#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro for backlog 9c57995e: precommit-audit's session-scoped one-shot
//! skip is spent on the FIRST read (`consume_session_skip(.., false)`). When a
//! LATER pre-commit check (check-doc-claims / check-test-weakening / ...)
//! blocks that same commit, no commit happened, yet the operator's bypass is
//! gone: the retry of the very commit it authorised is blocked again.
//!
//! Sequence (real binary, isolated HOME, one session id):
//!   staged `app.py` with no test        -> audit blocks (control, exit 1)
//!   `precommit-audit skip --reason ..`  -> skip issued
//!   audit #1                            -> exit 0 (skip honoured)
//!   (a later pre-commit check blocks; NO commit is made; HEAD unchanged)
//!   audit #2 on the identical tree      -> must still be exit 0

use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let d = std::env::temp_dir().join(format!(
        "pca-backlog-9c57995e-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(d.join("home")).unwrap();
    std::fs::create_dir_all(d.join("repo")).unwrap();
    d
}

fn git(dir: &Path, args: &[&str]) {
    let o = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap();
    assert!(o.status.success(), "git {args:?}: {o:?}");
}

fn pca(root: &Path, args: &[&str]) -> (i32, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_precommit-audit"))
        .args(args)
        .current_dir(root.join("repo"))
        .env("HOME", root.join("home"))
        .env("CLAUDE_CODE_SESSION_ID", "s-9c57995e")
        .env("HARNESS_TRUST_ALL", "1")
        .env_remove("CLAUDE_PROJECT_DIR")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap();
    (
        o.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ),
    )
}

fn audit(root: &Path) -> (i32, String) {
    let repo = root.join("repo");
    pca(
        root,
        &["--mode", "precommit", "--root", repo.to_str().unwrap()],
    )
}

#[test]
#[ignore = "backlog 9c57995e: open defect, remove ignore when fixed"]
fn backlog_9c57995e_skip_survives_a_commit_blocked_by_a_later_check() {
    let root = scratch();
    let repo = root.join("repo");
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "t@example.com"]);
    git(&repo, &["config", "user.name", "Test"]);
    std::fs::write(repo.join("README.md"), "# repo\n").unwrap();
    std::fs::write(
        repo.join(".precommit-audit.toml"),
        "[checks]\nlinters = false\n",
    )
    .unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "init"]);
    std::fs::write(repo.join("app.py"), "def add(a, b):\n    return a + b\n").unwrap();
    git(&repo, &["add", "app.py"]);

    let (c0, e0) = audit(&root);
    assert_eq!(c0, 1, "control: source without test must block: {e0}");

    let (cs, es) = pca(&root, &["skip", "--reason", "backlog 9c57995e probe"]);
    assert_eq!(cs, 0, "skip must be issued: {es}");

    let (c1, e1) = audit(&root);
    assert_eq!(
        c1, 0,
        "control: the issued skip must be honoured once: {e1}"
    );

    // A later pre-commit check blocks this commit: nothing is committed.
    let (c2, e2) = audit(&root);
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(
        c2, 0,
        "the commit the skip authorised never happened (a later check blocked it), \
         yet the retry is blocked again — the one-shot skip was burned: {e2}"
    );
}
