#![allow(clippy::expect_used)]
//! RED (verifier, 2026-11-28, tip f8bb5dc4c / 0.2.964): an alias defined on an
//! earlier LINE turns a listed non-binder into `read`, so its bare `P`
//! operand rebinds P. Measured: bash 3.2.57 `bash -c 'shopt -s
//! expand_aliases; alias echo=read<NL>P=/ok; echo P <<< /b; printf "%s\n"
//! "$P"'` prints /b. Also Allow on base 994be889d.
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
fn alias_shadowed_non_binder_is_never_allow() {
    let cases = [
        format!("shopt -s expand_aliases; alias echo=read\nP={OK}; echo P <<< /etc/hosts; echo hi > \"$P\""),
        format!("shopt -s expand_aliases; alias cat=read\nP={OK}; cat P <<< /etc/hosts; echo hi > \"$P\""),
        format!("P={OK}; shopt -s expand_aliases; alias grep=read\ngrep P <<< /etc/hosts; echo hi > \"$P\""),
    ];
    let mut bad = Vec::new();
    for c in &cases {
        let d = scoped(c);
        if matches!(d, Decision::Allow) || !d.clone().hardened().is_deny() {
            bad.push(format!("{d:?}: {c:?}"));
        }
    }
    assert!(
        bad.is_empty(),
        "{} of {}:\n{}",
        bad.len(),
        cases.len(),
        bad.join("\n")
    );
}
