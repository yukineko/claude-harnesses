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
