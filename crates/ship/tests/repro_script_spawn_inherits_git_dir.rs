#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro: `checklist::run_safe` / `rollout` (src/checklist.rs) spawn
//! `scripts/rebuild-plugins.sh` / `scripts/rollout-plugins.sh` without
//! `harness_core::git_env::scrub`, so any `git` run inside those scripts
//! follows an inherited `GIT_DIR`.
//!
//! Observable wrong effect: the stub script runs `git tag from-ship-script`
//! (a stand-in for the real scripts' git calls). With `GIT_DIR=<B>/.git`
//! inherited by `ship`, the tag is written into unrelated repo B instead of
//! the repo `ship` was run for (A).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> String {
    let o = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn committed_repo() -> tempfile::TempDir {
    let d = tempfile::TempDir::new().unwrap();
    git(d.path(), &["init", "-q", "-b", "main"]);
    fs::write(d.path().join("tracked"), "t").unwrap();
    git(d.path(), &["add", "tracked"]);
    git(d.path(), &["commit", "-q", "-m", "c1"]);
    d
}

fn add_stub(repo: &Path, name: &str) {
    fs::create_dir_all(repo.join("scripts")).unwrap();
    let s = repo.join("scripts").join(name);
    fs::write(
        &s,
        "#!/bin/sh\ngit -c user.name=t -c user.email=t@t tag from-ship-script\nexit 0\n",
    )
    .unwrap();
    fs::set_permissions(&s, fs::Permissions::from_mode(0o755)).unwrap();
}

fn run_with_git_dir(args: &[&str], a: &Path, b: &Path) {
    let o = harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_ship")))
        .args(args)
        .current_dir(a)
        .env("GIT_DIR", b.join(".git"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "setup: ship {args:?} failed: {}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
}

fn check_script_git_stays_in_repo(script: &str, args: &[&str]) {
    let a = committed_repo();
    let b = committed_repo();
    add_stub(a.path(), script);
    run_with_git_dir(args, a.path(), b.path());
    let tags_a = git(a.path(), &["tag", "--list"]);
    let tags_b = git(b.path(), &["tag", "--list"]);
    assert!(
        tags_b.trim().is_empty() && tags_a.contains("from-ship-script"),
        "git inside {script} followed inherited GIT_DIR: tag landed in unrelated repo B \
         (tags in A: {tags_a:?}, tags in B: {tags_b:?})"
    );
}

#[ignore = "RED repro: checklist::run_safe spawns rebuild-plugins.sh without git_env::scrub, git in the script follows inherited GIT_DIR"]
#[test]
fn run_safe_script_git_ignores_inherited_git_dir() {
    check_script_git_stays_in_repo("rebuild-plugins.sh", &["check", "--run-safe"]);
}

#[ignore = "RED repro: checklist::rollout spawns rollout-plugins.sh without git_env::scrub, git in the script follows inherited GIT_DIR"]
#[test]
fn rollout_script_git_ignores_inherited_git_dir() {
    check_script_git_stays_in_repo("rollout-plugins.sh", &["rollout"]);
}
