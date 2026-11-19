//! `dash_c_payloads` reads one candidate per shell WORD after `-c`, never one
//! starting inside a word whose quotes all close (task ab2097c5, see
//! `word_payloads_after`). The `/bin/sh -c` twins of the controls in
//! `tests/wait_bang_argument_fp_20261114.rs`, plus the candidate set itself.

use super::dash_c_payloads;
use crate::detect::detect;
use crate::model::Decision;
use serde_json::json;

fn bash(cmd: &str) -> Decision {
    detect("Bash", Some(&json!({ "command": cmd })))
}

#[test]
fn quoted_dash_c_operand_yields_no_mid_word_candidate() {
    let src = "-c 'sleep 1 & wait $!'";
    let rest: Vec<&str> = src.split_whitespace().collect();
    let got = dash_c_payloads(&rest, Some(src));
    assert_eq!(got, vec!["sleep 1 & wait $!".to_string()], "{got:?}");
}

#[test]
fn unquoted_multi_word_operand_keeps_every_word_as_a_candidate() {
    let src = "-c rm -rf /some/path";
    let rest: Vec<&str> = src.split_whitespace().collect();
    let got = dash_c_payloads(&rest, Some(src));
    for want in ["rm", "-rf", "/some/path", "rm -rf /some/path"] {
        assert!(got.iter().any(|p| p == want), "{want} missing: {got:?}");
    }
}

#[test]
fn sh_c_wait_bang_followed_by_system_harm_is_denied() {
    let d = bash("/bin/sh -c 'sleep 1 & wait $!; rm -rf /etc'");
    assert!(d.is_deny(), "{d:?}");
}

#[test]
fn sh_c_bang_in_command_position_is_not_allowed() {
    let d = bash("/bin/sh -c 'sleep 1 & $! foo'");
    assert_ne!(d, Decision::Allow);
}

#[test]
fn sh_c_wait_bang_is_allowed() {
    assert_eq!(bash("/bin/sh -c 'sleep 1 & wait $!'"), Decision::Allow);
}
