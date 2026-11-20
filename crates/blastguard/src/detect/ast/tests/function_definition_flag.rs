// Test-only module: a failed expectation panics.
#![allow(clippy::panic)]

//! `analyze_with_function_definitions` (task b1-ast-function-definition-flag):
//! the opt-in entry point parses a function definition; `analyze` is unchanged.

use super::*;

const OLD_CAUSE: &str = "function definition: its body is not evaluated";

fn known_fd(command: &str) -> Script {
    match analyze_with_function_definitions(command) {
        Analysis::Known(s) => s,
        Analysis::Undetermined(c) => {
            panic!("expected Known for {command:?}, got Undetermined({c:?})")
        }
    }
}

fn assert_old_unsupported(command: &str) {
    match analyze(command) {
        Analysis::Undetermined(Cause::Unsupported(m)) => {
            assert_eq!(m, OLD_CAUSE, "old cause changed for {command:?}");
        }
        other => panic!("analyze({command:?}) must stay Unsupported, got {other:?}"),
    }
}

fn assert_non_known_both(command: &str) {
    for (entry, a) in [
        ("analyze", analyze(command)),
        (
            "analyze_with_function_definitions",
            analyze_with_function_definitions(command),
        ),
    ] {
        assert!(
            matches!(a, Analysis::Undetermined(_)),
            "{entry}({command:?}) must be non-Known, got {a:?}"
        );
    }
}

const HEREDOC_LINE: &str = "f() { echo hi; }; cat <<'EOF'\nx\nEOF";
const SHADOW_LINE: &str = "cat() { bash; }; ls";
const KEYWORD_LINE: &str = "function g { rm -rf /etc; }";

#[test]
fn analyze_keeps_old_cause_heredoc_line() {
    assert_old_unsupported(HEREDOC_LINE);
}

#[test]
fn analyze_keeps_old_cause_shadowing_line() {
    assert_old_unsupported(SHADOW_LINE);
}

#[test]
fn analyze_keeps_old_cause_function_keyword_line() {
    assert_old_unsupported(KEYWORD_LINE);
}

#[test]
fn analyze_never_sets_the_flag_fields() {
    // A non-function line stays Known with the fields at their defaults.
    let s = known("echo hi");
    assert!(!s.has_function_definition);
    assert!(s.function_names.is_empty());
    let s = known_fd("echo hi");
    assert!(!s.has_function_definition);
    assert!(s.function_names.is_empty());
}

#[test]
fn heredoc_line_is_known_with_flag_name_and_body() {
    let s = known_fd(HEREDOC_LINE);
    assert!(s.has_function_definition);
    assert_eq!(s.function_names, vec!["f".to_string()]);
    // the body's echo and the trailing cat are both present
    assert_eq!(all_named(&s, "echo").len(), 1);
    assert_eq!(all_named(&s, "cat").len(), 1);
}

#[test]
fn shadowing_line_is_known_with_body_command() {
    let s = known_fd(SHADOW_LINE);
    assert!(s.has_function_definition);
    assert_eq!(s.function_names, vec!["cat".to_string()]);
    assert_eq!(all_named(&s, "bash").len(), 1);
    assert_eq!(all_named(&s, "ls").len(), 1);
}

#[test]
fn function_keyword_line_exposes_rm_of_body() {
    let s = known_fd(KEYWORD_LINE);
    assert!(s.has_function_definition);
    assert_eq!(s.function_names, vec!["g".to_string()]);
    let rm = one_named(&s, "rm");
    assert_eq!(arg(rm, 1), &lit("-rf"));
    assert_eq!(arg(rm, 2), &lit("/etc"));
}

#[test]
fn unsupported_construct_in_body_is_non_known_from_both() {
    // coproc is rejected by the subset everywhere (SUBSET.md).
    assert_non_known_both("f() { coproc cat; }");
}

#[test]
fn unsupported_construct_in_body_with_old_entry_point_is_old_cause() {
    // Through `analyze` the definition itself is the reason, unchanged.
    assert_old_unsupported("f() { coproc cat; }");
}

#[test]
fn command_substitution_in_arith_in_body_is_non_known_from_new_entry() {
    match analyze_with_function_definitions("f() { echo $(( $(id -u) + 1 )); }") {
        Analysis::Undetermined(_) => {}
        other => panic!("expected non-Known, got {other:?}"),
    }
}

#[test]
fn nested_definitions_record_both_names_and_bodies() {
    let s = known_fd("f() { g() { rm -rf /etc; }; }");
    assert!(s.has_function_definition);
    assert!(s.function_names.contains(&"f".to_string()), "{s:#?}");
    assert!(s.function_names.contains(&"g".to_string()), "{s:#?}");
    assert_eq!(one_named(&s, "rm").argv.len(), 3);
    assert_old_unsupported("f() { g() { rm -rf /etc; }; }");
}

#[test]
fn duplicate_name_is_recorded_once() {
    let s = known_fd("f() { :; }; f() { :; }");
    assert_eq!(s.function_names, vec!["f".to_string()]);
}

#[test]
fn two_names_recorded_in_order() {
    let s = known_fd("a() { :; }; b() { :; }");
    assert_eq!(s.function_names, vec!["a".to_string(), "b".to_string()]);
}

#[test]
fn redirect_on_definition_is_non_known_from_both() {
    assert_non_known_both("f() { echo hi; } > /tmp/x");
}

#[test]
fn non_literal_name_is_non_known_from_both() {
    assert_non_known_both("read N; ${N}() { rm -rf /etc; }");
    assert_non_known_both("function $N { rm -rf /etc; }");
}

#[test]
fn variable_assigned_later_on_the_line_is_undetermined_in_body() {
    // Either non-Known (the opaque state makes the later literal
    // assignment Unsupported, fail-closed) or Known with $X Undetermined;
    // never Known with $X resolved to /etc.
    match analyze_with_function_definitions("f() { rm -rf $X; }; X=/etc") {
        Analysis::Undetermined(_) => {}
        Analysis::Known(s) => {
            let rm = one_named(&s, "rm");
            assert!(
                is_undet(arg(rm, 2)),
                "late-bound $X must not resolve: {rm:#?}"
            );
        }
    }
}

#[test]
fn variable_assigned_later_never_resolves_in_body_even_unassigned_after() {
    let s = known_fd("f() { rm -rf $X; }");
    let rm = one_named(&s, "rm");
    assert!(is_undet(arg(rm, 2)), "unbound $X in body: {rm:#?}");
}

#[test]
fn variable_assigned_before_definition_is_undetermined_in_body() {
    let s = known_fd("X=/tmp/a; f() { rm -rf $X; }");
    let rm = one_named(&s, "rm");
    assert!(
        is_undet(arg(rm, 2)),
        "late-bound $X must not resolve: {rm:#?}"
    );
}

#[test]
fn state_after_definition_is_opaque() {
    let s = known_fd("X=/tmp/a; f() { :; }; rm -rf $X");
    let rm = one_named(&s, "rm");
    assert!(is_undet(arg(rm, 2)), "a later call may change $X: {rm:#?}");
}

#[test]
fn body_cwd_is_not_the_cd_before_the_definition() {
    let s = known_fd("cd /tmp && f() { rm x; }");
    let rm = one_named(&s, "rm");
    assert!(is_undet(&rm.cwd), "cwd at call time is unknown: {rm:#?}");
}

#[test]
fn control_literal_command_in_body_stays_literal() {
    let s = known_fd("f() { rm -rf /etc; }");
    assert_eq!(arg(one_named(&s, "rm"), 2), &lit("/etc"));
}
