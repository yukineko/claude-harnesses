#![allow(clippy::expect_used)]
//! RED (verifier, 2026-11-28): false positives new on fp-f3301770 tip
//! f8bb5dc4c (Deny/Deny) that are Allow on base 994be889d. None rebinds P.
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
fn non_rebinding_shapes_stay_allow_like_base() {
    let cases = [
        // an expanded VALUE argument, not the -v name
        format!("P={OK}; printf -v out \"%s\" \"$X\"; echo hi > \"$P\""),
        // length read
        format!("P={OK}; echo ${{#P}}; echo hi > \"$P\""),
        // bare P operand of a non-binder inside a compound pipeline stage
        format!("P={OK}; ls | {{ echo P; }}; echo hi > \"$P\""),
    ];
    let mut bad = Vec::new();
    for c in &cases {
        let d = scoped(c);
        if !matches!(d, Decision::Allow) {
            bad.push(format!("{d:?}: {c}"));
        }
    }
    assert!(bad.is_empty(), "FP (not Allow):\n{}", bad.join("\n"));
}
