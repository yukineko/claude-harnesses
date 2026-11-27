#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Fourth adversarial verification (94119fa05) through the real ctxrot
//! binary with an isolated HOME / cwd. Default `huge_tool_output_bytes` 50k.

use std::io::Write as _;
use std::process::{Command, Stdio};

fn run_raw(payload: &str) -> String {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let mut c = Command::new(env!("CARGO_BIN_EXE_ctxrot"));
    harness_core::git_env::scrub(&mut c);
    let mut child = c
        .arg("toolguard")
        .env("HOME", home.path())
        .env_remove("GUARD_DISABLE")
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
    assert_eq!(out.status.code(), Some(0), "toolguard must exit 0: {out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn payload(tool_name: &str, resp: serde_json::Value) -> String {
    serde_json::json!({
        "session_id": "posttool-shape-verify4",
        "transcript_path": "",
        "cwd": "/tmp",
        "hook_event_name": "PostToolUse",
        "tool_name": tool_name,
        "tool_input": {},
        "tool_response": resp,
    })
    .to_string()
}

fn updated(out: &str) -> Option<serde_json::Value> {
    serde_json::from_str::<serde_json::Value>(out)
        .ok()
        .and_then(|v| v["hookSpecificOutput"].get("updatedToolOutput").cloned())
}

/// Read of a ~96k-char PNG: `response_text_len` falls back to the serialized
/// size (no stdout/text/content key), so it is "huge", and the recursive
/// shrink splices `…[elided N chars of M]…` into the base64 — a corrupt image
/// with a valid shape.
#[test]
#[ignore = "verify4: Read image base64 spliced with a text elision marker (corrupt image)"]
fn read_image_base64_not_mangled() {
    let b64 = "iVBORw0KGgoAAAANSUhEUgAA".repeat(4_000);
    let resp = serde_json::json!({
        "type": "image",
        "file": {"base64": b64, "type": "image/png", "originalSize": 72_000}
    });
    let out = run_raw(&payload("Read", resp));
    if let Some(u) = updated(&out) {
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

/// Bash `isImage: true`: same corruption on stdout.
#[test]
#[ignore = "verify4: Bash isImage:true stdout spliced with a text elision marker"]
fn bash_is_image_not_mangled() {
    let data = format!(
        "data:image/png;base64,{}",
        "iVBORw0KGgoAAAANSUhEUgAA".repeat(4_000)
    );
    let resp = serde_json::json!({"stdout": data, "stderr": "", "interrupted": false, "isImage": true});
    let out = run_raw(&payload("Bash", resp));
    if let Some(u) = updated(&out) {
        let got = u["stdout"].as_str().unwrap_or("");
        assert_eq!(got.len(), data.len(), "image stdout rewritten: marker={}", got.contains("elided"));
    }
}

/// Unparseable large PostToolUse payload (stray trailing byte): a note must be
/// emitted (round-3 claim) — checked on a non-nesting invalidity too.
#[test]
fn unparseable_trailing_garbage_is_noted() {
    let resp = serde_json::json!({"stdout": "x\n".repeat(40_000), "stderr": "", "interrupted": false, "isImage": false});
    let raw = payload("Bash", resp) + "}";
    let out = run_raw(&raw);
    assert!(out.contains("could not be parsed"), "silent: {out:?}");
}

/// Lone surrogate escape (valid for JS JSON.stringify, rejected by serde_json)
/// inside an otherwise ordinary large Bash payload: must be noted, not silent.
#[test]
fn lone_surrogate_payload_is_noted() {
    let resp = serde_json::json!({"stdout": "x\n".repeat(40_000), "stderr": "", "interrupted": false, "isImage": false});
    let raw = payload("Bash", resp).replacen("\"stderr\":\"\"", "\"stderr\":\"\\ud800\"", 1);
    assert!(raw.contains("\\ud800"));
    let out = run_raw(&raw);
    assert!(out.contains("could not be parsed"), "silent: {out:?}");
}

/// 500 unshrinkable numeric objects (~64.5k chars, share ~98 >= 64): the
/// rewrite is declined and the note claims the shape cannot be shrunk, though
/// dropping items (which this module does for other arrays) would fit.
#[test]
#[ignore = "verify4: unshrinkable item array (share >= 64) never drops items; output passes untrimmed"]
fn unshrinkable_item_array_is_trimmed() {
    let items: Vec<serde_json::Value> = (0..500)
        .map(|i| {
            serde_json::json!({"a": 1_000_000 + i, "b": 2_000_000 + i, "c": 3_000_000 + i,
                   "d": 4_000_000 + i, "e": 5_000_000 + i, "f": 6_000_000 + i,
                   "g": 7_000_000 + i, "h": 8_000_000 + i, "i": 9_000_000 + i,
                   "j": true, "k": false})
        })
        .collect();
    let out = run_raw(&payload("Bash", serde_json::Value::Array(items)));
    let u = updated(&out).unwrap_or_else(|| panic!("no rewrite: {out}"));
    assert!(u.to_string().chars().count() <= 50_000);
}
