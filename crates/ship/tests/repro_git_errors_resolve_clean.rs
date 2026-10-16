#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro: git failures in `src/git.rs` resolve to "clean".
//!
//! Every function there maps a failed `git` call to the all-clear value:
//! `uncommitted_changes` -> false on Err (and ignores the exit status),
//! `unmerged_condukt_branches` / `leftover_worktrees` -> empty vec,
//! `unpushed_count` -> 0 when both `@{upstream}..HEAD` and `origin/main..HEAD`
//! fail. The checklist then prints `[x]` for a check that never ran.
//!
//! NOTE: the unit test `test_unpushed_count_no_upstream` in src/git.rs pins the
//! 0 for the no-upstream case. It encodes the current fail-open as spec and is
//! deliberately NOT edited here; fixing the finding will require the owner to
//! revisit that test.
//!
//! Asserts the correct behaviour: when git could not answer, the checklist must
//! not print a `[x]` line for that item.

use std::process::Command;

fn ship_check(dir: &std::path::Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_ship"))
        .arg("check")
        .current_dir(dir)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let o = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
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

#[test]
#[ignore = "RED repro: git errors resolve to clean in ship"]
fn no_upstream_is_not_reported_as_no_unpushed_commits() {
    let repo = tempfile::TempDir::new().unwrap();
    let p = repo.path();
    git(p, &["init", "-q"]);
    std::fs::write(p.join("a"), "a").unwrap();
    git(p, &["add", "a"]);
    git(p, &["commit", "-q", "-m", "c1"]);
    // One local commit, no remote, no upstream: ship cannot know what is unpushed.
    let out = ship_check(p);
    assert!(
        !out.contains("[x] no unpushed commits"),
        "no upstream => unpushed count undeterminable, but reported clean:\n{out}"
    );
}

#[test]
#[ignore = "RED repro: git errors resolve to clean in ship"]
fn broken_git_dir_is_not_reported_as_all_clean() {
    let repo = tempfile::TempDir::new().unwrap();
    let p = repo.path();
    git(p, &["init", "-q"]);
    // Corrupt HEAD so every git subcommand fails.
    std::fs::write(p.join(".git/HEAD"), "garbage\n").unwrap();
    std::fs::write(p.join("dirty.txt"), "uncommitted").unwrap();
    let out = ship_check(p);
    assert!(
        !out.contains("[x] no uncommitted changes"),
        "git failed (corrupt HEAD) but ship reported no uncommitted changes:\n{out}"
    );
}
