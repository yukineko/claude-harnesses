// Integration test file: unwrap/expect/panic are fine here (the workspace
// clippy denies are for production code).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Repro for backlog ff98fed0: `harness_core::pricing::rate_for` /
//! `builtin_rate` price a model id that matches neither the built-in table
//! (fable/mythos/opus/sonnet/haiku substring) nor a configured override at
//! $0/$0, so `gauge session --json` reports a KNOWN `cost_usd: 0.0` for a
//! session that spent real tokens on a model family gauge does not know yet.
//!
//! Apparatus (same as backlog_s10_repro.rs): record the session through the
//! real `record` Stop hook under an isolated HOME, then read
//! `gauge session --json --session <id>` (gauge prices the persisted token
//! aggregate on read).
//!
//! What this does NOT prove:
//! * it does not exercise `gauge report`, `subagents`, per-agent costs or the
//!   human-readable `session` output — only `session --json`'s `cost_usd`;
//! * it does not pin HOW the unknown cost is represented — null, an absent
//!   field, or a non-zero exit are all accepted as "not claimed known";
//! * it does not exercise output/cache tokens, only 1M input tokens;
//! * the override control only shows an explicit 0/0 `[[pricing]]` row is
//!   honoured as KNOWN $0 — not that other override shapes are handled.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

fn home() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("gauge-ff98fed0-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn run(home: &Path, args: &[&str], payload: &str) -> (i32, String, String) {
    let mut child = harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_gauge")))
        .args(args)
        .current_dir(home)
        .env("HOME", home)
        .env_remove("GAUGE_DISABLE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(payload.as_bytes());
    }
    let o = child.wait_with_output().unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).trim().to_string(),
        String::from_utf8_lossy(&o.stderr).trim().to_string(),
    )
}

/// Record session `sid` (1,000,000 input tokens on `model`, one assistant
/// turn) through the real `record` hook. Returns whether the record exists.
fn record_session(home: &Path, sid: &str, model: &str) -> bool {
    let tp = home.join(format!("{sid}.jsonl"));
    std::fs::write(
        &tp,
        format!(
            "{{\"type\":\"assistant\",\"timestamp\":\"2026-09-24T00:00:00.000Z\",\"message\":{{\"role\":\"assistant\",\"model\":\"{model}\",\"usage\":{{\"input_tokens\":1000000,\"output_tokens\":0}}}}}}\n"
        ),
    )
    .unwrap();
    let payload = serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": sid,
        "cwd": home.to_str().unwrap(),
        "transcript_path": tp.to_str().unwrap(),
    })
    .to_string();
    let (rc, _, err) = run(home, &["record"], &payload);
    assert_eq!(rc, 0, "record hook exited non-zero: {err}");
    home.join(".gauge/store/sessions")
        .join(format!("{sid}.json"))
        .exists()
}

fn session_json(home: &Path, sid: &str) -> (i32, String, String) {
    run(home, &["session", "--json", "--session", sid], "")
}

fn known_cost(home: &Path, sid: &str) -> f64 {
    let (rc, out, err) = session_json(home, sid);
    assert_eq!(rc, 0, "{err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
    v["cost_usd"]
        .as_f64()
        .unwrap_or_else(|| panic!("no numeric cost_usd: {out}"))
}

/// Apparatus control: 1M input tokens on opus price at the built-in $5/1M.
#[test]
fn backlog_ff98fed0_control_opus_session_costs_five_dollars() {
    let h = home();
    assert!(
        record_session(&h, "opus", "claude-opus-4-8"),
        "apparatus: record hook did not write the opus session record"
    );
    let c = known_cost(&h, "opus");
    assert!((c - 5.0).abs() < 1e-9, "apparatus: expected 5.0, got {c}");
}

/// Control: an explicit `[[pricing]]` row pricing the new family at 0/0 (e.g.
/// a local model) is a legitimate KNOWN $0.
#[test]
fn backlog_ff98fed0_control_explicit_zero_override_is_known_zero() {
    let h = home();
    std::fs::write(
        h.join("gauge.toml"),
        "[[pricing]]\npattern = \"newfamily\"\ninput = 0.0\noutput = 0.0\n",
    )
    .unwrap();
    assert!(
        record_session(&h, "free", "claude-newfamily-1"),
        "apparatus: record hook did not write the overridden session record"
    );
    let c = known_cost(&h, "free");
    assert_eq!(c, 0.0, "explicit 0/0 override must price as exactly 0.0");
}

/// Probe (RED while ff98fed0 is open): an unrecognized model with nonzero
/// usage and no matching override must not be reported as a known $0.00.
#[test]
fn backlog_ff98fed0_unknown_model_cost_is_not_claimed_zero() {
    let h = home();
    let recorded = record_session(&h, "newfam", "claude-newfamily-1");
    // Test author's choice, stated so it can be challenged: a fix that makes
    // the session vanish from the store instead (top-level `null`) is not
    // accepted — a dropped session also sums to $0 in every total.
    assert!(
        recorded,
        "backlog ff98fed0: the record hook dropped the `claude-newfamily-1` session instead of \
         recording its tokens; a vanished session reads as $0 in every total"
    );
    let (rc, out, err) = session_json(&h, "newfam");
    if rc != 0 {
        return; // non-zero exit: no cost claimed.
    }
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
    let cost = v.get("cost_usd");
    assert!(
        !cost.is_some_and(|c| c.as_f64() == Some(0.0)),
        "backlog ff98fed0: unrecognized model `claude-newfamily-1` with 1,000,000 input tokens \
         reported as KNOWN cost_usd 0.0 (rc={rc}); out={out} err={err}"
    );
}
