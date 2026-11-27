#![allow(clippy::expect_used)]
//! RED (verifier, backlog f3301770): top-level rebinds that are Allow on tip
//! 801d13518 and on base 994be889d. Each row was measured in a real shell
//! (P=/ok -> /b means rebound), bash 3.2.57 / zsh 5.9, 2026-11-28:
//!   bash: shopt -s expand_aliases; alias echo='read P'<NL>P=/ok; echo <<< /b   -> /b
//!   bash: same, every statement on its own line                                -> /b
//!   bash: alias ls='read P'                                                     -> /b
//!   bash: alias date='printf -v P /b'; date                                     -> /b
//!   bash: set -o posix; P=/ok; P=/b :                                           -> /b
//!   bash: set -o posix; P=/ok; P=/b exec 3>/dev/null                            -> /b
//!   bash: set -o posix; P=/ok; P=/b export Z                                    -> /b
//!   zsh:  setopt posix_builtins; P=/ok; P=/b :                                  -> /b
//!   zsh:  emulate sh; P=/ok; P=/b :                                             -> /b
//! `P=/b eval :` was not measured (UNVERIFIED) and must therefore not be Allow.

use blastguard::detect;
use blastguard::model::Decision;
use blastguard::scope::SafeRoots;
use serde_json::json;

const OK: &str = "/home/yuki/proj/ok.log";

fn identity(p: &str) -> Option<String> {
    Some(p.to_string())
}

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

fn assert_never_allow(cases: &[String]) {
    let mut bad = Vec::new();
    for c in cases {
        let d = scoped(c);
        let h = d.clone().hardened();
        if matches!(d, Decision::Allow) || !h.is_deny() {
            bad.push(format!("main={d:?} sub={h:?} :: {c:?}"));
        }
    }
    assert!(
        bad.is_empty(),
        "rebind resolved as the confined literal:\n{}",
        bad.join("\n")
    );
}

#[test]
fn alias_defined_before_the_binding_whose_body_rebinds_is_never_allow() {
    let w = "echo hi > \"$P\"";
    assert_never_allow(&[
        format!("shopt -s expand_aliases; alias echo='read P'\nP={OK}; echo <<< /etc/hosts; {w}"),
        format!("shopt -s expand_aliases\nalias echo='read P'\nP={OK}\necho <<< /etc/hosts\n{w}"),
        format!("shopt -s expand_aliases; alias ls='read P'\nP={OK}; ls <<< /etc/hosts; {w}"),
        format!("shopt -s expand_aliases; alias date='printf -v P /etc/hosts'\nP={OK}; date; {w}"),
    ]);
}

#[test]
fn posix_special_builtin_prefix_assignment_is_never_allow() {
    let w = "echo hi > \"$P\"";
    assert_never_allow(&[
        format!("set -o posix; P={OK}; P=/etc/hosts :; {w}"),
        format!("P={OK}; set -o posix; P=/etc/hosts exec 3>/dev/null; {w}"),
        format!("set -o posix; P={OK}; P=/etc/hosts export Z; {w}"),
        format!("setopt posix_builtins; P={OK}; P=/etc/hosts :; {w}"),
        format!("emulate sh; P={OK}; P=/etc/hosts :; {w}"),
    ]);
}

#[test]
fn unmeasured_prefix_on_eval_is_never_allow() {
    let w = "echo hi > \"$P\"";
    assert_never_allow(&[
        format!("P={OK}; P=/etc/hosts eval :; {w}"),
        format!("set -o posix; P={OK}; P=/etc/hosts eval :; {w}"),
    ]);
}
