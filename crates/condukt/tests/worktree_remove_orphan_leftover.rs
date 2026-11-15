// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (backlog: worktree-hygiene — task worktree dir survives as an
//! UNREGISTERED orphan after `condukt worktree remove`).
//!
//! Observed 2026-11-05 / 2026-11-06: after condukt merged a task and removed
//! its worktree, `/mnt/c/tmp/aegis-worktrees/<run>-<task>/` remained holding
//! only `target/` (cargo output) and an untracked `tdd.toml`, absent from
//! `git worktree list`. The tdd / donegate Stop hooks then failed closed
//! ("not a git repository") on every Stop.
//!
//! Mechanism reproduced here: `worktree::remove_worktree_preserving` falls back
//! to `git worktree remove --force` (the untracked `tdd.toml` makes the plain
//! remove refuse). When that forced removal fails PART-WAY through the
//! directory tree (here: an entry under the ignored `target/` cannot be
//! unlinked; in the field plausibly a concurrent cargo write or a drvfs/v9fs
//! unlink failure), git still deletes the worktree's admin dir. The result is a
//! directory that is neither removed nor a registered worktree — `git rev-parse`
//! inside it fails — and nothing in condukt cleans it up or re-registers it.
//!
//! The invariant asserted: after `condukt worktree remove` returns, the path is
//! EITHER gone OR still a registered worktree. Never an unregistered orphan.
//!
//! Limit (stated): the unlink failure is simulated with a non-writable
//! directory, so the test is not RED when run as root (root ignores the mode).

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

fn run_git(dir: &Path, args: &[&str]) -> String {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn registered_worktrees(repo: &Path) -> Vec<PathBuf> {
    run_git(repo, &["worktree", "list", "--porcelain"])
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .map(PathBuf::from)
        .collect()
}

#[test]
#[ignore = "backlog: condukt worktree remove leaves an unregistered orphan dir (target/ + tdd.toml) when the forced removal fails part-way"]
fn remove_never_leaves_an_unregistered_orphan_directory() {
    let base = std::env::temp_dir().join(format!("condukt-wt-orphan-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let base = base.canonicalize().unwrap();
    let repo = base.join("repo");
    let wt_base = base.join("worktrees");
    let home = base.join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&wt_base).unwrap();
    std::fs::create_dir_all(home.join(".condukt").join("state")).unwrap();
    std::fs::create_dir_all(home.join(".claude").join("projects")).unwrap();

    run_git(&repo, &["init", "-q", "-b", "main"]);
    run_git(&repo, &["config", "user.email", "t@t.t"]);
    run_git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join(".gitignore"), "target/\n").unwrap();
    std::fs::write(repo.join("seed.txt"), "seed\n").unwrap();
    run_git(&repo, &["add", ".gitignore", "seed.txt"]);
    run_git(&repo, &["commit", "-q", "-m", "seed"]);

    // A task worktree whose branch is merged (as after `condukt worktree merge`).
    let wt = wt_base.join("run-x-task");
    run_git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            wt.to_str().unwrap(),
            "-b",
            "condukt/run-x/task",
        ],
    );
    // Field residue: untracked tdd.toml + ignored cargo target/.
    std::fs::write(wt.join("tdd.toml"), "x\n").unwrap();
    let locked = wt.join("target").join("debug");
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::write(locked.join("artifact"), "bin\n").unwrap();
    // Make one entry under target/ un-unlinkable so the forced removal fails
    // part-way (stand-in for a concurrent cargo write / FS unlink failure).
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();

    let out = harness_core::git_env::scrub(&mut Command::new(bin()))
        .args([
            "worktree",
            "remove",
            "--path",
            wt.to_str().unwrap(),
            "--branch",
            "condukt/run-x/task",
        ])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("CONDUKT_WORKTREE_BASE", &wt_base)
        .env_remove("CONDUKT_DISABLE")
        .output()
        .unwrap();

    let exists = wt.exists();
    let registered = registered_worktrees(&repo)
        .iter()
        .any(|p| p.canonicalize().ok().as_deref() == wt.canonicalize().ok().as_deref());
    let listing: Vec<String> = if exists { walk(&wt) } else { Vec::new() };

    // Restore permissions so the temp dir can be cleaned up.
    let _ = std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755));
    let _ = std::fs::remove_dir_all(&base);

    assert!(
        !exists || registered,
        "`condukt worktree remove` left {} on disk but NOT registered as a git worktree \
         (an orphan: `git rev-parse` inside it fails, so tdd/donegate Stop hooks fail closed). \
         exit={:?}\nstderr: {}\nremaining entries: {listing:?}",
        wt.display(),
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn walk(dir: &Path) -> Vec<String> {
    let mut v = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            v.push(p.display().to_string());
            if p.is_dir() {
                v.extend(walk(&p));
            }
        }
    }
    v
}
