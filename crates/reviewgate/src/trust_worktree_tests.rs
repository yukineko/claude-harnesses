//! backlog 4cacdfae: a LINKED git worktree whose main working tree is in the
//! trust store must load its own project `reviewgate.toml`. Exact-match
//! `is_trusted` does not do that, so these tests pin the worktree-inheritance
//! behaviour (and the two controls that must keep holding).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::config::{Config, HOME_ENV_LOCK};
use std::path::{Path, PathBuf};
use std::process::Command;

const PROJECT_TOML: &str = "mode = \"subprocess\"\nreviewer_cmd = \"wt-reviewer\"\n";

fn git(dir: &Path, args: &[&str]) {
    let o = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

/// (main working tree, linked worktree); the project config is committed so it
/// exists in both checkouts.
fn fixture(base: &Path) -> (PathBuf, PathBuf) {
    let main = base.join("main");
    std::fs::create_dir_all(&main).unwrap();
    git(&main, &["init", "-q", "-b", "main"]);
    git(&main, &["config", "user.email", "t@t.com"]);
    git(&main, &["config", "user.name", "t"]);
    std::fs::write(Config::project_path(&main), PROJECT_TOML).unwrap();
    git(&main, &["add", "-A"]);
    git(&main, &["commit", "-qm", "seed"]);
    let wt = base.join("wt");
    git(
        &main,
        &["worktree", "add", "-q", wt.to_str().unwrap(), "HEAD"],
    );
    assert!(wt.join(".git").is_file(), "apparatus: linked worktree");
    assert!(
        Config::project_path(&wt).exists(),
        "apparatus: config in worktree"
    );
    (main, wt)
}

fn isolate_home(home: &Path) {
    std::env::set_var("HOME", home);
    std::env::remove_var("HARNESS_TRUST_ALL");
}

#[test]
fn linked_worktree_of_trusted_main_loads_project_config() {
    let _g = HOME_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (home, base) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    isolate_home(home.path());
    let (main, wt) = fixture(base.path());
    harness_core::trust::add(&main).unwrap();
    assert_eq!(
        Config::load(&main).reviewer_cmd,
        "wt-reviewer",
        "apparatus: main loads"
    );
    assert_eq!(
        Config::load(&wt).reviewer_cmd,
        "wt-reviewer",
        "a linked worktree of a TRUSTED main must load its project reviewgate.toml"
    );
}

#[test]
fn linked_worktree_of_untrusted_main_still_ignores_project_config() {
    let _g = HOME_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (home, base) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    isolate_home(home.path());
    let (_main, wt) = fixture(base.path());
    assert_eq!(
        Config::load(&wt).reviewer_cmd,
        "claude -p",
        "a worktree of an UNTRUSTED main must not load the project config"
    );
}

#[test]
fn trusted_plain_checkout_still_loads_project_config() {
    let _g = HOME_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (home, base) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    isolate_home(home.path());
    let (main, _wt) = fixture(base.path());
    harness_core::trust::add(&main).unwrap();
    assert_eq!(Config::load(&main).reviewer_cmd, "wt-reviewer");
}
