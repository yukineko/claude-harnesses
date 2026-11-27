#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Fourth adversarial verification (94119fa05) through the real
//! context-governor binary in an isolated state dir / HOME / cwd.

use std::io::Write as _;
use std::process::{Command, Stdio};

fn run_raw(payload: &str) -> String {
    let state = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_context-governor"))
        .env("CONTEXT_GOVERNOR_STATE_DIR", state.path())
        .env("HOME", home.path())
        .env("CLAUDE_CODE_SESSION_ID", "posttool-shape-verify4")
        .env_remove("CONTEXT_GOVERNOR_GROOM_BUDGET")
        .env_remove("CONTEXT_GOVERNOR_REFERENCE_DOC")
        .current_dir(cwd.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0), "hook must exit 0: {out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn payload(transcript: &str, tool_name: &str, resp: serde_json::Value) -> String {
    serde_json::json!({
        "session_id": "posttool-shape-verify4",
        "transcript_path": transcript,
        "cwd": "/tmp",
        "hook_event_name": "PostToolUse",
        "tool_name": tool_name,
        "tool_input": {},
        "tool_response": resp,
    })
    .to_string()
}

/// Transcript whose last usage block reports a full window (budget -> floor).
fn high_pressure_transcript(dir: &std::path::Path) -> String {
    let p = dir.join("t.jsonl");
    std::fs::write(
        &p,
        "{\"message\":{\"usage\":{\"input_tokens\":190000,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0}}}\n",
    )
    .unwrap();
    p.to_string_lossy().into_owned()
}

fn bash(chars: usize) -> serde_json::Value {
    serde_json::json!({
        "stdout": "build output line\n".repeat(chars / 18),
        "stderr": "",
        "interrupted": false,
        "isImage": false,
    })
}

/// Control: under high window pressure a ~1.5k-token Bash output IS groomed
/// when the payload parses (budget is pressure-scaled to its floor).
#[test]
fn control_parsed_high_pressure_grooms() {
    let d = tempfile::tempdir().unwrap();
    let t = high_pressure_transcript(d.path());
    let out = run_raw(&payload(&t, "Bash", bash(6_000)));
    assert!(
        out.contains("updatedToolOutput"),
        "control did not groom: {out}"
    );
}

/// The same payload made unparseable (one stray trailing byte) at the same
/// pressure: the unparsed-note threshold is the UNSCALED default budget
/// (2048 tokens) — pressure is undetermined there, and §3 says resolve to the
/// restrictive side (the floor) — so a payload the parsed path would groom
/// passes with no note at all.
#[test]
fn unparsed_note_uses_restrictive_budget() {
    let d = tempfile::tempdir().unwrap();
    let t = high_pressure_transcript(d.path());
    let raw = payload(&t, "Bash", bash(6_000)) + "}";
    let out = run_raw(&raw);
    assert!(
        out.contains("could not") || out.contains("NOT groomed"),
        "silent on an unparseable {}-char PostToolUse payload: {out:?}",
        raw.len()
    );
}

/// A Read of an image (`{type:"image", file:{base64,...}}`): grooming must not
/// splice a text marker into the base64 (corrupt image, shape still "ok").
#[test]
fn read_image_base64_not_mangled() {
    let b64 = "iVBORw0KGgoAAAANSUhEUgAA".repeat(4_000);
    let resp = serde_json::json!({
        "type": "image",
        "file": {"base64": b64, "type": "image/png", "originalSize": 72_000}
    });
    let out = run_raw(&payload("", "Read", resp));
    let up = serde_json::from_str::<serde_json::Value>(&out)
        .ok()
        .and_then(|v| v["hookSpecificOutput"].get("updatedToolOutput").cloned());
    if let Some(u) = up {
        let got = u["file"]["base64"].as_str().unwrap_or("");
        assert_eq!(
            got,
            b64,
            "base64 rewritten (len {} -> {}), contains marker: {}",
            b64.len(),
            got.len(),
            got.contains("elided")
        );
    }
}
