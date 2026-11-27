#![allow(clippy::expect_used)]
//! Controls for the prebound-alias / POSIX special-builtin prefix fix: without
//! an alias or POSIX mode the earlier behaviour stays (measured in a real
//! shell: `P=/ok; P=/b :` leaves P=/ok outside POSIX mode).

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
fn without_alias_or_posix_the_confined_value_still_resolves() {
    let w = "echo hi > \"$P\"";
    for c in [
        format!("P={OK}; P=/etc/hosts :; {w}"),
        format!("P={OK}; P=/etc/hosts true; {w}"),
        format!("P={OK}; echo ll; {w}"),
    ] {
        let d = scoped(&c);
        assert!(
            matches!(d, Decision::Allow),
            "control not Allow: {c:?} -> {d:?}"
        );
    }
}
