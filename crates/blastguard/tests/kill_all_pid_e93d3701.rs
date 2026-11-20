#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Backlog e93d3701 (spec 596ef57d Q5): `kill` aimed at pid `-1` is Deny.
//!
//! These are the implementer's own deny variants and allow controls for the
//! narrow `detect::kill` rule. The independent requirement test is `r_kill` in
//! `backlog_20261120_596ef57d_requirements.rs`.

use blastguard::detect::detect;
use blastguard::model::Decision;
use blastguard::rule_id::rule_id;
use serde_json::json;

const REASON: &str = "kill aimed at pid -1 signals every process the user owns (system harm)";

fn bash(cmd: &str) -> Decision {
    detect("Bash", Some(&json!({ "command": cmd })))
}

#[test]
fn kill_all_variants_deny() {
    for cmd in [
        "kill -9 -1",
        "kill -- -1",
        "kill -s KILL -1",
        "kill -KILL -1",
        "kill -SIGKILL -1",
        "kill -n 9 -1",
        "kill --signal KILL -1",
        "kill --signal=KILL -1",
        "kill -sKILL -1",
        "kill -TERM -- -1",
        "kill -1 -1",
        "kill -9 -01",
        "kill 1234 -1",
        "kill -9 '-1'",
        "/bin/kill -9 -1",
        "kill -9 -1 2>/dev/null",
    ] {
        match bash(cmd) {
            Decision::Deny(reason) => assert_eq!(reason, REASON, "{cmd}"),
            other => panic!("{cmd}: expected Deny, got {other:?}"),
        }
    }
}

#[test]
fn kill_ordinary_forms_allow() {
    for cmd in [
        "kill 1234",
        "kill -9 1234",
        "kill -TERM %1",
        "kill -l",
        "kill -l -1",
        "kill -0 $$",
        "kill -0 -1",
        "kill -1 1234",
        "kill -1",
        "kill -9 -1234",
        "kill -9 -10",
        "kill -9 $pid",
    ] {
        assert_eq!(bash(cmd), Decision::Allow, "{cmd}");
    }
}

#[test]
fn kill_all_reason_has_stable_rule_id() {
    assert_eq!(rule_id(REASON), "kill-all");
}
