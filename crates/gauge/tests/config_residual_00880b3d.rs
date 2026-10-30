// Integration test file: unwrap/expect/panic are fine here (the workspace
// clippy denies are for production code).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Residual repro for backlog 00880b3d, written by an independent test author
//! after the first fix (`Config::load_checked` -> `Undetermined` for an
//! unreadable/unparseable `gauge.toml` or a rate-less `[[pricing]]` row).
//! An adversarial verifier found two more paths of the same class — "the
//! config cannot be applied as written, yet a number comes out with rc 0":
//!
//! * R1: a misspelled TOP-LEVEL key is silently ignored, because
//!   `FileConfig` has no `deny_unknown_fields` (only `FilePrice` does).
//!   `[[pricng]]` (opus at 0/0) -> `session --json` reports `cost_usd: 5.0`
//!   from the built-in table with rc 0 and `status` says `pricing: built-in`;
//!   `enabeld = false` -> gauge is still enabled.
//! * R2: a `[[pricing]]` rate that is not a finite non-negative number
//!   (`input = nan`, `output = inf`, `input = -5.0`) is accepted. nan/inf give
//!   `cost_usd: null` with rc 0 and `status` prints `total cost: $NaN`;
//!   `-5.0` gives `cost_usd: -5.0` with rc 0.
//!
//! Required behaviour pinned here: such a config is refused like the other
//! unusable configs — the read command exits NON-ZERO and stderr names
//! `gauge.toml`. (Stricter than `backlog_00880b3d.rs`, which also accepts a
//! null `cost_usd`: a null with rc 0 is exactly what nan produces today.)
//! The valid key set is exactly: `enabled`, `track_tools`, `state_dir`,
//! `[[pricing]]` (`pattern`/`input`/`output`).
//!
//! Controls (must pass before AND after the fix): no config, the shipped
//! `gauge.example.toml` verbatim, the same file with its commented
//! `[[pricing]]` examples uncommented, the `gauge init` starter, a valid
//! `enabled = false` (proves the key is honoured, so the `enabeld` probe is
//! about the typo), a valid finite override, and an explicit `0.0` rate.
//!
//! Apparatus: same as `backlog_00880b3d.rs` — record 1M opus input tokens
//! through the real `record` Stop hook with no config present, then write the
//! config under test and run the read command (gauge prices at read time).
//!
//! What this does NOT prove:
//! * only `session --json` and `status` are exercised — not `report`,
//!   `subagents`, or human-readable `session`;
//! * only the project-level `gauge.toml` is probed, not `~/.gauge/config.toml`;
//! * the `record` Stop hook under such a config is not exercised;
//! * misspelled keys INSIDE `[[pricing]]` are already covered elsewhere
//!   (`backlog_00880b3d.rs` probe 3) and are not re-probed;
//! * it does not pin the wording of the refusal, only rc != 0 + the file name;
//! * `-0.0` and very large finite rates are not probed.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

fn home() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!(
        "gauge-00880b3d-residual-{}-{n}",
        std::process::id()
    ));
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

/// Records session `s1` (1M opus input tokens) through the real `record` hook
/// with no config file present.
fn recorded_home() -> PathBuf {
    let h = home();
    assert!(!h.join("gauge.toml").exists());
    let tp = h.join("s1.jsonl");
    std::fs::write(
        &tp,
        "{\"type\":\"assistant\",\"timestamp\":\"2026-09-24T00:00:00.000Z\",\"message\":{\"role\":\"assistant\",\"model\":\"claude-opus-4-8\",\"usage\":{\"input_tokens\":1000000,\"output_tokens\":0}}}\n",
    )
    .unwrap();
    let payload = serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": "s1",
        "cwd": h.to_str().unwrap(),
        "transcript_path": tp.to_str().unwrap(),
    })
    .to_string();
    let (rc, _, err) = run(&h, &["record"], &payload);
    assert_eq!(rc, 0, "apparatus: record hook exited non-zero: {err}");
    assert!(
        h.join(".gauge/store/sessions/s1.json").exists(),
        "apparatus: record hook did not write the session record"
    );
    h
}

fn fixture(config: Option<&str>) -> PathBuf {
    let h = recorded_home();
    if let Some(text) = config {
        std::fs::write(h.join("gauge.toml"), text).unwrap();
    }
    h
}

/// `(rc, cost_usd if numeric, stdout, stderr)` of `session --json`.
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

/// The refusal contract: `session --json` AND `status` exit non-zero, and
/// stderr names `gauge.toml`.
fn assert_refused(h: &Path, what: &str) {
    let (rc, cost, out, err) = session_cost(h);
    eprintln!("{what} session: rc={rc} cost={cost:?} stdout={out:?} stderr={err:?}");
    assert!(
        rc != 0,
        "backlog 00880b3d: {what}: `session --json` exited 0 (cost_usd {cost:?}) from a \
         gauge.toml that cannot be applied as written; stdout={out}"
    );
    assert!(
        err.contains("gauge.toml"),
        "backlog 00880b3d: {what}: `session --json` stderr does not name gauge.toml: {err:?}"
    );
    let (rc, out, err) = run(h, &["status"], "");
    eprintln!("{what} status: rc={rc} stdout={out:?} stderr={err:?}");
    assert!(
        rc != 0,
        "backlog 00880b3d: {what}: `status` exited 0 from a gauge.toml that cannot be \
         applied as written; stdout={out}"
    );
    assert!(
        err.contains("gauge.toml"),
        "backlog 00880b3d: {what}: `status` stderr does not name gauge.toml: {err:?}"
    );
}

// ---------------------------------------------------------------- controls

#[test]
fn control_no_config_uses_builtin_price() {
    assert_known_cost(&fixture(None), 5.0, "control (no gauge.toml)");
}

#[test]
fn control_example_toml_verbatim_loads() {
    let text = include_str!("../gauge.example.toml");
    assert_known_cost(&fixture(Some(text)), 5.0, "control (gauge.example.toml)");
}

#[test]
fn control_example_toml_with_pricing_uncommented_loads() {
    let text = include_str!("../gauge.example.toml");
    let mut uncommented = 0;
    let out: String = text
        .lines()
        .map(|l| {
            let keep = ["[[pricing]]", "pattern =", "input =", "output ="];
            match l.strip_prefix("# ") {
                Some(rest) if keep.iter().any(|k| rest.starts_with(k)) => {
                    uncommented += 1;
                    rest.to_string()
                }
                _ => l.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        uncommented, 8,
        "apparatus: expected the two commented [[pricing]] examples (8 lines) in gauge.example.toml"
    );
    // The example's opus row is 5.0/25.0 == the built-in rate.
    assert_known_cost(
        &fixture(Some(&out)),
        5.0,
        "control (gauge.example.toml, [[pricing]] uncommented)",
    );
}

#[test]
fn control_init_starter_loads() {
    let h = recorded_home();
    let (rc, out, err) = run(&h, &["init"], "");
    assert_eq!(rc, 0, "apparatus: `gauge init` failed: {out} {err}");
    assert!(
        h.join("gauge.toml").exists(),
        "apparatus: init wrote no gauge.toml"
    );
    assert_known_cost(&h, 5.0, "control (`gauge init` starter)");
}

#[test]
fn control_valid_enabled_false_is_honoured() {
    let h = fixture(Some("enabled = false\n"));
    let (rc, out, err) = run(&h, &["status"], "");
    assert_eq!(rc, 0, "control: status rc={rc} stderr={err}");
    assert!(
        out.contains("enabled:      false"),
        "control: a correctly spelled `enabled = false` must be honoured: {out}"
    );
}

#[test]
fn control_valid_finite_override_applies() {
    assert_known_cost(
        &fixture(Some(
            "[[pricing]]\npattern = \"opus\"\ninput = 15.0\noutput = 75.0\n",
        )),
        15.0,
        "control (finite override)",
    );
}

#[test]
fn control_explicit_zero_rate_is_a_known_zero() {
    // 0.0 is finite and non-negative: a legitimate "free" model, not a refusal.
    assert_known_cost(
        &fixture(Some(
            "[[pricing]]\npattern = \"opus\"\ninput = 0.0\noutput = 0.0\n",
        )),
        0.0,
        "control (explicit 0/0 override)",
    );
}

// ------------------------------------------------------------- R1 probes

#[test]
fn r1_misspelled_pricing_table_is_refused_not_ignored() {
    // `[[pricng]]` would price opus at $0; today it is dropped and the
    // built-in $5.00 is reported with rc 0, `pricing: built-in`.
    assert_refused(
        &fixture(Some(
            "[[pricng]]\npattern = \"opus\"\ninput = 0.0\noutput = 0.0\n",
        )),
        "R1 `[[pricng]]`",
    );
}

#[test]
fn r1_misspelled_enabled_key_is_refused_not_ignored() {
    // `enabeld = false` — the operator meant to disable gauge; today it stays
    // enabled with rc 0.
    assert_refused(&fixture(Some("enabeld = false\n")), "R1 `enabeld = false`");
}

// ------------------------------------------------------------- R2 probes

#[test]
fn r2_nan_rate_is_refused() {
    assert_refused(
        &fixture(Some(
            "[[pricing]]\npattern = \"opus\"\ninput = nan\noutput = 25.0\n",
        )),
        "R2 `input = nan`",
    );
}

#[test]
fn r2_inf_rate_is_refused() {
    assert_refused(
        &fixture(Some(
            "[[pricing]]\npattern = \"opus\"\ninput = 5.0\noutput = inf\n",
        )),
        "R2 `output = inf`",
    );
}

#[test]
fn r2_negative_rate_is_refused() {
    assert_refused(
        &fixture(Some(
            "[[pricing]]\npattern = \"opus\"\ninput = -5.0\noutput = 25.0\n",
        )),
        "R2 `input = -5.0`",
    );
}
