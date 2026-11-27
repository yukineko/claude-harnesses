#![allow(clippy::expect_used)]
//! FP controls for backlog f3301770: binders of OTHER names, and plain
//! references, must keep resolving `"$P"` to the confined static value.

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
fn unrelated_binders_and_references_keep_the_static_value() {
    let cases = [
        format!("P={OK}; echo \"$P\"; echo hi > \"$P\""),
        format!("P={OK}; read -r Q <<< x; echo hi > \"$P\""),
        format!("P={OK}; printf -v Q %s x; echo hi > \"$P\""),
        format!("P={OK}; mapfile -t Q < /dev/null; echo hi > \"$P\""),
        format!("P={OK}; cat \"$P\" | wc -l; echo hi > \"$P\""),
    ];
    for c in &cases {
        let d = scoped(c);
        assert!(matches!(d, Decision::Allow), "FP: {c} -> {d:?}");
    }
}
