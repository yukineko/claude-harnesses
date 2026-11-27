#![allow(clippy::expect_used)]
//! Top-level `"$P"` rebinding fail-open (system harm), found by an independent
//! verifier on 24263a0c8.
//!
//! At TOP LEVEL (not inside a function) blastguard resolves `> "$P"` to the
//! earlier static assignment `P=<confined path>` and answers Allow, although a
//! statement in between rebinds P, so the real write lands on `/etc/hosts`.
//!
//! Shell behaviour measured on this machine (bash 3.2.57, zsh, harmless
//! literal paths `/a` -> `/b`): rebinding CONFIRMED for bash: `IFS= read -r P
//! <<<`, `P[0]=`, `printf -v`, `command printf -v`, `builtin read`, `trap
//! 'P=..' DEBUG`; for zsh: `P[1,-1]=`, `echo .. | read P`, `X=1 printf -v`,
//! `path=`/`PATH=` tie, `print -v`, `print -vP`, `zformat -f`, `zstat -A`,
//! `sysread`, `zparseopts -A`, `{P}>` (P became fd 11 in `echo {P}>` and
//! `exec {P}>`). NOT measured: bash `{P}>` (bash 3.2 has no `{var}` redirect),
//! `vared P` (needs a tty).

use blastguard::detect;
use blastguard::model::Decision;
use blastguard::scope::SafeRoots;
use serde_json::json;

const PROJECT: &str = "/home/yuki/proj";
const HOME: &str = "/home/yuki";
const OK: &str = "/home/yuki/proj/ok.log";

fn identity(p: &str) -> Option<String> {
    Some(p.to_string())
}

fn roots() -> SafeRoots {
    SafeRoots::new(
        Some(PROJECT),
        Some(PROJECT),
        Some(HOME),
        None,
        Some(identity),
    )
}

fn scoped(cmd: &str) -> Decision {
    detect::detect_scoped("Bash", Some(&json!({ "command": cmd })), &roots())
}

#[test]
fn control_resolution_still_allows_confined_and_denies_system() {
    let ok = format!("P={OK}; echo hi > \"$P\"");
    assert!(
        matches!(scoped(&ok), Decision::Allow),
        "control: confined assignment must be Allow, got {:?}",
        scoped(&ok)
    );
    let bad = "P=/etc/hosts; echo hi > \"$P\"";
    assert!(
        matches!(scoped(bad), Decision::Deny(..)),
        "control: system target must be Deny, got {:?}",
        scoped(bad)
    );
}

#[test]
fn toplevel_rebind_between_assignment_and_redirect_is_never_allow() {
    let cases: Vec<String> = vec![
        format!("P={OK}; IFS= read -r P <<< /etc/hosts; echo hi > \"$P\""),
        format!("P={OK}; P[0]=/etc/hosts; echo hi > \"$P\""),
        format!("P={OK}; P[1,-1]=/etc/hosts; echo hi > \"$P\""),
        format!("P={OK}; echo /etc/hosts | read P; echo hi > \"$P\""),
        format!("P={OK}; X=1 printf -v P /etc/hosts; echo hi > \"$P\""),
        format!("P={OK}; command printf -v P /etc/hosts; echo hi > \"$P\""),
        format!("P={OK}; builtin read P <<< /etc/hosts; echo hi > \"$P\""),
        format!("P={OK}; trap 'P=/etc/hosts' DEBUG; echo hi > \"$P\""),
        format!("P={OK}; cd /etc; echo {{P}}>/dev/null; echo hi > \"$P\""),
        format!("P={OK}; exec {{P}}>/dev/null; echo hi > \"$P\""),
        format!("path={OK}; PATH=/etc/hosts; echo hi > \"$path\""),
        format!("P={OK}; print -v P /etc/hosts; echo hi > \"$P\""),
        format!("P={OK}; print -vP P /etc/hosts; echo hi > \"$P\""),
        format!("P={OK}; zformat -f P /etc/hosts; echo hi > \"$P\""),
        format!("P={OK}; zstat -A P /etc; echo hi > \"$P\""),
        format!("P={OK}; sysread P < /etc/hosts; echo hi > \"$P\""),
        format!("P={OK}; vared P; echo hi > \"$P\""),
        format!("P={OK}; zparseopts -A P; echo hi > \"$P\""),
    ];
    let mut allowed = Vec::new();
    for c in &cases {
        let d = scoped(c);
        if matches!(d, Decision::Allow) {
            allowed.push(format!("ALLOW (fail-open): {c}"));
        }
    }
    assert!(
        allowed.is_empty(),
        "{} of {} rebinding cases are Allow:\n{}",
        allowed.len(),
        cases.len(),
        allowed.join("\n")
    );
}
