// Integration test file: unwrap/expect/panic are fine here (the workspace
// clippy denies are for production code).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog ff98fed0 (consumer erasure), finding F4: budgetguard's `[[price]]`
//! override `pattern` must match model ids case-insensitively, like gauge's
//! `[[pricing]]` patterns. `harness_core::pricing::rate_for` lowercases only
//! the model id, so an override written `pattern = "NewFamily"` that is not
//! lowercased by budgetguard's config loader never matches
//! `claude-newfamily-1`; the model is then cost-undetermined and an armed
//! session block blocks a session the operator explicitly priced at $0.
//!
//! Drives the real `budgetguard gate` Stop hook under an isolated HOME/cwd
//! (no `~/.gauge` store, so the cost comes from the transcript parse).
//!
//! What this does NOT prove:
//! * it does not exercise the gauge-record cost source, the daily limit or
//!   warn limits;
//! * it does not prove the fix lowercases at load time vs. at match time —
//!   any case-insensitive match satisfies it;
//! * it only probes a mixed-case pattern against a lowercase model id, not a
//!   mixed-case model id (that side is lowercased by `rate_for`).

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

fn temp_home(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("budgetguard-f4-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Session block $1.00 plus `extra_toml`; one assistant turn of 1,000,000
/// input tokens on `model`; run the real Stop hook. Returns (code, stdout, stderr).
fn run_gate(tag: &str, model: &str, extra_toml: &str) -> (i32, String, String) {
    let home = temp_home(tag);
    std::fs::write(
        home.join("budgetguard.toml"),
        format!("[session]\nblock_usd = 1.0\n{extra_toml}"),
    )
    .unwrap();
    let tp = home.join("transcript.jsonl");
    std::fs::write(
        &tp,
        format!(
            "{{\"type\":\"assistant\",\"timestamp\":\"2026-06-22T10:00:01Z\",\"message\":{{\"model\":\"{model}\",\"content\":[{{\"type\":\"text\",\"text\":\"x\"}}],\"usage\":{{\"input_tokens\":1000000,\"output_tokens\":0,\"cache_read_input_tokens\":0}}}}}}\n"
        ),
    )
    .unwrap();
    let payload = serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": format!("f4-{tag}"),
        "transcript_path": tp.to_str().unwrap(),
        "cwd": home.to_str().unwrap(),
        "stop_hook_active": false,
    })
    .to_string();
    let mut child = Command::new(env!("CARGO_BIN_EXE_budgetguard"))
        .arg("gate")
        .env("HOME", &home)
        .env_remove("BUDGETGUARD_DISABLE")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(&home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn is_block(stdout: &str) -> bool {
    stdout.lines().any(|l| {
        serde_json::from_str::<serde_json::Value>(l)
            .ok()
            .is_some_and(|v| v["decision"] == "block")
    })
}

/// Anti-vacuity control: the same model with NO override is undetermined and
/// blocks. Without it, "allowed" below could mean the gate allows everything.
#[test]
fn f4_control_unpriced_model_without_override_blocks() {
    let (code, out, err) = run_gate("nooverride", "claude-newfamily-1", "");
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        is_block(&out),
        "apparatus: unpriced model with 1M input and a $1 block must block; stdout={out:?} stderr={err:?}"
    );
}

/// F4 probe (RED until budgetguard matches `[[price]]` patterns
/// case-insensitively): a mixed-case `pattern = "NewFamily"` 0/0 override is
/// a known $0 for `claude-newfamily-1`, so the stop must be ALLOWED.
#[test]
fn f4_mixed_case_price_pattern_matches_and_allows() {
    let (code, out, err) = run_gate(
        "mixedcase",
        "claude-newfamily-1",
        "\n[[price]]\npattern = \"NewFamily\"\ninput = 0.0\noutput = 0.0\n",
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        !is_block(&out),
        "backlog ff98fed0 F4: `[[price]] pattern = \"NewFamily\"` (0/0) did not match \
         `claude-newfamily-1` case-insensitively and the stop was blocked; stdout={out:?} stderr={err:?}"
    );
}
