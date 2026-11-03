//! RED test for GitHub issue #1486: blastguard lib-test fixtures must not
//! hard-code the macOS-only temp root `/private/tmp/bg-mutkill...`.
//!
//! `/private/tmp` does not exist on Linux, so a fixture that creates
//! directories under that literal path fails there. This is a source scan:
//! it reads `src/scope.rs` and `src/detect.rs` and reports every string
//! literal (a quote immediately followed by the path, which also covers
//! `format!`-style literals) that begins with the hard-coded root.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

const NEEDLE: &str = "\"/private/tmp/bg-mutkill";

fn offending_lines(rel: &str) -> Vec<(String, usize, String)> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    // An unreadable source is "cannot determine", never "clean": fail loudly.
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {} to scan it: {e}", path.display()));
    text.lines()
        .enumerate()
        .filter(|(_, l)| l.contains(NEEDLE))
        .map(|(i, l)| (rel.to_string(), i + 1, l.trim().to_string()))
        .collect()
}

#[test]
fn no_hardcoded_private_tmp_bg_mutkill_fixture_roots() {
    let mut hits = Vec::new();
    for rel in ["src/scope.rs", "src/detect.rs"] {
        hits.extend(offending_lines(rel));
    }
    let report: Vec<String> = hits
        .iter()
        .map(|(f, n, l)| format!("  {f}:{n}: {l}"))
        .collect();
    assert!(
        hits.is_empty(),
        "hard-coded macOS-only temp root in test fixtures (/private/tmp does not exist on Linux; \
         derive the root portably instead of a string literal starting with \
         \"/private/tmp/bg-mutkill\"):\n{}",
        report.join("\n")
    );
}
