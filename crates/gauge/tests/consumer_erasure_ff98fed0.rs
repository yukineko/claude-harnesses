// Integration test file: unwrap/expect/panic are fine here (the workspace
// clippy denies are for production code).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog ff98fed0 (consumer erasure): the human-readable gauge views that
//! SUM costs through `report::CostSum` must print `unknown` — never a dollar
//! figure — when any recorded model has usage but no known rate. A dollar
//! figure there is the priced part alone, an under-count of unknown size.
//!
//! Apparatus: two sessions recorded through the real `record` Stop hook under
//! an isolated HOME — one on opus ($5.00 for 1M input) and one on the unknown
//! `claude-newfamily-1` — then `gauge report`, `gauge status` and the text-mode
//! `gauge session` are run as the real binary.
//!
//! What this does NOT prove:
//! * it does not exercise `gauge subagents` text mode, per-agent or per-day
//!   rows of `report` (only the total line, the model rows and the footnote);
//! * it does not pin the exact wording of the unknown cell beyond the word
//!   `unknown` and the absence of `$` on the asserted line;
//! * it does not exercise `[[pricing]]` overrides or `--json` output (see
//!   tests/backlog_ff98fed0.rs for `session --json`).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

fn home() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!(
        "gauge-ff98fed0-consumer-{}-{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn run(home: &Path, args: &[&str], payload: &str) -> (i32, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_gauge"))
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
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

/// Record session `sid` (1,000,000 input tokens on `model`) via the real hook.
fn record_session(home: &Path, sid: &str, model: &str) {
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
    assert!(
        home.join(".gauge/store/sessions")
            .join(format!("{sid}.json"))
            .exists(),
        "apparatus: record hook did not write session {sid}"
    );
}

/// A home with one priced (opus, $5) and one unpriced (newfamily) session.
fn mixed_home() -> PathBuf {
    let h = home();
    record_session(&h, "opus", "claude-opus-4-8");
    record_session(&h, "newfam", "claude-newfamily-1");
    h
}

fn line_with<'a>(out: &'a str, needle: &str) -> &'a str {
    out.lines()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no line containing {needle:?} in:\n{out}"))
}

/// Apparatus control: a home holding ONLY the opus session reports a known
/// $5.00 total in `report` and `status`, so the `unknown` assertions below are
/// caused by the unpriced model, not by an apparatus that never prices.
#[test]
fn ff98fed0_consumer_control_priced_only_totals_are_numbers() {
    let h = home();
    record_session(&h, "opus", "claude-opus-4-8");
    let (rc, out, err) = run(&h, &["report"], "");
    assert_eq!(rc, 0, "{err}");
    let total = line_with(&out, "合計コスト");
    assert!(
        total.contains("$5.00") && !total.contains("unknown"),
        "control report total: {total:?}"
    );
    let (rc, out, err) = run(&h, &["status"], "");
    assert_eq!(rc, 0, "{err}");
    let total = line_with(&out, "total cost:");
    assert!(
        total.contains("$5.00") && !total.contains("unknown"),
        "control status total: {total:?}"
    );
}

/// backlog ff98fed0 (consumer erasure): `gauge report`'s total must be
/// `unknown` when one session's model is unpriced — not the $5.00 priced part.
#[test]
fn ff98fed0_consumer_report_total_is_unknown_not_partial_sum() {
    let h = mixed_home();
    let (rc, out, err) = run(&h, &["report"], "");
    assert_eq!(rc, 0, "{err}");
    let total = line_with(&out, "合計コスト");
    assert!(
        total.contains("unknown") && !total.contains('$'),
        "backlog ff98fed0: report total must be unknown, got {total:?}\n{out}"
    );
    let model_row = line_with(&out, "claude-newfamily-1");
    assert!(
        model_row.contains("unknown") && !model_row.contains('$'),
        "backlog ff98fed0: unpriced model row must be unknown, got {model_row:?}"
    );
}

/// backlog ff98fed0 (consumer erasure): `gauge status`'s total cost line must
/// be `unknown` when a recorded model is unpriced.
#[test]
fn ff98fed0_consumer_status_total_is_unknown_not_partial_sum() {
    let h = mixed_home();
    let (rc, out, err) = run(&h, &["status"], "");
    assert_eq!(rc, 0, "{err}");
    let total = line_with(&out, "total cost:");
    assert!(
        total.contains("unknown") && !total.contains('$'),
        "backlog ff98fed0: status total must be unknown, got {total:?}\n{out}"
    );
}

/// backlog ff98fed0 (consumer erasure): text-mode `gauge session` for the
/// unpriced session must print `cost     unknown`, not `$0.00`.
#[test]
fn ff98fed0_consumer_session_text_cost_is_unknown() {
    let h = mixed_home();
    let (rc, out, err) = run(&h, &["session", "--session", "newfam"], "");
    assert_eq!(rc, 0, "{err}");
    let cost = line_with(&out, "cost ");
    assert!(
        cost.contains("unknown") && !cost.contains('$'),
        "backlog ff98fed0: session text cost must be unknown, got {cost:?}\n{out}"
    );
}
