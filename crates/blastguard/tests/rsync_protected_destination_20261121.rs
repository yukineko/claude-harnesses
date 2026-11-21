// このファイルは丸ごと integration test なので expect/unwrap を許可する。
#![allow(clippy::expect_used)]
//! `rsync`'s local destination operand is a write target: a protected path
//! there must be judged like `cp`'s (protected_path_block /
//! protected_landing_block). Remote destinations and ordinary local
//! destinations stay Allow; a remote SOURCE does not matter.

use blastguard::detect;
use blastguard::model::Decision;
use serde_json::json;

fn bash(cmd: &str) -> Decision {
    detect::detect("Bash", Some(&json!({ "command": cmd })))
}

fn assert_allow(cmd: &str) {
    let d = bash(cmd);
    assert!(
        matches!(d, Decision::Allow),
        "must be Allow, got {d:?} for: {cmd}"
    );
}

fn assert_not_allow(cmd: &str) {
    let d = bash(cmd);
    assert!(
        !matches!(d, Decision::Allow),
        "must be Deny or Ask, got Allow for: {cmd}"
    );
}

#[test]
fn allow_tmp_destination() {
    assert_allow("rsync -a src/ /tmp/x/");
}

#[test]
fn allow_relative_build_destination() {
    assert_allow("rsync -a src/ ./build/");
}

#[test]
fn allow_remote_host_destination() {
    assert_allow("rsync -a src/ host:/srv/x");
}

#[test]
fn allow_remote_source_local_tmp_destination() {
    assert_allow("rsync host:.githooks/pre-commit /tmp/x");
}

#[test]
fn not_allow_protected_file_destination() {
    assert_not_allow("rsync -a x .githooks/pre-commit");
}

#[test]
fn not_allow_git_hooks_dir_destination() {
    assert_not_allow("rsync x .git/hooks/");
}

// A value-taking option AFTER the operands must not have its value read as
// the destination (orchestrator probe 2026-11-21: `--bwlimit 100` masked
// `.githooks/pre-commit`).

#[test]
fn not_allow_protected_destination_bwlimit_after_operands() {
    assert_not_allow("rsync src/ .githooks/pre-commit --bwlimit 100");
}

#[test]
fn not_allow_protected_destination_bwlimit_eq_after_operands() {
    assert_not_allow("rsync src/ .githooks/pre-commit --bwlimit=100");
}

#[test]
fn not_allow_git_hooks_destination_usermap_after_operands() {
    assert_not_allow("rsync -a src/ .git/hooks/ --usermap a:b");
}

#[test]
fn allow_tmp_destination_bwlimit_after_operands() {
    assert_allow("rsync -a src/ /tmp/x/ --bwlimit 100");
}

#[test]
fn allow_build_destination_modify_window_after_operands() {
    assert_allow("rsync src/ ./build/ --modify-window 1");
}

#[test]
fn not_allow_protected_destination_short_value_cluster_after_operands() {
    assert_not_allow("rsync src/ .githooks/pre-commit -ae ssh");
}

#[test]
fn not_allow_protected_candidate_behind_unknown_option() {
    // `--frobnicate` is not an rsync option blastguard knows: it may take
    // `1` as its value, so `.githooks/pre-commit` may be the destination.
    assert_not_allow("rsync src/ .githooks/pre-commit --frobnicate 1");
}

#[test]
fn not_allow_delete_home_destination_bwlimit_after_operands() {
    assert_not_allow("rsync -a --delete /tmp/empty/ ~/ --bwlimit 100");
}
