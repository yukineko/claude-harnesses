#![allow(clippy::expect_used)]
//! RED (verifier, backlog f3301770, round on 39a44be73 / 0.2.972): top-level
//! rebinds that still resolve `"$P"` to the confined literal. Real-shell
//! measurements (bash 3.2.57 / zsh 5.9, literal -c, </dev/null, 2026-11-28;
//! P=/ok -> /b means rebound):
//!   bash: shopt -s expand_aliases; \alias echo="read P"<NL>P=/ok; echo <<< /b   -> /b
//!   bash: shopt -s expand_aliases; al""ias echo="read P"<NL>...                 -> /b
//!   bash: shopt -s expand_aliases; . /dev/stdin <<< "alias echo=..."<NL>...     -> /b
//!   bash: echo(){ read P; }; P=/ok; echo <<< /b                                 -> /b
//!   bash: trap 'P=/b' DEBUG; P=/ok; true                                        -> /b
//!   zsh:  TRAPDEBUG(){ P=/b; }; P=/ok; true                                     -> /b
//!   bash: PS4='$((P=1))'; set -x; P=/ok; true                                   -> 1
//!   bash: set -o $'posix'; P=/ok; P=/b :                                        -> /b
//!   zsh:  unsetopt noposixbuiltins; P=/ok; P=/b :                               -> /b
//!   zsh:  unsetopt NO_POSIX_BUILTINS; P=/ok; P=/b :                             -> /b
//!   zsh:  options[posixbuiltins]=on; P=/ok; P=/b :                              -> /b
//!   zsh:  setopt posix_builtins; P=/ok; P=/b typeset Z                          -> /b
//! The `unmeasured_*` rows could not be measured here (eval / run-time command
//! words are refused by blastguard; bash 4+ for BASH_ALIASES is not installed):
//! they are undetermined and must therefore not be Allow either.

use blastguard::detect;
use blastguard::model::Decision;
use blastguard::scope::SafeRoots;
use serde_json::json;

const OK: &str = "/home/yuki/proj/ok.log";
const W: &str = "echo hi > \"$P\"";

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
    assert!(bad.is_empty(), "rebind resolved as the confined literal:\n{}", bad.join("\n"));
}

#[test]
#[ignore = "RED: alias spellings (\\alias, al''ias, . /dev/stdin) before P= still resolve \"$P\" to the stale confined value"]
fn measured_alias_spellings_before_the_binding_are_never_allow() {
    assert_never_allow(&[
        format!("shopt -s expand_aliases; \\alias echo='read P'\nP={OK}; echo <<< /etc/hosts; {W}"),
        format!("shopt -s expand_aliases; al''ias echo='read P'\nP={OK}; echo <<< /etc/hosts; {W}"),
        format!("shopt -s expand_aliases; . /dev/stdin <<< \"alias echo='read P'\"\nP={OK}; echo <<< /etc/hosts; {W}"),
    ]);
}

#[test]
#[ignore = "RED: a prebound function / trap / TRAPDEBUG / PS4 hook still resolves \"$P\" to the stale confined value"]
fn measured_prebound_function_and_hooks_are_never_allow() {
    assert_never_allow(&[
        format!("echo(){{ read P; }}; P={OK}; echo <<< /etc/hosts; {W}"),
        format!("trap 'P=/etc/hosts' DEBUG; P={OK}; true; {W}"),
        format!("TRAPDEBUG(){{ P=/etc/hosts; }}; P={OK}; true; {W}"),
        format!("cd /etc; PS4='$((P=1))'; set -x; P={OK}; true; {W}"),
    ]);
}

#[test]
#[ignore = "RED: undetected POSIX-enabling spellings still let a special-builtin prefix assignment resolve stale"]
fn measured_posix_enabling_spellings_are_never_allow() {
    assert_never_allow(&[
        format!("set -o $'posix'; P={OK}; P=/etc/hosts :; {W}"),
        format!("unsetopt noposixbuiltins; P={OK}; P=/etc/hosts :; {W}"),
        format!("unsetopt NO_POSIX_BUILTINS; P={OK}; P=/etc/hosts :; {W}"),
        format!("options[posixbuiltins]=on; P={OK}; P=/etc/hosts :; {W}"),
        format!("setopt posix_builtins; P={OK}; P=/etc/hosts typeset Z; {W}"),
    ]);
}

#[test]
#[ignore = "RED: undetermined alias/option definitions still resolve \"$P\" to the stale confined value"]
fn unmeasured_undetermined_definitions_are_never_allow() {
    assert_never_allow(&[
        format!("shopt -s expand_aliases; A=alias; $A echo='read P'\nP={OK}; echo <<< /etc/hosts; {W}"),
        format!("shopt -s expand_aliases; eval \"alias echo='read P'\"\nP={OK}; echo <<< /etc/hosts; {W}"),
        format!("shopt -s expand_aliases; source /home/yuki/proj/env.sh\nP={OK}; echo <<< /etc/hosts; {W}"),
        format!("shopt -s expand_aliases; BASH_ALIASES[echo]='read P'\nP={OK}; echo <<< /etc/hosts; {W}"),
        format!("set -o $(echo posix); P={OK}; P=/etc/hosts :; {W}"),
    ]);
}
