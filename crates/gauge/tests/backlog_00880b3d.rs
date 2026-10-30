// Integration test file: unwrap/expect/panic are fine here (the workspace
// clippy denies are for production code).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Repro for backlog 00880b3d (gauge verdict-path audit items P3/P4):
//! `gauge::config::Config::load` silently degrades a broken `gauge.toml`.
//!
//! * P3: `if let Ok(read_to_string)` / `if let Ok(toml::from_str)` — a config
//!   file that EXISTS but cannot be read or parsed falls back to built-in
//!   defaults with no diagnostic, so the read commands report numbers
//!   computed from a configuration the user never wrote.
//! * P4: a `[[pricing]]` row whose `input`/`output` is missing (a one-letter
//!   typo `inputt`, or the rates omitted) maps to $0 via `unwrap_or(0.0)` and
//!   OVERRIDES the built-in table: 1M opus input tokens report
//!   `cost_usd: 0.0` with rc 0, and `gauge status` claims
//!   "1 override(s) + built-in".
//!
//! Required behaviour pinned here:
//! * absent config -> built-in defaults, a KNOWN cost (controls);
//! * a config that exists but cannot be read/parsed, or a pricing row that
//!   cannot be applied as written, must not yield a numeric cost with rc 0
//!   (the stricter of the two options in the ticket: a stderr warning next to
//!   a defaults-derived number is NOT accepted), AND the condition must be
//!   visible: stderr must name the config file (`gauge.toml`).
//!
//! Apparatus (same as backlog_s10_repro.rs / backlog_ff98fed0.rs): record the
//! session through the real `record` Stop hook under an isolated HOME/cwd
//! with NO config present, then write the config under test and run the read
//! command (gauge prices the persisted token aggregate at read time, so the
//! config under test only affects the read).
//!
//! What this does NOT prove:
//! * the `record` Stop hook under a broken config is NOT pinned. Today it
//!   would write into the default `state_dir` silently; whether the fix
//!   should block, warn, or skip recording is a design decision this test
//!   cannot justify, so nothing is asserted about it (the config is written
//!   only AFTER recording precisely so record behaviour is not exercised);
//! * only `session --json` and `status` are exercised — not `report`,
//!   `subagents`, or the human-readable `session` output;
//! * it does not pin the exact shape of the refusal: non-zero exit, a null
//!   `cost_usd`, or no JSON at all are all accepted as "no numeric cost";
//! * misspelled NON-pricing keys (e.g. `enabeld = true`) are not probed;
//! * the home-level `~/.gauge/config.toml` layer is not probed, only the
//!   project-level `gauge.toml`;
//! * the "unreadable" case uses a directory named `gauge.toml` (deterministic
//!   even as root), not a permission-denied file.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

fn home() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("gauge-00880b3d-{}-{n}", std::process::id()));
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

/// Records session `sid` (1M opus input tokens, one assistant turn) through
/// the real `record` hook, with no config file present.
fn record_session(home: &Path, sid: &str) {
    assert!(
        !home.join("gauge.toml").exists(),
        "apparatus: record must run before the config under test is written"
    );
    let tp = home.join(format!("{sid}.jsonl"));
    std::fs::write(
        &tp,
        "{\"type\":\"assistant\",\"timestamp\":\"2026-09-24T00:00:00.000Z\",\"message\":{\"role\":\"assistant\",\"model\":\"claude-opus-4-8\",\"usage\":{\"input_tokens\":1000000,\"output_tokens\":0}}}\n",
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
    assert_eq!(rc, 0, "apparatus: record hook exited non-zero: {err}");
    assert!(
        home.join(".gauge/store/sessions")
            .join(format!("{sid}.json"))
            .exists(),
        "apparatus: record hook did not write the session record"
    );
}

/// A recorded session plus the config under test (`None` = no config).
fn fixture(config: Option<&str>) -> PathBuf {
    let h = home();
    record_session(&h, "s1");
    if let Some(text) = config {
        std::fs::write(h.join("gauge.toml"), text).unwrap();
    }
    h
}

/// `(rc, cost_usd as f64 if numeric, stdout, stderr)` of `session --json`.
fn session_cost(h: &Path) -> (i32, Option<f64>, String, String) {
    let (rc, out, err) = run(h, &["session", "--json", "--session", "s1"], "");
    let cost = serde_json::from_str::<serde_json::Value>(&out)
        .ok()
        .and_then(|v| v["cost_usd"].as_f64());
    (rc, cost, out, err)
}

fn assert_known_cost(h: &Path, want: f64, what: &str) {
    let (rc, cost, out, err) = session_cost(h);
    assert_eq!(rc, 0, "{what}: rc={rc} stderr={err}");
    let c = cost.unwrap_or_else(|| panic!("{what}: no numeric cost_usd: {out}"));
    assert!((c - want).abs() < 1e-9, "{what}: expected {want}, got {c}");
}

/// The broken-config contract for `session --json`: no numeric cost with
/// rc 0, and stderr names the config file.
fn assert_refuses_numeric_cost(h: &Path, what: &str) {
    let (rc, cost, out, err) = session_cost(h);
    eprintln!("{what}: rc={rc} cost={cost:?} stdout={out:?} stderr={err:?}");
    assert!(
        !(rc == 0 && cost.is_some()),
        "{what}: `session --json` reported numeric cost_usd {cost:?} with rc 0 from a \
         config that cannot be applied as written (silent fallback / $0 override)"
    );
    assert!(
        err.contains("gauge.toml"),
        "{what}: the broken config is not visible — stderr does not name gauge.toml: {err:?}"
    );
}

const COMPLETE: &str = "[[pricing]]\npattern = \"opus\"\ninput = 15.0\noutput = 75.0\n";
const TYPO: &str = "[[pricing]]\npattern = \"opus\"\ninputt = 15.0\noutput = 75.0\n";
const OMITTED: &str = "[[pricing]]\npattern = \"opus\"\n";
const UNPARSEABLE: &str = "enabled = true\n[[pricing]\npattern = \"opus\"\ninput = 15.0\n";

#[test]
fn control_1_no_config_uses_builtin_price() {
    assert_known_cost(&fixture(None), 5.0, "control 1 (no gauge.toml)");
}

#[test]
fn control_1b_valid_config_without_pricing_uses_builtin_price() {
    // A valid partial config must keep working (the fix must not reject it).
    assert_known_cost(
        &fixture(Some("enabled = true\ntrack_tools = true\n")),
        5.0,
        "control 1b (valid gauge.toml, no [[pricing]])",
    );
}

#[test]
fn control_2_complete_override_applies() {
    assert_known_cost(
        &fixture(Some(COMPLETE)),
        15.0,
        "control 2 (complete override)",
    );
}

#[test]
fn probe_3_typo_rate_field_is_not_a_zero_dollar_override() {
    assert_refuses_numeric_cost(&fixture(Some(TYPO)), "probe 3 (`inputt` typo)");
}

#[test]
fn probe_4_omitted_rate_fields_are_not_a_zero_dollar_override() {
    assert_refuses_numeric_cost(&fixture(Some(OMITTED)), "probe 4 (rates omitted)");
}

#[test]
fn probe_5_unparseable_config_is_not_silently_defaults() {
    assert_refuses_numeric_cost(&fixture(Some(UNPARSEABLE)), "probe 5 (TOML syntax error)");
}

#[test]
fn probe_5b_unreadable_config_is_not_silently_defaults() {
    // `gauge.toml` exists but `read_to_string` fails (it is a directory).
    let h = fixture(None);
    std::fs::create_dir(h.join("gauge.toml")).unwrap();
    assert_refuses_numeric_cost(&h, "probe 5b (gauge.toml unreadable)");
}

#[test]
fn probe_6_status_does_not_claim_a_broken_override_was_adopted() {
    let h = fixture(Some(TYPO));
    let (rc, out, err) = run(&h, &["status"], "");
    eprintln!("probe 6: rc={rc} stdout={out:?} stderr={err:?}");
    assert!(
        !(rc == 0 && out.contains("1 override(s) + built-in")),
        "probe 6: `gauge status` claims the malformed override was adopted \
         (\"1 override(s) + built-in\") with rc 0"
    );
    // Same contract as `session --json`: no numeric total from a config that
    // cannot be applied as written.
    let total = out
        .lines()
        .find(|l| l.starts_with("total cost:"))
        .unwrap_or("");
    assert!(
        !(rc == 0 && total.contains('$')),
        "probe 6: `gauge status` reports a numeric total ({total:?}) with rc 0 \
         from a malformed pricing override"
    );
    assert!(
        err.contains("gauge.toml"),
        "probe 6: the broken config is not visible — stderr does not name gauge.toml: {err:?}"
    );
}
