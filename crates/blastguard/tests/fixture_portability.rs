//! RED test for GitHub issue #1486: blastguard lib-test fixtures must not
//! hard-code the macOS-only temp root `/private/tmp/bg-mutkill...`.
//!
//! `/private/tmp` does not exist on Linux, so a fixture that creates
//! directories under that literal path fails there. This is a source scan:
//! it reads `src/scope.rs` plus every `.rs` file under `src/detect/` (and
//! `src/detect.rs`) and reports every string
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

/// Every `.rs` file under `rel` (recursive). An unreadable directory is
/// "cannot determine", never "clean": panic.
fn rs_files_under(rel: &str) -> Vec<String> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("cannot read directory {} to scan it: {e}", dir.display()));
        for entry in entries {
            let entry =
                entry.unwrap_or_else(|e| panic!("cannot read an entry of {}: {e}", dir.display()));
            let path = entry.path();
            let ty = entry
                .file_type()
                .unwrap_or_else(|e| panic!("cannot stat {}: {e}", path.display()));
            if ty.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|x| x == "rs") {
                let rel_path = path
                    .strip_prefix(env!("CARGO_MANIFEST_DIR"))
                    .unwrap_or_else(|e| panic!("{} outside manifest dir: {e}", path.display()))
                    .to_string_lossy()
                    .into_owned();
                out.push(rel_path);
            }
        }
    }
    out.sort();
    out
}

#[test]
fn no_hardcoded_private_tmp_bg_mutkill_fixture_roots() {
    let mut hits = Vec::new();
    let mut files = vec!["src/scope.rs".to_string(), "src/detect.rs".to_string()];
    files.extend(rs_files_under("src/detect"));
    assert!(
        files.iter().any(|f| f.ends_with("detect/tests.rs")),
        "directory walk of src/detect did not find tests.rs: {files:?}"
    );
    for rel in &files {
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
