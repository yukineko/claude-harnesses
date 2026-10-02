// Integration test: unwrap/expect/panic are allowed (the workspace lint denies
// them for production code only).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Closure regression test for backlog c867ff8e, written by an independent
//! verifier (backlog-closure audit, batch b1_0).
//!
//! c867ff8e: `rollback_cli::record_finding`'s doc comment described a
//! `None` (caller stated no verdict, "reads as Confirmed") branch after the
//! signature had become a required `verdict: &str`. A docstring describing an
//! unreachable permissive branch as live behaviour is the CLAUDE.md §4 shape.
//! This pins the doc and the signature together: while `verdict` is a plain
//! `&str`, the doc must not describe a `None` / no-verdict path.

fn record_finding_doc_and_signature() -> (String, String) {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/rollback_cli.rs"),
    )
    .expect("rollback_cli.rs readable");
    let lines: Vec<&str> = src.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.starts_with("pub fn record_finding("))
        .expect("record_finding still exists");
    let mut doc = Vec::new();
    let mut i = at;
    while i > 0 {
        i -= 1;
        let l = lines[i].trim_start();
        if l.starts_with("///") {
            doc.push(l.to_string());
        } else if l.starts_with("#[") {
            continue;
        } else {
            break;
        }
    }
    doc.reverse();
    let sig_end = lines[at..]
        .iter()
        .position(|l| l.contains(") ->") || l.trim() == ") {")
        .map(|n| at + n)
        .expect("signature closes");
    (doc.join("\n"), lines[at..=sig_end].join("\n"))
}

#[test]
fn backlog_c867ff8e_record_finding_doc_has_no_none_verdict_branch() {
    let (doc, sig) = record_finding_doc_and_signature();
    assert!(
        sig.contains("verdict: &str"),
        "precondition: verdict is a required &str (if it became Option again, this test's \
         premise changed — re-read c867ff8e):\n{sig}"
    );
    for stale in ["caller stated no verdict", "`None`", "* None", "Some(raw)"] {
        assert!(
            !doc.contains(stale),
            "c867ff8e: the doc of record_finding describes a no-verdict branch ({stale:?}) the \
             `verdict: &str` signature cannot reach:\n{doc}"
        );
    }
}
