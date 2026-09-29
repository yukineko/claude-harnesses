// このファイルは丸ごと integration test なので expect/unwrap を許可する。
#![allow(clippy::expect_used)]
//! RED tests for the remaining part of backlog daf7611b: code-executing
//! programs other than sh/bash/zsh/ksh/dash and the -c shells (fish/pwsh/csh
//! were closed by faf9136f). Written by an agent that did not write the fix
//! (CLAUDE.md 2(a)).
//!
//! Contract under test: an executor whose payload blastguard cannot analyse
//! must not be silently Allowed. A recognisable destructive payload (rm -rf of
//! a user path) is Deny; an unanalysable one is Ask. PROBES fail until the fix
//! lands; CONTROLS pass today so the file is not vacuous.

use blastguard::detect;
use blastguard::model::Decision;
use serde_json::json;

fn bash(cmd: &str) -> Decision {
    detect::detect("Bash", Some(&json!({ "command": cmd })))
}

fn not_allow(cmd: &str) {
    let d = bash(cmd);
    assert!(
        !matches!(d, Decision::Allow),
        "must be Deny or Ask, got Allow for: {cmd}"
    );
}

fn deny(cmd: &str) {
    let d = bash(cmd);
    assert!(d.is_deny(), "must be Deny, got {d:?} for: {cmd}");
}

fn allow(cmd: &str) {
    let d = bash(cmd);
    assert!(
        matches!(d, Decision::Allow),
        "control must stay Allow, got {d:?} for: {cmd}"
    );
}

#[test]
fn daf7611b_control_bare_and_known_shell_forms_are_denied() {
    deny("rm -rf ~/x");
    not_allow(r#"sh -c "rm -rf ~/x""#);
}

#[test]
fn daf7611b_git_dash_c_config_payload_not_allowed() {
    not_allow(r#"git -c core.pager="rm -rf ~/x" log"#);
}

#[test]
fn daf7611b_deno_eval_payload_not_allowed() {
    not_allow(r#"deno eval "Deno.removeSync(Deno.env.get('HOME') + '/x', {recursive: true})""#);
}

#[test]
fn daf7611b_bun_eval_payload_not_allowed() {
    not_allow(
        r#"bun -e "require('fs').rmSync(process.env.HOME + '/x', {recursive: true, force: true})""#,
    );
}

#[test]
fn daf7611b_osascript_do_shell_script_payload_is_denied() {
    deny(r#"osascript -e 'do shell script "rm -rf ~/x"'"#);
}

#[test]
fn daf7611b_awk_system_payload_is_denied() {
    deny(r#"awk 'BEGIN{system("rm -rf ~/x")}'"#);
}

#[test]
fn daf7611b_env_dash_s_payload_is_denied() {
    deny(r#"env -S "rm -rf ~/x""#);
}

// Controls: benign uses of the same programs must not become friction.
#[test]
fn daf7611b_controls_benign_forms_stay_allow() {
    allow("git -c color.ui=always log");
    allow("git -c core.pager=less log");
    allow("awk '{print $1}' file");
    allow("env FOO=1 ls");
    allow("deno --version");
    allow(r#"osascript -e 'display notification "hi"'"#);
}
