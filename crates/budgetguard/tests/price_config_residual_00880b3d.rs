// Integration test file: unwrap/expect/panic are fine here (the workspace
// clippy denies are for production code).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Residual repro for backlog 00880b3d (budgetguard twin of the gauge
//! `[[pricing]]` fix), written by an independent test author.
//!
//! budgetguard's `Config::load_checked` already resolves an unreadable or
//! unparseable `budgetguard.toml` to `Undetermined`, and `main` turns that
//! into `gate::config_undetermined_result`: the Stop hook BLOCKS with a
//! reason that names the config file ("設定を読み取れませんでした（budgetguard
//! config <path>/budgetguard.toml could not be parsed …）"). Observed at HEAD
//! with the apparatus below (control `control_unparseable_config_blocks_naming_the_file`).
//!
//! But a `[[price]]` row that parses yet cannot be applied as written is
//! accepted verbatim (`PriceOverrideCfg` -> `PriceOverride` with no checks):
//! * `pattern = ""` — the empty substring matches EVERY model, so
//!   `pattern = "" input = 0.0 output = 0.0` prices opus at $0 and the armed
//!   $1.00 session block allows a $5.00 session (observed: `session $0.0000`);
//! * `input = nan` / `output = inf` — the session cost becomes NaN
//!   (observed: `session $NaN`), NaN > limit is false, the stop is allowed;
//! * `input = -5.0` — the session cost is -$5.00, the stop is allowed.
//!
//! Required behaviour pinned here: the same observable outcome as an
//! unparseable config — the gate blocks and the block reason names
//! `budgetguard.toml` (so it is the config, not the spend, being reported).
//!
//! Apparatus (as `backlog_ff98fed0.rs`): isolated HOME/cwd, `budgetguard.toml`
//! with `[session] block_usd = 1.0` plus the case's `[[price]]`, a transcript
//! of one opus turn with 1,000,000 input tokens ($5.00 at the built-in rate),
//! then the real `budgetguard gate` Stop hook.
//!
//! What this does NOT prove:
//! * only the project-level `budgetguard.toml`, not `~/.budgetguard/config.toml`;
//! * only the transcript-parse cost source, not a gauge-record source;
//! * not the daily / warn limits, nor a config with NO limit armed (whether an
//!   unusable `[[price]]` row must block an otherwise-disabled gate is not
//!   pinned here — every case has `block_usd = 1.0`);
//! * a whitespace-only pattern (`"  "`), `-0.0`, and a NaN/negative rate on a
//!   row whose pattern matches no model in the session are not probed;
//! * the exact wording / check_kind of the block is not pinned — only
//!   "blocks" + "reason names budgetguard.toml".

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

fn temp_home(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "budgetguard-00880b3d-{tag}-{}-{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `budgetguard.toml` = `[session] block_usd = 1.0` + `extra_toml`; one opus
/// turn of 1M input tokens; run the real Stop hook. Returns (code, stdout, stderr).
fn run_gate(tag: &str, extra_toml: &str) -> (i32, String, String) {
    let home = temp_home(tag);
    std::fs::write(
        home.join("budgetguard.toml"),
        format!("[session]\nblock_usd = 1.0\n{extra_toml}"),
    )
    .unwrap();
    let tp = home.join("transcript.jsonl");
    std::fs::write(
        &tp,
        "{\"type\":\"assistant\",\"timestamp\":\"2026-06-22T10:00:01Z\",\"message\":{\"model\":\"claude-opus-4-8\",\"content\":[{\"type\":\"text\",\"text\":\"x\"}],\"usage\":{\"input_tokens\":1000000,\"output_tokens\":0,\"cache_read_input_tokens\":0}}}\n",
    )
    .unwrap();
    let payload = serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": format!("00880b3d-{tag}"),
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

/// The block reason, if the gate emitted `{"decision":"block"}`.
fn block_reason(stdout: &str) -> Option<String> {
    stdout.lines().find_map(|l| {
        let v = serde_json::from_str::<serde_json::Value>(l).ok()?;
        (v["decision"] == "block").then(|| v["reason"].as_str().unwrap_or("").to_string())
    })
}

/// Required outcome: block, and the reason names the config file.
fn assert_config_undetermined_block(tag: &str, extra: &str, what: &str) {
    let (code, out, err) = run_gate(tag, extra);
    assert_eq!(
        code, 0,
        "{what}: apparatus: gate exited {code}; stderr={err}"
    );
    let reason = block_reason(&out);
    assert!(
        reason
            .as_deref()
            .is_some_and(|r| r.contains("budgetguard.toml")),
        "backlog 00880b3d: {what}: a [[price]] row that cannot be applied as written must be \
         treated like an unparseable budgetguard.toml (block naming the file); got \
         block_reason={reason:?} stderr={err:?}"
    );
}

// ---------------------------------------------------------------- controls

/// The observable outcome the probes require, measured on the existing
/// Undetermined path (a syntax error).
#[test]
fn control_unparseable_config_blocks_naming_the_file() {
    let (code, out, err) = run_gate("unparseable", "this is not toml\n");
    assert_eq!(code, 0, "stderr: {err}");
    let reason = block_reason(&out).unwrap_or_else(|| panic!("no block: {out} {err}"));
    assert!(reason.contains("budgetguard.toml"), "{reason}");
}

/// No `[[price]]`: opus 1M input = $5.00 over the $1.00 block -> a SPEND block.
#[test]
fn control_no_override_blocks_on_spend() {
    let (code, out, err) = run_gate("none", "");
    assert_eq!(code, 0, "stderr: {err}");
    let reason = block_reason(&out).unwrap_or_else(|| panic!("no block: {out} {err}"));
    assert!(
        !reason.contains("budgetguard.toml"),
        "apparatus: the spend block must not name the config file, or the probes' \
         discriminator is vacuous: {reason}"
    );
}

/// A valid finite override is applied: opus at 0.5/0.5 -> $0.50 < $1.00 -> allow.
#[test]
fn control_valid_finite_override_allows() {
    let (code, out, err) = run_gate(
        "valid",
        "\n[[price]]\npattern = \"opus\"\ninput = 0.5\noutput = 0.5\n",
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        block_reason(&out).is_none(),
        "a valid override pricing the session under the limit must allow: {out} {err}"
    );
}

/// An explicit 0/0 rate on a non-empty pattern is a legitimate known $0.
#[test]
fn control_explicit_zero_rate_on_named_pattern_allows() {
    let (code, out, err) = run_gate(
        "zero",
        "\n[[price]]\npattern = \"opus\"\ninput = 0.0\noutput = 0.0\n",
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        block_reason(&out).is_none(),
        "an explicit 0/0 opus override is a known $0 and must allow: {out} {err}"
    );
}

// ------------------------------------------------------------------ probes

#[test]
fn r3_empty_pattern_is_config_undetermined_not_a_wildcard() {
    assert_config_undetermined_block(
        "empty-pattern",
        "\n[[price]]\npattern = \"\"\ninput = 0.0\noutput = 0.0\n",
        "R3 `pattern = \"\"`",
    );
}

#[test]
fn r3_nan_rate_is_config_undetermined() {
    assert_config_undetermined_block(
        "nan",
        "\n[[price]]\npattern = \"opus\"\ninput = nan\noutput = 25.0\n",
        "R3 `input = nan`",
    );
}

#[test]
fn r3_inf_rate_is_config_undetermined() {
    assert_config_undetermined_block(
        "inf",
        "\n[[price]]\npattern = \"opus\"\ninput = 5.0\noutput = inf\n",
        "R3 `output = inf`",
    );
}

#[test]
fn r3_negative_rate_is_config_undetermined() {
    assert_config_undetermined_block(
        "negative",
        "\n[[price]]\npattern = \"opus\"\ninput = -5.0\noutput = 25.0\n",
        "R3 `input = -5.0`",
    );
}
