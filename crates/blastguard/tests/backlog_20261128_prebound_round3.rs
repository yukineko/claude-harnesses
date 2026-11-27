#![allow(clippy::expect_used)]
//! RED (verifier, backlog f3301770, round on d4c4d34a8 / 0.2.974).
//! `measured_*`: real-shell rebinds (bash 3.2.57 / zsh 5.9, literal -c,
//! </dev/null, 2026-11-28; P=/ok -> /b means rebound):
//!   bash: export POSIXLY_CORRECT=1; P=/ok; P=/b :            -> /b
//!   bash: declare POSIXLY_CORRECT=1; P=/ok; P=/b :           -> /b
//!   zsh:  setopt -m "*builtins"; P=/ok; P=/b :               -> /b
//!   zsh:  x echo () { read P; }; P=/ok; echo <<< /b          -> /b
//!   zsh:  chpwd(){ P=/b; }; P=/ok; cd /tmp                   -> /b
//! `unmeasured_*`: same families, NOT measured (the verifier's measurement
//! was stopped by two blastguard denials, relays bgr-da4de2423f80 and
//! bgr-9e98dd069c20). UNVERIFIED: decide after main re-runs them.

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
    assert!(
        bad.is_empty(),
        "rebind resolved as the confined literal:\n{}",
        bad.join("\n")
    );
}

#[test]
fn measured_posix_and_hook_rebinds_are_never_allow() {
    assert_never_allow(&[
        format!("export POSIXLY_CORRECT=1; P={OK}; P=/etc/hosts :; {W}"),
        format!("declare POSIXLY_CORRECT=1; P={OK}; P=/etc/hosts :; {W}"),
        format!("setopt -m '*builtins'; P={OK}; P=/etc/hosts :; {W}"),
        format!("x echo () {{ read P; }}; P={OK}; echo <<< /etc/hosts; {W}"),
        format!("chpwd(){{ P=/etc/hosts; }}; P={OK}; cd /home/yuki/proj; {W}"),
    ]);
}

#[test]
fn unmeasured_same_family_rows_are_never_allow() {
    assert_never_allow(&[
        format!("typeset POSIXLY_CORRECT=1; P={OK}; P=/etc/hosts :; {W}"),
        format!("read POSIXLY_CORRECT <<< 1; P={OK}; P=/etc/hosts :; {W}"),
        format!("printf -v POSIXLY_CORRECT 1; P={OK}; P=/etc/hosts :; {W}"),
        format!("setopt -m 'p*ins'; P={OK}; P=/etc/hosts :; {W}"),
        format!("command -p set -o posix; P={OK}; P=/etc/hosts :; {W}"),
        format!(":(){{ P=/etc/hosts; }}; P={OK}; :; {W}"),
        format!("g(){{ P=/etc/hosts; }}; P={OK}; \\g; {W}"),
        format!("g(){{ P=/etc/hosts; }}; P={OK}; 'g'; {W}"),
        format!("chpwd_functions+=(g); g(){{ P=/etc/hosts; }}; P={OK}; cd /home/yuki/proj; {W}"),
        format!("cd /etc; declare PS4='$((P=1))'; set -x; P={OK}; true; {W}"),
        format!("cd /etc; export PS4='$((P=1))'; set -x; P={OK}; true; {W}"),
        format!("cd /etc; read PS4 <<< '$((P=1))'; set -x; P={OK}; true; {W}"),
        format!("cd /etc; printf -v PS4 '$((P=1))'; set -x; P={OK}; true; {W}"),
        format!("command -p trap 'P=/etc/hosts' DEBUG; P={OK}; true; {W}"),
        format!("shopt -s expand_aliases; command -p alias echo='read P'\nP={OK}; echo <<< /etc/hosts; {W}"),
    ]);
}

// Round 3 additions (worker; relays bgr-da4de2423f80 / bgr-9e98dd069c20 measured
// printf-fn cd-fn true-fn iffn colonfn bsg qg declPS4 expPS4 readPS4 printfPS4
// declPC expPC typesetPC readPC printfPC zsetoptm zsetoptm2 cmdptrap cmdpset
// cmdpalias zchpwd zchpwdfns zmultiname as rebinding; `command g`, precmd,
// preexec and pipe-last-bash measured NOT rebinding).
#[test]
fn measured_function_named_like_builtin_or_quoted_call_is_never_allow() {
    assert_never_allow(&[
        format!("printf(){{ P=/etc/hosts; }}; P={OK}; printf x; {W}"),
        format!("cd(){{ P=/etc/hosts; }}; P={OK}; cd /home/yuki/proj; {W}"),
        format!("true(){{ P=/etc/hosts; }}; P={OK}; true; {W}"),
        format!("if true; then g(){{ P=/etc/hosts; }}; fi; P={OK}; g; {W}"),
        format!(":(){{ P=/etc/hosts; }}; P={OK}; :; {W}"),
        format!("g(){{ P=/etc/hosts; }}; P={OK}; \\g; {W}"),
        format!("g(){{ P=/etc/hosts; }}; P={OK}; 'g'; {W}"),
        format!("export PS4='$((P=1))'; set -x; P={OK}; true; {W}"),
        format!("typeset POSIXLY_CORRECT=1; P={OK}; P=/etc/hosts :; {W}"),
        format!("setopt -m 'p*ins'; P={OK}; P=/etc/hosts :; {W}"),
        format!("chpwd_functions+=(g); g(){{ P=/etc/hosts; }}; P={OK}; cd /home/yuki/proj; {W}"),
    ]);
}

#[test]
fn removed_false_positives_stay_allow() {
    let mut bad = Vec::new();
    for c in [
        format!("alias ll='ls -l'; P={OK}; {W}"),
        format!("alias ll='ls -l'\nP={OK}; {W}"),
        format!("trap 'echo bye' EXIT; P={OK}; {W}"),
        format!("trap - EXIT; P={OK}; {W}"),
        format!("PS4='+ '; set -x; P={OK}; {W}"),
    ] {
        let d = scoped(&c);
        if !matches!(d, Decision::Allow) {
            bad.push(format!("{d:?} :: {c:?}"));
        }
    }
    assert!(
        bad.is_empty(),
        "false positive vs base:\n{}",
        bad.join("\n")
    );
}
