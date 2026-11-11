//! #1604: `git restore` worktree discard, content-in-commit verdict. Kept out
//! of `tests.rs`, which is at its file-growth cap. Written by an independent
//! test writer, not the implementer.
//!
//! The detect-level half of the user ruling (2026-11-08): a working-tree
//! `git restore` is Allow ONLY when the probe observed every path's index and
//! working-tree content in some commit; NotInAnyCommit is Deny; anything
//! undeterminable (probe Undetermined, no probe, global options, pathspec
//! magic, globs, options outside the model, no pathspec) is never Allow.
//! The probes below are fakes so the verdict logic is tested in isolation;
//! the real probe is tested in `reversible::tests`.

use super::*;
use serde_json::json;

const DP: &str = "/home/yuki/proj";

/// A filesystem with no symlinks (same convention as `tests.rs`'s
/// `wt_identity`): resolution never touches this machine's disk.
fn wt_identity(p: &str) -> Option<String> {
    Some(p.to_string())
}

fn verdict_name(d: &Decision) -> &'static str {
    match d {
        Decision::Allow => "allow",
        Decision::Ask(_) => "ask",
        Decision::Deny(_) => "deny",
    }
}

fn restore_in_commits(_: &str) -> Determination<crate::reversible::RestoreContent> {
    Determination::known(crate::reversible::RestoreContent::InCommits)
}

fn restore_not_in_commit(_: &str) -> Determination<crate::reversible::RestoreContent> {
    Determination::known(crate::reversible::RestoreContent::NotInAnyCommit(
        "working tree".to_string(),
    ))
}

fn restore_undetermined(_: &str) -> Determination<crate::reversible::RestoreContent> {
    Determination::undetermined("git exited 128")
}

/// InCommits for every path except one whose name ends in `dirty.sh`.
fn restore_by_path(p: &str) -> Determination<crate::reversible::RestoreContent> {
    if p.ends_with("dirty.sh") {
        restore_not_in_commit(p)
    } else {
        restore_in_commits(p)
    }
}

fn restore_roots(probe: Option<crate::reversible::GitRestoreProbe>) -> SafeRoots {
    let r = SafeRoots::new(
        Some(DP),
        Some(DP),
        Some("/home/yuki"),
        None,
        Some(wt_identity),
    );
    match probe {
        Some(p) => r.with_git_restore_probe(p),
        None => r,
    }
}

fn restore_run(cmd: &str, probe: Option<crate::reversible::GitRestoreProbe>) -> Decision {
    detect_scoped(
        "Bash",
        Some(&json!({ "command": cmd })),
        &restore_roots(probe),
    )
}

#[test]
fn restore_1604_in_commits_probe_allows_worktree_restore() {
    for cmd in [
        "git restore --staged --worktree -- a.sh",
        "git restore a.sh",
        "git restore --worktree a.sh",
        "git restore -SW -- a.sh b.sh",
        "git restore --source=HEAD --staged --worktree -- a.sh",
        "git restore -s HEAD -- a.sh",
        "git restore -- /home/yuki/proj/sub/a.sh",
    ] {
        assert_eq!(
            restore_run(cmd, Some(restore_in_commits)),
            Decision::Allow,
            "{cmd}"
        );
    }
}

#[test]
fn restore_1604_in_commits_probe_still_refuses_undeterminable_shapes() {
    for cmd in [
        "git restore --staged --worktree -- .githooks/pre-commit",
        "git restore --staged --worktree -- .claude/settings.json",
        "git restore -- .claude/settings.json",
        "git -C /home/yuki/proj restore --staged --worktree -- a.sh",
        "git -C sub restore a.sh",
        "git restore --patch -- a.sh",
        "git restore -p a.sh",
        "git restore --pathspec-from-file=x",
        "git restore --pathspec-from-file=x -- a.sh",
        "git restore -- *.sh",
        "git restore -- ':(glob)a.sh'",
        "git restore ':(glob)a.sh'",
        "git restore -- :/a.sh",
        "git restore --staged --worktree",
        "git restore --worktree --",
        "git restore --source",
        "git restore --staged --worktree --source",
        "git restore --overlay -- a.sh",
        "git restore --ignore-unmerged -- a.sh",
    ] {
        let d = restore_run(cmd, Some(restore_in_commits));
        assert_ne!(verdict_name(&d), "allow", "{cmd}: {d:?}");
    }
}

#[test]
fn restore_1604_protected_operands_are_destroy_denied() {
    for cmd in [
        "git restore --staged --worktree -- .githooks/pre-commit",
        "git restore --staged --worktree -- .claude/settings.json",
    ] {
        assert!(
            restore_run(cmd, Some(restore_in_commits)).is_deny(),
            "{cmd}"
        );
    }
}

#[test]
fn restore_1604_not_in_any_commit_is_deny() {
    for cmd in [
        "git restore --staged --worktree -- a.sh",
        "git restore a.sh",
    ] {
        let d = restore_run(cmd, Some(restore_not_in_commit));
        assert!(d.is_deny(), "{cmd}: {d:?}");
    }
}

#[test]
fn restore_1604_undetermined_probe_is_not_allow() {
    for cmd in [
        "git restore --staged --worktree -- a.sh",
        "git restore a.sh",
    ] {
        let d = restore_run(cmd, Some(restore_undetermined));
        assert_ne!(verdict_name(&d), "allow", "{cmd}: {d:?}");
    }
}

#[test]
fn restore_1604_no_probe_attached_is_not_allow() {
    for cmd in [
        "git restore --staged --worktree -- a.sh",
        "git restore a.sh",
    ] {
        let d = restore_run(cmd, None);
        assert_ne!(verdict_name(&d), "allow", "{cmd}: {d:?}");
    }
}

#[test]
fn restore_1604_one_path_not_in_commit_denies_the_whole_command() {
    // Control: the clean paths alone are Allowed by this probe.
    assert_eq!(
        restore_run(
            "git restore --staged --worktree -- a.sh b.sh",
            Some(restore_by_path)
        ),
        Decision::Allow
    );
    for cmd in [
        "git restore --staged --worktree -- a.sh dirty.sh b.sh",
        "git restore --staged --worktree -- dirty.sh a.sh",
        "git restore a.sh b.sh dirty.sh",
    ] {
        let d = restore_run(cmd, Some(restore_by_path));
        assert!(d.is_deny(), "{cmd}: {d:?}");
    }
}

#[test]
fn restore_1604_staged_only_unstage_stays_allow() {
    for probe in [
        None,
        Some(restore_in_commits as crate::reversible::GitRestoreProbe),
        Some(restore_not_in_commit),
        Some(restore_undetermined),
    ] {
        assert_eq!(
            restore_run("git restore --staged a.sh", probe),
            Decision::Allow,
            "probe attached: {}",
            probe.is_some()
        );
    }
}
