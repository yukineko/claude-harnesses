#![allow(clippy::expect_used)]
//! RED (verifier, backlog f3301770, round on 922f960f6 / 0.2.976).
//! `measured_*`: real-shell rebinds (bash 3.2.57 / zsh 5.9, literal -c,
//! </dev/null, 2026-11-28; P=/ok -> /b or 1 means rebound). With `cd /etc`
//! first, a rebind to `1` writes /etc/1.
//!   bash: PS4="\044((P=1))"; set -x; P=/ok; true                -> 1
//!   zsh:  setopt promptsubst; PS3='$((P=1))'; select … <<< 1     -> 1
//!   bash/zsh: X="P=1"; P=/ok; [[ X -eq 1 ]]                      -> 1 / 1
//!   bash: declare -i Y; P=/ok; Y="P=1"                           -> 1
//!   bash: X="P=1"; P=/ok; (( X ))                                -> 1
//!   bash: X="P=1"; P=/ok; let X                                  -> 1
//!   zsh:  noglob trap "P=/b" DEBUG; P=/ok; true                  -> /b
//!   zsh:  nocorrect trap "P=/b" DEBUG; P=/ok; true               -> /b
//!   zsh:  noglob setopt posix_builtins; P=/ok; P=/b :            -> /b
//! `unmeasured_*` and `pending_fp_*`: not measured (relays bgr-90bcb99fc647,
//! bgr-42e4c2aaa25c); UNVERIFIED until main re-runs them.

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
#[ignore = "RED: round4 prebound rebinds"]
fn measured_prompt_and_arithmetic_rebinds_are_never_allow() {
    assert_never_allow(&[
        format!("cd /etc; PS4='\\044((P=1))'; set -x; P={OK}; true; {W}"),
        format!("cd /etc; setopt promptsubst; PS3='$((P=1))'; P={OK}; select x in a; do break; done <<< 1; {W}"),
        format!("cd /etc; X='P=1'; P={OK}; [[ X -eq 1 ]]; {W}"),
        format!("cd /etc; declare -i Y; P={OK}; Y='P=1'; {W}"),
        format!("cd /etc; X='P=1'; P={OK}; (( X )); {W}"),
        format!("cd /etc; X='P=1'; P={OK}; let X; {W}"),
    ]);
}

#[test]
#[ignore = "RED: round4 prebound rebinds"]
fn measured_precommand_modifier_hooks_are_never_allow() {
    assert_never_allow(&[
        format!("noglob trap 'P=/etc/hosts' DEBUG; P={OK}; true; {W}"),
        format!("nocorrect trap 'P=/etc/hosts' DEBUG; P={OK}; true; {W}"),
        format!("noglob setopt posix_builtins; P={OK}; P=/etc/hosts :; {W}"),
    ]);
}

#[test]
#[ignore = "RED: round4 prebound rebinds"]
fn unmeasured_same_family_rows_are_never_allow() {
    assert_never_allow(&[
        format!("cd /etc; PS4='\\44((P=1))'; set -x; P={OK}; true; {W}"),
        format!("- trap 'P=/etc/hosts' DEBUG; P={OK}; true; {W}"),
        format!("- setopt posix_builtins; P={OK}; P=/etc/hosts :; {W}"),
        format!("noglob source /home/yuki/proj/x.sh; P={OK}; true; {W}"),
    ]);
}

#[test]
#[ignore = "RED: round4 prebound rebinds"]
fn pending_fp_non_exit_traps_with_inert_bodies_stay_allow() {
    // Allow on base ace252997, Deny on 922f960f6. The trap bodies bind no
    // name and no signal is sent.
    for c in [
        format!("P={OK}; trap 'echo done' INT; {W}"),
        format!("trap 'rm -f \"$T\"' EXIT INT TERM; P={OK}; {W}"),
    ] {
        let d = scoped(&c);
        assert!(matches!(d, Decision::Allow), "new FP: {c:?} -> {d:?}");
    }
}
