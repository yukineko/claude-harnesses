#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Repro for backlog 84a45113: two comments describe a "post-cap" injected-size
//! metric — `src/bin/context-governor.rs` ("record the post-cap injected
//! `additionalContext` size") and `crates/harness-core/src/inject_metrics.rs`
//! ("Each has its own per-injector char cap") — but the context-governor
//! injector emits the whole matched section with no cap.
//!
//! The test drives the REAL binary on UserPromptSubmit with a reference doc
//! whose matched section is far larger than any plausible cap, and measures the
//! injected `additionalContext`. It is RED while the comments claim a cap AND
//! the output is uncapped; either fix (implement the cap, or correct both
//! comments) turns it GREEN — the ticket leaves that choice open.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const BODY_CHARS: usize = 200_000;

fn injected_len(doc: &Path, state: &Path, cwd: &Path) -> usize {
    let payload = serde_json::json!({
        "session_id": "backlog-84a45113",
        "transcript_path": "",
        "cwd": cwd.to_str().unwrap(),
        "hook_event_name": "UserPromptSubmit",
        "prompt": "explain the authentication policy",
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_context-governor"))
        .env("CONTEXT_GOVERNOR_STATE_DIR", state)
        .env("CONTEXT_GOVERNOR_REFERENCE_DOC", doc)
        .env("CLAUDE_CODE_SESSION_ID", "backlog-84a45113")
        .env("HOME", state)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn context-governor");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "exit {:?}", out.status);
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout is one JSON envelope");
    let ctx = v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_else(|| panic!("no additionalContext injected (undetermined, not clean): {v}"));
    ctx.chars().count()
}

fn setup() -> (tempfile::TempDir, std::path::PathBuf) {
    let td = tempfile::tempdir().unwrap();
    let doc = td.path().join("ref.md");
    let body = "x".repeat(BODY_CHARS);
    std::fs::write(
        &doc,
        format!("# Authentication policy\n{body}\n\n# Other\nsmall\n"),
    )
    .unwrap();
    (td, doc)
}

/// Anti-vacuity: the heading match fires and the measurement reads a real size.
#[test]
fn control_matched_section_is_injected() {
    let (td, doc) = setup();
    let n = injected_len(&doc, &td.path().join("state"), td.path());
    assert!(n > 1_000, "expected a sizeable injection, got {n} chars");
}

#[test]
#[ignore = "backlog 84a45113: open defect, remove ignore when fixed"]
fn post_cap_comment_is_backed_by_a_cap() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let bin_src = std::fs::read_to_string(root.join("src/bin/context-governor.rs")).unwrap();
    let metrics_src =
        std::fs::read_to_string(root.join("../harness-core/src/inject_metrics.rs")).unwrap();
    let claims_cap = bin_src.contains("post-cap") || metrics_src.contains("per-injector char cap");

    let (td, doc) = setup();
    let n = injected_len(&doc, &td.path().join("state"), td.path());
    let capped = n < BODY_CHARS;

    assert!(
        !claims_cap || capped,
        "comments claim a post-cap/per-injector cap, but the injector emitted {n} chars \
         for a {BODY_CHARS}-char section (no cap applied)"
    );
}
