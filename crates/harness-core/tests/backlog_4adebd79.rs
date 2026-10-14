#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog 4adebd79: `primary_tree` decides NoRepo (= write allowed) when git
//! fails and `git_probe::dot_git_present` is false, and that uses
//! `Path::exists`, which follows symlinks. A `.git` entry that IS present but
//! cannot be resolved (a broken symlink) therefore reads as "no repo here"
//! instead of Undetermined, and the store write is allowed.
//!
//! RED repro: cargo test -p harness-core --test backlog_4adebd79 -- --ignored

use harness_core::primary_tree::{classify, refuse_if_primary, TreeKind};

#[test]
#[ignore = "backlog 4adebd79: RED repro"]
fn present_but_unresolvable_dot_git_is_not_no_repo() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().canonicalize().unwrap();
    std::os::unix::fs::symlink(dir.join("gone/gitdir"), dir.join(".git")).unwrap();
    // control: the entry really is there (lstat sees it), git cannot use it
    assert!(std::fs::symlink_metadata(dir.join(".git")).is_ok());
    let k = classify(&dir);
    assert!(
        matches!(k, TreeKind::Undetermined(_)),
        "a present-but-broken .git must be Undetermined, got {k:?}"
    );
    assert!(
        refuse_if_primary(&dir, "w").is_err(),
        "the write must be refused when .git is present but unresolvable"
    );
}

#[test]
fn control_absent_dot_git_is_no_repo() {
    let t = tempfile::tempdir().unwrap();
    assert_eq!(classify(t.path()), TreeKind::NoRepo);
    assert!(refuse_if_primary(t.path(), "w").is_ok());
}
