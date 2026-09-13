// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `record-finding` must not invent a verdict when the recorder omitted one.
//!
//! User ruling 2026-07-21 (backlog eda212a0): defaulting an omitted `--verdict`
//! to `confirmed` is not a fail-open — nothing gets wrongly dismissed — but it
//! does mean `unverified` can NEVER arise by default. A tri-state whose third
//! value is unreachable without an explicit flag is a two-state in practice, and
//! that contradicts the invariant written into CLAUDE.md the same day: do not
//! manufacture certainty. Recording a finding is exactly the moment a verifier
//! knows whether it settled the claim, so the flag is required and the recorder
//! is made to say.
//!
//! The control below (`explicit_verdict_is_accepted`) keeps this from passing
//! vacuously: without it, a binary that rejected EVERY record-finding call would
//! satisfy the requirement test.

use std::process::Command;
use tempfile::TempDir;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_overwatch")
}

fn record(
    home: &TempDir,
    project: &TempDir,
    id: &str,
    verdict: Option<&str>,
) -> std::process::Output {
    let mut cmd = Command::new(bin());
    cmd.arg("record-finding")
        .arg("--finding-id")
        .arg(id)
        .arg("--source")
        .arg("continuous-audit")
        .arg("--summary")
        .arg("a claim the verifier may or may not have settled");
    if let Some(v) = verdict {
        cmd.arg("--verdict").arg(v);
    }
    cmd.current_dir(project.path())
        .env("HOME", home.path())
        .output()
        .expect("spawn overwatch record-finding")
}

#[test]
fn omitting_verdict_is_an_input_error() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let out = record(&home, &project, "CA-req-001", None);
    assert!(
        !out.status.success(),
        "record-finding accepted a call with no --verdict and recorded SOMETHING \
         as the verifier's conclusion. Whatever it stored, the verifier never \
         said it. stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("verdict"),
        "the refusal must name the missing flag so the recorder can fix it: {stderr}"
    );
}

#[test]
fn explicit_verdict_is_accepted() {
    // Anti-vacuity control for the test above.
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    for v in ["confirmed", "refuted", "unverified"] {
        let out = record(&home, &project, &format!("CA-req-{v}"), Some(v));
        assert!(
            out.status.success(),
            "--verdict {v} must still be accepted, otherwise the requirement \
             test above passes for the wrong reason. stderr={}",
            String::from_utf8_lossy(&out.stderr),
        );
    }
}
