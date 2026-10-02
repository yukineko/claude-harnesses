//! Repro for backlog 23869ff7: the SessionStart rehydrator (whose docstrings say
//! pins survive compaction (I1), "Most relevant on source == compact") is never
//! invoked on compact because hooks/hooks.json's SessionStart matcher is
//! "startup|resume|clear".
//!
//! This test reads the shipped hook registration (what Claude Code actually
//! matches on), NOT the binary invoked directly (which bypasses the matcher — see
//! backlog 565fb2a8).

use std::path::PathBuf;

fn session_start_matchers() -> Vec<String> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("hooks/hooks.json");
    let raw = std::fs::read_to_string(&p).expect("hooks.json readable");
    let v: serde_json::Value = serde_json::from_str(&raw).expect("hooks.json parses");
    v["hooks"]["SessionStart"]
        .as_array()
        .expect("SessionStart registered")
        .iter()
        .map(|g| g["matcher"].as_str().unwrap_or("").to_string())
        .collect()
}

#[test]
#[ignore = "backlog 23869ff7: open defect, remove ignore when fixed"]
fn session_start_matcher_covers_compact() {
    let ms = session_start_matchers();
    assert!(
        ms.iter()
            .any(|m| m.is_empty() || m.split('|').any(|alt| alt == "compact" || alt == "*")),
        "SessionStart matcher(s) {ms:?} exclude source=compact, so the rehydrator never runs on compaction"
    );
}

/// Control: the matcher list is non-empty and does cover the sources it claims,
/// so the assertion above is not vacuously RED from a parse problem.
#[test]
fn control_matcher_parses_and_covers_startup() {
    let ms = session_start_matchers();
    assert!(
        ms.iter().any(|m| m.split('|').any(|alt| alt == "startup")),
        "{ms:?}"
    );
}
