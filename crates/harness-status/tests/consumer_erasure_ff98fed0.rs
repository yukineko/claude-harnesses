// Integration test file: unwrap/expect/panic are fine here (the workspace
// clippy denies are for production code).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog ff98fed0 (consumer erasure): `harness-status sessions` falls back to
//! `pricing::session_cost` when budgetguard's ledger has no entry for a
//! session. For a gauge record whose model has usage but no known rate that
//! fallback is Undetermined, and the panel must say `unknown` (text) /
//! `cost_usd: null` (JSON) — never `$0.0000` / `0.0`.
//!
//! Apparatus: a gauge `SessionRecord` JSON written directly into an isolated
//! HOME's `~/.gauge/store/sessions/`, no budgetguard ledger, real binary.
//!
//! What this does NOT prove:
//! * it does not exercise the ledger-hit path (budgetguard's per-session USD);
//! * it does not exercise the default (no-subcommand) report rendering;
//! * it does not exercise records mixing priced and unpriced models.

use std::path::{Path, PathBuf};
use std::process::Command;

fn sandbox(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "harness-status-ff98fed0-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_record(home: &Path, sid: &str, model: &str) {
    let dir = home.join(".gauge/store/sessions");
    std::fs::create_dir_all(&dir).unwrap();
    let rec = serde_json::json!({
        "session_id": sid,
        "project": "proj",
        "cwd": "/cwd",
        "models": { model: { "input": 1_000_000u64, "output": 0u64 } },
        "turns": 1,
        "last_ts": "2026-09-24T00:00:00Z",
        "updated_at": "2026-09-24T00:00:00Z",
    });
    std::fs::write(dir.join(format!("{sid}.json")), rec.to_string()).unwrap();
}

fn run(home: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_harness-status"))
        .args(args)
        .current_dir(home)
        .env("HOME", home)
        .env("HARNESS_DATE", "2026-09-24")
        .output()
        .expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Apparatus control: an opus record ($5.00 for 1M input) is priced, so the
/// fixture really reaches the pricing fallback and the panel prints numbers.
#[test]
fn ff98fed0_consumer_control_priced_record_shows_dollars() {
    let h = sandbox("opus");
    write_record(&h, "opussess", "claude-opus-4-8");
    let (rc, out, err) = run(&h, &["sessions"]);
    assert_eq!(rc, 0, "{err}");
    let line = out
        .lines()
        .find(|l| l.starts_with("opussess"))
        .unwrap_or_else(|| panic!("no session row: {out}"));
    assert!(line.contains("$5.0000"), "control row: {line:?}");
    let _ = std::fs::remove_dir_all(&h);
}

/// backlog ff98fed0 (consumer erasure): an unpriced model's session renders
/// `unknown` in text and `cost_usd: null` with a reason in JSON.
#[test]
fn ff98fed0_consumer_unpriced_record_cost_is_unknown_not_zero() {
    let h = sandbox("newfam");
    write_record(&h, "newfamsess", "claude-newfamily-1");

    let (rc, out, err) = run(&h, &["sessions"]);
    assert_eq!(rc, 0, "{err}");
    let line = out
        .lines()
        .find(|l| l.starts_with("newfamse"))
        .unwrap_or_else(|| panic!("no session row: {out}"));
    assert!(
        line.contains("unknown") && !line.contains('$'),
        "backlog ff98fed0: unpriced session cost must be unknown, got {line:?}"
    );

    let (rc, out, err) = run(&h, &["--json", "sessions"]);
    assert_eq!(rc, 0, "{err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
    let s = &v[0];
    assert_eq!(s["session_id"], "newfamsess", "{out}");
    assert!(
        s["cost_usd"].is_null(),
        "backlog ff98fed0: unpriced session cost_usd must be null, got {}",
        s["cost_usd"]
    );
    assert!(
        s["cost_unknown_reason"].is_string(),
        "an unknown cost must carry its reason: {out}"
    );
    let _ = std::fs::remove_dir_all(&h);
}
