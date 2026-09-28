// このファイルは丸ごと integration test なので expect/unwrap を許可する。
#![allow(clippy::expect_used)]
//! RED tests for four p0 backlog items (daf7611b, 06345780, a83802ad,
//! f1c170ab). Written by an agent that did not write the fixes (CLAUDE.md
//! 2(a)). Each item has CONTROL cases that are correct today (so the test is
//! not vacuous) and PROBE cases that must FAIL until the fix lands: the
//! probes assert the verdict is not `Allow` / equals the control's verdict.

use blastguard::detect;
use blastguard::model::Decision;
use blastguard::scope::SafeRoots;
use serde_json::json;

fn identity(p: &str) -> Option<String> {
    Some(p.to_string())
}

/// The hook's real entry point (it knows the session cwd/project).
fn scoped(cmd: &str) -> Decision {
    let roots = SafeRoots::new(
        Some("/home/yuki/proj"),
        Some("/home/yuki/proj"),
        Some("/home/yuki"),
        None,
        Some(identity),
    );
    detect::detect_scoped("Bash", Some(&json!({ "command": cmd })), &roots)
}

fn bash(cmd: &str) -> Decision {
    detect::detect("Bash", Some(&json!({ "command": cmd })))
}

fn rank(d: &Decision) -> u8 {
    match d {
        Decision::Allow => 0,
        Decision::Ask(_) => 1,
        Decision::Deny(_) => 2,
    }
}

fn not_allow(cmd: &str) {
    let d = bash(cmd);
    assert!(
        !matches!(d, Decision::Allow),
        "must be Deny or Ask, got Allow for: {cmd}"
    );
}

// ---------------------------------------------------------------- daf7611b

#[test]
fn daf7611b_control_known_shell_quoted_payload_not_allowed() {
    not_allow(r#"sh -c "rm -rf /Users/yuki/src""#);
    not_allow(r#"bash -c "rm -rf /Users/yuki/src""#);
    not_allow("rm -rf /Users/yuki/src");
}

#[test]
fn daf7611b_unknown_shell_quoted_payload_is_not_allowed() {
    for sh in ["fish", "pwsh", "tcsh", "csh"] {
        not_allow(&format!(r#"{sh} -c "rm -rf /Users/yuki/src""#));
    }
}

// ---------------------------------------------------------------- 06345780

#[test]
fn c06345780_control_bare_destructive_is_not_allowed() {
    not_allow("rm -rf ~/work");
}

#[test]
fn c06345780_command_substitution_in_operand_is_judged_like_bare() {
    for cmd in [
        "echo $(rm -rf ~/work)",
        "cat $(rm -rf ~/work)",
        "cat `rm -rf ~/work`",
        "echo `rm -rf ~/work`",
    ] {
        not_allow(cmd);
    }
}

// ---------------------------------------------------------------- a83802ad

#[test]
fn a83802ad_control_literal_and_truncate_forms_are_not_allowed() {
    not_allow("echo x >> /etc/fstab");
    not_allow("P=/etc/fstab; echo x > $P");
}

#[test]
fn a83802ad_append_via_variable_matches_truncate_via_variable() {
    let trunc = bash("P=/etc/fstab; echo x > $P");
    let append = bash("P=/etc/fstab; echo x >> $P");
    assert!(
        !matches!(append, Decision::Allow),
        "append via variable must not be Allow (truncate form is {trunc:?})"
    );
    assert_eq!(
        rank(&append),
        rank(&trunc),
        "`>>` and `>` through the same variable must agree: {append:?} vs {trunc:?}"
    );
}

// ---------------------------------------------------------------- f1c170ab

#[test]
fn f1c170ab_control_absolute_system_dir_redirect_is_denied() {
    assert!(scoped("echo x > /etc/paths.d/zz-evil").is_deny());
    assert!(scoped("cd /tmp && echo x > /etc/paths.d/zz-evil").is_deny());
}

#[test]
fn f1c170ab_cd_then_relative_redirect_matches_absolute_form() {
    for cmd in [
        "cd /etc && echo x > paths.d/zz-evil",
        "cd /etc/paths.d && echo x > zz-evil",
    ] {
        let d = scoped(cmd);
        assert!(
            !matches!(d, Decision::Allow),
            "cd + relative redirect into a system dir must not be Allow: {cmd}"
        );
    }
}
