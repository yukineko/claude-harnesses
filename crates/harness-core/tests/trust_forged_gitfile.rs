#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! e79afdd2: `trust::resolve` must not grant `InheritedFromMainWorktree` to a
//! directory merely because its `.git` FILE says `gitdir: <trusted>/.git/worktrees/x`.
//! A real linked worktree has (a) `worktrees/<name>` present in the main repo and
//! (b) git's back-pointer `<git_dir>/gitdir` naming `<root>/.git`. A forged gitfile
//! has neither, or has (a) without (b).
//!
//! Integration test => its own process. HOME is pinned to a temp dir under a
//! local lock; the real home is never touched.

use harness_core::trust::{self, Trust};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

static LOCK: Mutex<()> = Mutex::new(());

fn pin_home() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", home.path());
    std::env::remove_var("HARNESS_TRUST_ALL");
    assert!(trust::trust_path().starts_with(home.path()), "apparatus");
    home
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .args(args)
        .output()
        .expect("git must be runnable");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A main repo with one commit, inside `base`.
fn make_main(base: &Path) -> PathBuf {
    let main = base.join("main");
    std::fs::create_dir_all(&main).unwrap();
    git(&main, &["init", "-q"]);
    std::fs::write(main.join("f"), "x").unwrap();
    git(&main, &["add", "f"]);
    git(&main, &["commit", "-q", "-m", "init"]);
    main
}

fn real_worktree(main: &Path, dest: &Path, name: &str) {
    git(
        main,
        &["worktree", "add", "-q", dest.to_str().unwrap(), "-b", name],
    );
    assert!(dest.join(".git").is_file(), "apparatus: linked worktree");
}

fn forged_dir(base: &Path, gitfile: &str) -> PathBuf {
    let d = base.join("attacker");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join(".git"), gitfile).unwrap();
    d
}

#[test]
fn forged_absolute_gitfile_to_nonexistent_worktree_is_untrusted() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _home = pin_home();
    let base = tempfile::tempdir().unwrap();
    let main = make_main(base.path());
    trust::add(&main).unwrap();
    let main_git = std::fs::canonicalize(main.join(".git")).unwrap();
    let forged = forged_dir(
        base.path(),
        &format!("gitdir: {}/worktrees/nonexistent\n", main_git.display()),
    );
    assert_eq!(trust::resolve(&forged), Trust::Untrusted);
}

#[test]
fn forged_relative_gitfile_is_untrusted() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _home = pin_home();
    let base = tempfile::tempdir().unwrap();
    let main = make_main(base.path());
    trust::add(&main).unwrap();
    let forged = forged_dir(base.path(), "gitdir: ../main/.git/worktrees/zz\n");
    assert_eq!(trust::resolve(&forged), Trust::Untrusted);
}

#[test]
fn forged_gitfile_naming_existing_worktree_with_foreign_backpointer_is_untrusted() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _home = pin_home();
    let base = tempfile::tempdir().unwrap();
    let main = make_main(base.path());
    trust::add(&main).unwrap();
    let real = base.path().join("realwt");
    real_worktree(&main, &real, "realwt");
    let main_git = std::fs::canonicalize(main.join(".git")).unwrap();
    let existing = main_git.join("worktrees").join("realwt");
    assert!(existing.is_dir(), "apparatus: worktrees/realwt exists");
    // Control: the genuine worktree resolves, so `existing` is a valid target.
    assert!(matches!(
        trust::resolve(&real),
        Trust::InheritedFromMainWorktree(_)
    ));
    let forged = forged_dir(base.path(), &format!("gitdir: {}\n", existing.display()));
    assert_eq!(
        trust::resolve(&forged),
        Trust::Untrusted,
        "back-pointer names realwt, not the forged dir"
    );
}

#[test]
fn real_worktree_of_trusted_main_inherits() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _home = pin_home();
    let base = tempfile::tempdir().unwrap();
    let main = make_main(base.path());
    trust::add(&main).unwrap();
    let wt = base.path().join("wt");
    real_worktree(&main, &wt, "wt1");
    match trust::resolve(&wt) {
        Trust::InheritedFromMainWorktree(m) => {
            assert_eq!(
                std::fs::canonicalize(m).unwrap(),
                std::fs::canonicalize(&main).unwrap()
            );
        }
        other => panic!("expected InheritedFromMainWorktree, got {other:?}"),
    }
}

#[test]
fn real_worktree_of_untrusted_main_is_untrusted() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _home = pin_home();
    let base = tempfile::tempdir().unwrap();
    let main = make_main(base.path());
    let wt = base.path().join("wt");
    real_worktree(&main, &wt, "wt1");
    assert_eq!(trust::resolve(&wt), Trust::Untrusted);
}

#[test]
fn forged_gitfile_naming_symlinked_admin_dir_is_untrusted() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _home = pin_home();
    let base = tempfile::tempdir().unwrap();
    let main = make_main(base.path());
    trust::add(&main).unwrap();
    let main_git = std::fs::canonicalize(main.join(".git")).unwrap();
    let alias = main_git.join("worktrees").join("alias");
    // The back-pointer in the outside dir DOES name the forged dir's .git, so
    // only the "admin dir is a real directory" check can reject this.
    let outside = base.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let forged = forged_dir(base.path(), &format!("gitdir: {}\n", alias.display()));
    let forged_dot_git = std::fs::canonicalize(forged.join(".git")).unwrap();
    std::fs::write(
        outside.join("gitdir"),
        format!("{}\n", forged_dot_git.display()),
    )
    .unwrap();
    std::fs::create_dir_all(alias.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&outside, &alias).unwrap();
    assert_eq!(trust::resolve(&forged), Trust::Untrusted);
}

#[test]
fn forged_gitfile_naming_admin_dir_without_backpointer_file_is_untrusted() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _home = pin_home();
    let base = tempfile::tempdir().unwrap();
    let main = make_main(base.path());
    trust::add(&main).unwrap();
    let main_git = std::fs::canonicalize(main.join(".git")).unwrap();
    let admin = main_git.join("worktrees").join("x");
    std::fs::create_dir_all(&admin).unwrap();
    assert!(!admin.join("gitdir").exists(), "apparatus: no back-pointer");
    let forged = forged_dir(base.path(), &format!("gitdir: {}\n", admin.display()));
    assert_eq!(trust::resolve(&forged), Trust::Untrusted);
}
