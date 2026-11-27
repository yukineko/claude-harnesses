#![allow(clippy::expect_used)]
//! RED (verifier, 2026-11-28, tip a98ffbfaa / blastguard 0.2.963): top-level
//! rebinding of P that blastguard still resolves to the stale confined value
//! (main Allow, subagent Allow). Real rebinding measured with literal
//! `zsh -c` / `bash -c` (zsh 5.9, bash 3.2.57, P=/ok -> /b). Also Allow on
//! base 994be889d (pre-existing, not introduced by the branch).
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

#[test]
fn residual_toplevel_rebinds_are_never_allow() {
    let shapes = [
        "case ${P::=/etc/hosts} in *) ;; esac",           // zsh: /b
        "for i in ${P::=/etc/hosts}; do :; done",         // zsh: /b
        "N=P; set -A $N /etc/hosts",                      // zsh: /b
        "echo /etc/hosts | while read P; do break; done", // zsh: /b
        "echo /etc/hosts | { read P; }",                  // zsh: /b
        "Q=P; printf -v \"$Q\" %s /etc/hosts",            // bash: /b
        "Q=P; read \"$Q\" <<< /etc/hosts",                // bash, zsh: /b
        "Q=P; declare \"$Q=/etc/hosts\"",                 // bash: /b
        "Q=P; export \"$Q=/etc/hosts\"",                  // bash, zsh: /b
        "Q=P; typeset \"$Q\"=/etc/hosts",                 // bash, zsh: /b
    ];
    let mut cases: Vec<String> = shapes
        .iter()
        .map(|s| format!("P={OK}; {s}; echo hi > \"$P\""))
        .collect();
    // zsh: user tie, measured `typeset -T Q P; P=/ok; Q=/b` -> /b
    cases.push(format!(
        "typeset -T Q P; P={OK}; Q=/etc/hosts; echo hi > \"$P\""
    ));
    let mut bad = Vec::new();
    for c in &cases {
        let d = scoped(c);
        if matches!(d, Decision::Allow) || !d.clone().hardened().is_deny() {
            bad.push(format!("{d:?}: {c}"));
        }
    }
    assert!(
        bad.is_empty(),
        "{} of {} still Allow / not Deny as subagent:\n{}",
        bad.len(),
        cases.len(),
        bad.join("\n")
    );
}

/// FP controls found by the verifier: base 994be889d Allow, tip a98ffbfaa
/// Deny/Deny. A plain `P` argument to a command that cannot bind a variable
/// does not rebind P.
#[test]
fn plain_name_argument_to_non_binder_stays_allow() {
    let mut bad = Vec::new();
    for mid in ["echo P", "grep P /home/yuki/proj/x"] {
        let c = format!("P={OK}; {mid}; echo hi > \"$P\"");
        let d = scoped(&c);
        if !matches!(d, Decision::Allow) {
            bad.push(format!("{d:?}: {c}"));
        }
    }
    assert!(bad.is_empty(), "FP (not Allow):\n{}", bad.join("\n"));
}
