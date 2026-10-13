// Integration test file: unwrap/expect/panic are fine here (the workspace
// clippy denies are for production code).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Repro for backlog ff98fed0: `harness_core::pricing::rate_for` /
//! `builtin_rate` price a model id that matches neither the built-in table
//! (fable/mythos/opus/sonnet/haiku substring) nor a configured override at
//! $0/$0. Model ids are external input, so the day a new model family ships
//! the session spend reads as $0.00 and the budget gate silently stops
//! blocking.
//!
//! Drives the real `budgetguard gate` Stop hook under an isolated HOME/cwd
//! (no `~/.gauge` store exists there, so the cost comes from the transcript
//! parse, not a gauge record).
//!
//! What this does NOT prove:
//! * it does not exercise the gauge-record cost source (`session::load_one`
//!   path in `gate::session_cost`) — only the transcript-parse fallback;
//! * it does not exercise the DAILY limit, warn limits, or output/cache tokens;
//! * it does not prove which mechanism the fix uses — any blocking decision
//!   for an unpriceable model with nonzero usage satisfies the probe;
//! * the override control only shows an explicit 0/0 override is honoured as
//!   KNOWN $0 — not that other override shapes are handled correctly.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

fn temp_home(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "budgetguard-ff98fed0-{tag}-{}-{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Write budgetguard.toml (session block at $1.00, plus `extra`) and a
/// transcript of one assistant turn with 1,000,000 input tokens on `model`,
/// then run the real Stop hook. Returns (exit code, stdout, stderr).
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
        "session_id": format!("ff98fed0-{tag}"),
        "transcript_path": tp.to_str().unwrap(),
        "cwd": home.to_str().unwrap(),
        "stop_hook_active": false,
    })
    .to_string();
    run(&home, &payload)
}

fn run(home: &Path, payload: &str) -> (i32, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_budgetguard"))
        .arg("gate")
        .env("HOME", home)
        .env_remove("BUDGETGUARD_DISABLE")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(home)
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

/// Apparatus control: the same 1M input tokens on a known model ($5.00 at the
/// built-in opus rate) cross the $1.00 session block and the gate blocks.
#[test]
fn backlog_ff98fed0_control_known_model_over_limit_blocks() {
    let (code, out, err) = run_gate("opus", "claude-opus-4-8", "");
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        is_block(&out),
        "apparatus: opus 1M input ($5) over a $1 session block must block; stdout={out:?} stderr={err:?}"
    );
}

/// Control: an explicit operator override pricing the model at 0/0 (e.g. a
/// local model) is a legitimate KNOWN $0 and must still be allowed.
#[test]
fn backlog_ff98fed0_control_explicit_zero_override_allows() {
    let (code, out, err) = run_gate(
        "override",
        "claude-newfamily-1",
        "\n[[price]]\npattern = \"newfamily\"\ninput = 0.0\noutput = 0.0\n",
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        !is_block(&out),
        "an explicit 0/0 price override is a known $0 and must allow; stdout={out:?} stderr={err:?}"
    );
}

/// Probe (RED while ff98fed0 is open): an unrecognized model with nonzero
/// usage and no matching override is cost-UNDETERMINED, which with an armed
/// block limit must block — not be read as $0 and allowed.
#[test]
fn backlog_ff98fed0_unknown_model_with_usage_blocks_not_priced_zero() {
    let (code, out, err) = run_gate("newfamily", "claude-newfamily-1", "");
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        is_block(&out),
        "backlog ff98fed0: unrecognized model `claude-newfamily-1` with 1,000,000 input tokens \
         was priced at $0 and the $1.00 session block allowed the stop; stdout={out:?} stderr={err:?}"
    );
}
