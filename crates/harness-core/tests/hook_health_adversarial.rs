#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Adversarial tests for hook-health binary extraction. No env/cwd mutation.

use harness_core::hook::{extract_binary_path, missing_hook_binaries};
use serde_json::json;

const NOPE: &str = "/nonexistent-zz9/missing.sh";

fn flagged(cmd: &str, root: Option<&str>) -> Vec<String> {
    let s = json!({"hooks":{"Stop":[{"hooks":[{"type":"command","command":cmd}]}]}});
    missing_hook_binaries(&s, root)
        .into_iter()
        .map(|m| m.binary_path)
        .collect()
}

fn existing_abs() -> String {
    format!("{}/Cargo.toml", env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn abs_interpreter_missing_script_flagged() {
    assert_eq!(flagged(&format!("/bin/bash {NOPE}"), None), vec![NOPE]);
}

#[test]
fn bare_interpreter_missing_script_flagged_with_script_path() {
    assert_eq!(flagged(&format!("bash {NOPE}"), None), vec![NOPE]);
    assert_eq!(flagged(&format!("python3 -u {NOPE}"), None), vec![NOPE]);
}

#[test]
fn existing_script_not_flagged() {
    assert!(flagged(&format!("bash {}", existing_abs()), None).is_empty());
}

#[test]
fn interpreter_itself_never_flagged_when_script_exists() {
    assert!(flagged(&format!("sh -x {}", existing_abs()), None).is_empty());
}

#[test]
fn relative_script_not_checked() {
    assert!(flagged("bash hooks/does-not-exist.sh", None).is_empty());
    assert!(flagged("bash ./does-not-exist.sh", None).is_empty());
}

#[test]
fn inline_code_not_checked() {
    assert!(flagged("bash -c 'echo hi'", None).is_empty());
    assert!(flagged("python3 -c 'print(1)'", None).is_empty());
    assert!(flagged("python3 -m json.tool", None).is_empty());
}

#[test]
fn plugin_root_script_arg_without_root_unchecked() {
    assert!(flagged("bash ${CLAUDE_PLUGIN_ROOT}/x.sh", None).is_empty());
}

#[test]
fn plugin_root_script_arg_with_root_flagged() {
    assert_eq!(
        flagged(
            "bash ${CLAUDE_PLUGIN_ROOT}/x.sh",
            Some("/nonexistent-zz9/root")
        ),
        vec!["/nonexistent-zz9/root/x.sh"]
    );
}

#[test]
fn plugin_root_direct_command() {
    assert!(flagged("${CLAUDE_PLUGIN_ROOT}/bin/x", None).is_empty());
    assert_eq!(
        flagged("${CLAUDE_PLUGIN_ROOT}/bin/x", Some("/nonexistent-zz9/r")),
        vec!["/nonexistent-zz9/r/bin/x"]
    );
}

#[test]
fn relative_plugin_root_script_unchecked() {
    assert_eq!(
        extract_binary_path("bash ${CLAUDE_PLUGIN_ROOT}/x.sh", Some("rel")),
        None
    );
}

#[test]
fn bare_name_not_on_path_flagged_by_name() {
    assert_eq!(
        flagged("definitely-not-a-cmd-zz9 --x", None),
        vec!["definitely-not-a-cmd-zz9"]
    );
}

#[test]
fn bare_name_on_path_not_flagged_and_resolved_absolute() {
    assert!(flagged("sh", None).is_empty());
    let p = extract_binary_path("sh", None).unwrap();
    assert!(p.starts_with('/'), "{p}");
}

#[test]
fn cwd_independence_bare_name_matching_cwd_file() {
    // cargo runs integration tests with cwd = crate dir, where Cargo.toml exists.
    assert!(std::path::Path::new("Cargo.toml").exists());
    assert_eq!(flagged("Cargo.toml", None), vec!["Cargo.toml"]);
}

#[test]
fn cwd_independence_relative_slash_path() {
    // Ideal: `./x` is cwd-relative and must not be existence-checked against checker cwd.
    assert_eq!(extract_binary_path("./Cargo.toml", None), None);
}

#[test]
fn bash_dash_e_is_errexit_not_inline() {
    assert_eq!(flagged(&format!("bash -e {NOPE}"), None), vec![NOPE]);
}

#[test]
fn perl_dash_e_is_inline() {
    assert!(flagged("perl -e 'print 1'", None).is_empty());
}

#[test]
fn bash_option_with_value_o_pipefail() {
    assert_eq!(
        flagged(&format!("bash -o pipefail {NOPE}"), None),
        vec![NOPE]
    );
}

#[test]
fn clustered_inline_flag_lc() {
    assert!(flagged(&format!("bash -lc 'echo hi' {NOPE}"), None).is_empty());
}

#[test]
fn glued_module_flag() {
    assert!(flagged(&format!("python3 -mjson.tool {NOPE}"), None).is_empty());
}

#[test]
fn versioned_interpreter_checks_script() {
    assert_eq!(flagged(&format!("python3.12 {NOPE}"), None), vec![NOPE]);
}

#[test]
fn env_wrapper_checks_script() {
    assert_eq!(
        flagged(&format!("/usr/bin/env bash {NOPE}"), None),
        vec![NOPE]
    );
}

#[test]
fn env_assignment_prefix_not_false_flagged() {
    assert!(flagged("FOO=1 sh", None).is_empty());
}

#[test]
fn shell_builtin_not_false_flagged() {
    assert!(flagged("cd /tmp && sh -c true", None).is_empty());
}

#[test]
fn quoted_plugin_root_without_root_unchecked() {
    assert!(flagged("\"${CLAUDE_PLUGIN_ROOT}/bin/x\"", None).is_empty());
}

#[test]
fn tilde_and_home_var_expanded_like_the_shell() {
    // The shell expands `~` / `$HOME` before exec, so the check must judge the
    // EXPANDED path — not the literal (false positive for an existing target)
    // and not "unchecked" (a genuinely missing target would go unreported).
    // `~/definitely-missing-zz9/x` really is missing, so it IS flagged, under
    // its expanded name; an existing target under $HOME is not.
    let home = std::env::var("HOME").unwrap();
    let want = format!("{home}/definitely-missing-zz9/x");
    assert_eq!(
        flagged("~/definitely-missing-zz9/x", None),
        vec![want.clone()]
    );
    assert_eq!(flagged("$HOME/definitely-missing-zz9/x", None), vec![want]);
    assert!(flagged("~", None).is_empty(), "$HOME itself exists");
    // An unresolvable variable is never existence-checked as a literal.
    assert!(flagged("$NOT_A_KNOWN_ROOT/x", None).is_empty());
}

#[test]
fn bare_interpreter_with_only_flags_checks_interpreter() {
    assert!(flagged("bash --version", None).is_empty());
    assert!(flagged("bash -", None).is_empty());
}

#[test]
fn double_dash_then_script() {
    assert_eq!(flagged(&format!("bash -- {NOPE}"), None), vec![NOPE]);
}

#[test]
fn empty_and_whitespace_commands() {
    assert!(flagged("", None).is_empty());
    assert!(flagged("   \t ", None).is_empty());
}
