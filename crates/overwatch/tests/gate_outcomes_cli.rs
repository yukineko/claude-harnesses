// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Black-box CLI integration tests for the NOT-YET-IMPLEMENTED `overwatch
//! gate-outcomes [--json]` subcommand.
//!
//! Written from the contract file only:
//! `gate-outcomes-contract.md` (see the task prompt for the exact path this
//! was authored against). This test file does NOT read or depend on any
//! `crates/overwatch/src` implementation of the subcommand — at authoring
//! time it does not exist. These tests are expected to be RED (compile, but
//! fail at runtime / non-zero unexpected exit / stdout not matching) until
//! `gate-outcomes` is implemented.
//!
//! Sandbox strategy mirrors `tests/auto_approved_cli.rs`: HOME is
//! overridden per-test to an isolated temp directory so the machine-global
//! `$HOME/.<gate>/state/log.jsonl` sources can be seeded by hand without
//! touching the real gate logs on this machine.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

const GATES: [&str; 3] = ["donegate", "tdd", "reviewgate"];

fn make_sandbox(tag: &str) -> (PathBuf, PathBuf) {
    let n = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!(
        "overwatch-gate-outcomes-test-{tag}-{}-{n}",
        std::process::id()
    ));
    let home = base.join("home");
    let work = base.join("work");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    (home, work)
}

fn overwatch_bin() -> &'static str {
    env!("CARGO_BIN_EXE_overwatch")
}

fn gate_log_path(home: &Path, gate: &str) -> PathBuf {
    home.join(format!(".{gate}"))
        .join("state")
        .join("log.jsonl")
}

/// Seed a gate's `log.jsonl` from an ordered list of raw JSONL line strings
/// (each element is exactly one line; an empty string element is a genuinely
/// blank line). Lines are newline-joined with a trailing newline, matching
/// typical JSONL files.
fn write_gate_log(home: &Path, gate: &str, lines: &[&str]) {
    let path = gate_log_path(home, gate);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut content = lines.join("\n");
    content.push('\n');
    std::fs::write(&path, content).unwrap();
}

fn write_gate_log_bytes(home: &Path, gate: &str, bytes: &[u8]) {
    let path = gate_log_path(home, gate);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, bytes).unwrap();
}

fn run(home: &Path, work: &Path, json: bool) -> Output {
    let mut args = vec!["gate-outcomes"];
    if json {
        args.push("--json");
    }
    Command::new(overwatch_bin())
        .args(&args)
        .env("HOME", home)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(work)
        .output()
        .expect("failed to spawn overwatch binary")
}

fn run_json(home: &Path, work: &Path) -> (Value, i32) {
    let out = run(home, work, true);
    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8(out.stdout).expect("stdout not utf8");
    let v: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("gate-outcomes --json must be parseable: {e}\nstdout={stdout}"));
    (v, code)
}

fn gate<'a>(v: &'a Value, name: &str) -> &'a Value {
    v["gates"]
        .as_array()
        .expect("gates must be an array")
        .iter()
        .find(|g| g["gate"] == name)
        .unwrap_or_else(|| panic!("gate {name} missing from gates array: {v}"))
}

fn as_f64_close(v: &Value, expected: f64) {
    let got = v
        .as_f64()
        .unwrap_or_else(|| panic!("expected number, got {v}"));
    assert!(
        (got - expected).abs() < 1e-9,
        "expected {expected}, got {got}"
    );
}

// ---------------------------------------------------------------------
// 1. blocked then green in one session -> complied 1, firings 1, rates.complied 1.0
// ---------------------------------------------------------------------
#[test]
fn donegate_blocked_then_green_is_complied() {
    let (home, work) = make_sandbox("blocked-green");
    write_gate_log(
        &home,
        "donegate",
        &[
            r#"{"session":"s1","verdict":"blocked","ts":1,"attempt":1}"#,
            r#"{"session":"s1","verdict":"green","ts":2}"#,
        ],
    );

    let (v, code) = run_json(&home, &work);
    assert_eq!(code, 0, "no undetermined content expected: {v}");

    let g = gate(&v, "donegate");
    assert_eq!(g["status"], "read");
    assert_eq!(g["firings"], 1);
    assert_eq!(g["blocks"], 1);
    assert_eq!(g["complied"], 1);
    assert_eq!(g["overridden"], 0);
    assert_eq!(g["released_by_loop_bound"], 0);
    assert_eq!(g["unresolved"], 0);
    assert_eq!(g["escalated"], Value::Null);
    as_f64_close(&g["rates"]["complied"], 1.0);
    assert_eq!(g["override_reasons"].as_array().unwrap().len(), 0);
    assert_eq!(g["undetermined_lines"], 0);
}

// ---------------------------------------------------------------------
// 2. several consecutive blocked lines then green -> ONE firing, blocks == blocked line count
// ---------------------------------------------------------------------
#[test]
fn donegate_consecutive_blocks_collapse_into_one_firing() {
    let (home, work) = make_sandbox("consecutive-blocks");
    write_gate_log(
        &home,
        "donegate",
        &[
            r#"{"session":"s1","verdict":"blocked","ts":1}"#,
            r#"{"session":"s1","verdict":"blocked","ts":2}"#,
            r#"{"session":"s1","verdict":"blocked","ts":3}"#,
            r#"{"session":"s1","verdict":"green","ts":4}"#,
        ],
    );

    let (v, code) = run_json(&home, &work);
    assert_eq!(code, 0);
    let g = gate(&v, "donegate");
    assert_eq!(g["firings"], 1, "consecutive blocks must collapse: {g}");
    assert_eq!(g["blocks"], 3, "blocks counts raw blocked lines: {g}");
    assert_eq!(g["complied"], 1);
}

// ---------------------------------------------------------------------
// 3. blocked then skip_consumed(reason) -> overridden; reason clustering + ordering
// ---------------------------------------------------------------------
#[test]
fn donegate_skip_consumed_overrides_cluster_and_sort_reasons() {
    let (home, work) = make_sandbox("skip-consumed-cluster");
    write_gate_log(
        &home,
        "donegate",
        &[
            r#"{"session":"s1","verdict":"blocked","ts":1}"#,
            r#"{"session_id":"s1","event":"skip_consumed","reason":"A"}"#,
            r#"{"session":"s2","verdict":"blocked","ts":2}"#,
            r#"{"session_id":"s2","event":"skip_consumed","reason":"A"}"#,
            r#"{"session":"s3","verdict":"blocked","ts":3}"#,
            r#"{"session_id":"s3","event":"skip_consumed","reason":"C"}"#,
            r#"{"session":"s4","verdict":"blocked","ts":4}"#,
            r#"{"session_id":"s4","event":"skip_consumed","reason":"B"}"#,
        ],
    );

    let (v, code) = run_json(&home, &work);
    assert_eq!(code, 0);
    let g = gate(&v, "donegate");
    assert_eq!(g["firings"], 4);
    assert_eq!(g["overridden"], 4);
    let reasons = g["override_reasons"].as_array().unwrap();
    assert_eq!(
        reasons,
        &vec![
            serde_json::json!({"reason": "A", "count": 2}),
            serde_json::json!({"reason": "B", "count": 1}),
            serde_json::json!({"reason": "C", "count": 1}),
        ],
        "must be sorted by count desc, then reason asc: {reasons:?}"
    );
}

// ---------------------------------------------------------------------
// 4. blocked then giveup -> released_by_loop_bound (donegate plain verdict,
//    reviewgate `-giveup`-suffixed verdict)
// ---------------------------------------------------------------------
#[test]
fn blocked_then_giveup_is_released_by_loop_bound() {
    let (home, work) = make_sandbox("giveup");
    write_gate_log(
        &home,
        "donegate",
        &[
            r#"{"session":"s1","verdict":"blocked","ts":1}"#,
            r#"{"session":"s1","verdict":"giveup","ts":2}"#,
        ],
    );
    write_gate_log(
        &home,
        "reviewgate",
        &[
            r#"{"session":"s1","verdict":"blocked-inject","ts":1}"#,
            r#"{"session":"s1","verdict":"review-giveup","ts":2}"#,
        ],
    );

    let (v, code) = run_json(&home, &work);
    assert_eq!(code, 0, "recognized giveup verdicts, no undetermined: {v}");

    let dg = gate(&v, "donegate");
    assert_eq!(dg["firings"], 1);
    assert_eq!(dg["released_by_loop_bound"], 1);
    assert_eq!(dg["complied"], 0);
    assert_eq!(dg["unresolved"], 0);

    let rg = gate(&v, "reviewgate");
    assert_eq!(rg["firings"], 1);
    assert_eq!(
        rg["released_by_loop_bound"], 1,
        "any verdict ending in -giveup resolves the episode: {rg}"
    );
}

// ---------------------------------------------------------------------
// 5. blocked then nothing (EOF) -> unresolved 1; a pass verdict in a
//    DIFFERENT session must not resolve it
// ---------------------------------------------------------------------
#[test]
fn donegate_blocked_with_no_resolver_is_unresolved_cross_session_pass_ignored() {
    let (home, work) = make_sandbox("unresolved-eof");
    write_gate_log(
        &home,
        "donegate",
        &[
            r#"{"session":"s1","verdict":"blocked","ts":1}"#,
            r#"{"session":"s2","verdict":"green","ts":2}"#,
        ],
    );

    let (v, code) = run_json(&home, &work);
    assert_eq!(code, 0);
    let g = gate(&v, "donegate");
    assert_eq!(g["firings"], 1, "only s1 opened an episode: {g}");
    assert_eq!(g["unresolved"], 1);
    assert_eq!(
        g["complied"], 0,
        "s2's green must not resolve s1's open episode: {g}"
    );
}

// ---------------------------------------------------------------------
// 6. reviewgate blocked-inject -> already-reviewed complied;
//    tdd blocked -> ok complied
// ---------------------------------------------------------------------
#[test]
fn reviewgate_and_tdd_pass_verdicts_are_complied() {
    let (home, work) = make_sandbox("reviewgate-tdd-complied");
    write_gate_log(
        &home,
        "reviewgate",
        &[
            r#"{"session":"s1","verdict":"blocked-inject","ts":1}"#,
            r#"{"session":"s1","verdict":"already-reviewed","ts":2}"#,
        ],
    );
    write_gate_log(
        &home,
        "tdd",
        &[
            r#"{"session":"s1","verdict":"blocked","ts":1}"#,
            r#"{"session":"s1","verdict":"ok","ts":2}"#,
        ],
    );

    let (v, code) = run_json(&home, &work);
    assert_eq!(code, 0);

    let rg = gate(&v, "reviewgate");
    assert_eq!(rg["firings"], 1);
    assert_eq!(rg["complied"], 1);

    let tdd = gate(&v, "tdd");
    assert_eq!(tdd["firings"], 1);
    assert_eq!(tdd["complied"], 1);
}

// ---------------------------------------------------------------------
// 7. corrupt line, no-session-key line, unknown verdict, missing verdict/event,
//    blank line ignored -> undetermined_lines with correct 1-based line numbers;
//    exit code 3; other valid lines still classified
// ---------------------------------------------------------------------
#[test]
fn undetermined_lines_are_counted_with_correct_line_numbers_and_exit_3() {
    let (home, work) = make_sandbox("undetermined-lines");
    write_gate_log(
        &home,
        "donegate",
        &[
            /* 1 */ "this is not json at all",
            /* 2 */ r#"{"verdict":"blocked"}"#, // no string session key
            /* 3 */ r#"{"session":"s1","verdict":"blorp"}"#, // unrecognized verdict
            /* 4 */ "", // blank line: ignored
            /* 5 */ r#"{"session":"s3"}"#, // neither verdict nor event
            /* 6 */ r#"{"session":"s2","verdict":"blocked"}"#, // valid firing
            /* 7 */ r#"{"session":"s2","verdict":"green"}"#, // valid complied
        ],
    );

    let (v, code) = run_json(&home, &work);
    assert_eq!(code, 3, "undetermined lines present must exit 3: {v}");

    let g = gate(&v, "donegate");
    assert_eq!(g["undetermined_lines"], 4);
    let line_numbers: Vec<i64> = g["undetermined_line_numbers"]
        .as_array()
        .expect("undetermined_line_numbers must be an array")
        .iter()
        .map(|n| n.as_i64().unwrap())
        .collect();
    assert_eq!(
        line_numbers,
        vec![1, 2, 3, 5],
        "blank line 4 must be ignored, not undetermined: {g}"
    );

    // Valid lines (6, 7) must still be classified correctly.
    assert_eq!(g["firings"], 1);
    assert_eq!(g["blocks"], 1);
    assert_eq!(g["complied"], 1);
}

// ---------------------------------------------------------------------
// 8. absent logs -> status absent, zeros, rates null, escalated null, exit 0
// ---------------------------------------------------------------------
#[test]
fn absent_logs_report_zero_status_and_exit_0() {
    let (home, work) = make_sandbox("absent");
    // Intentionally never seed any gate log.

    let (v, code) = run_json(&home, &work);
    assert_eq!(code, 0, "absent logs must exit 0: {v}");

    for name in GATES {
        let g = gate(&v, name);
        assert_eq!(g["status"], "absent", "gate {name}: {g}");
        assert_eq!(g["firings"], 0, "gate {name}: {g}");
        assert_eq!(g["blocks"], 0, "gate {name}: {g}");
        assert_eq!(g["complied"], 0, "gate {name}: {g}");
        assert_eq!(g["overridden"], 0, "gate {name}: {g}");
        assert_eq!(g["released_by_loop_bound"], 0, "gate {name}: {g}");
        assert_eq!(g["unresolved"], 0, "gate {name}: {g}");
        assert_eq!(g["overrides_without_firing"], 0, "gate {name}: {g}");
        assert_eq!(g["releases_without_firing"], 0, "gate {name}: {g}");
        assert_eq!(g["undetermined_lines"], 0, "gate {name}: {g}");
        assert_eq!(g["escalated"], Value::Null, "gate {name}: {g}");
        // Contract line 45: rates is always the 4-key object shape;
        // "each null when firings == 0" describes the MEMBERS, not the
        // object itself (nothing in the contract says `rates` collapses
        // to JSON null).
        let rates = g["rates"]
            .as_object()
            .unwrap_or_else(|| panic!("rates must remain an object, gate {name}: {g}"));
        for key in [
            "complied",
            "overridden",
            "released_by_loop_bound",
            "unresolved",
        ] {
            assert_eq!(
                rates[key],
                Value::Null,
                "rates.{key} must be null when firings == 0, gate {name}: {g}"
            );
        }
    }
}

// ---------------------------------------------------------------------
// 9. unreadable log (non-UTF-8 bytes) -> status undetermined, counts null, exit 3
// ---------------------------------------------------------------------
#[test]
fn non_utf8_log_is_undetermined_with_null_counts_and_exit_3() {
    let (home, work) = make_sandbox("non-utf8");
    // 0xFF/0xFE are invalid UTF-8 lead bytes on their own.
    write_gate_log_bytes(&home, "donegate", &[0xFF, 0xFE, b'\n', b'{', b'}', b'\n']);

    let (v, code) = run_json(&home, &work);
    assert_eq!(code, 3, "unreadable log must exit 3: {v}");

    let g = gate(&v, "donegate");
    assert_eq!(g["status"], "undetermined", "{g}");
    assert_ne!(g["reason"], Value::Null, "reason must be set: {g}");
    assert_eq!(g["firings"], Value::Null, "{g}");
    assert_eq!(g["blocks"], Value::Null, "{g}");
    assert_eq!(g["complied"], Value::Null, "{g}");
    assert_eq!(g["overridden"], Value::Null, "{g}");
    assert_eq!(g["released_by_loop_bound"], Value::Null, "{g}");
    assert_eq!(g["unresolved"], Value::Null, "{g}");
    assert_eq!(g["undetermined_lines"], Value::Null, "{g}");
    // Contract line 45 fixes rates' shape as the 4-key object; line 51
    // ("the numeric count fields are null") reads most naturally as the
    // scalar fields (firings/blocks/complied/...), with rates continuing
    // to follow its own already-stated "each null" convention rather than
    // collapsing to JSON null itself.
    let rates = g["rates"]
        .as_object()
        .unwrap_or_else(|| panic!("rates must remain an object: {g}"));
    for key in [
        "complied",
        "overridden",
        "released_by_loop_bound",
        "unresolved",
    ] {
        assert_eq!(
            rates[key],
            Value::Null,
            "rates.{key} must be null under status undetermined: {g}"
        );
    }
    assert_eq!(g["escalated"], Value::Null, "{g}");
}

// ---------------------------------------------------------------------
// 10. skip_consumed with no open episode -> overrides_without_firing 1, firings 0
// ---------------------------------------------------------------------
#[test]
fn skip_consumed_without_open_episode_counts_overrides_without_firing() {
    let (home, work) = make_sandbox("override-no-firing");
    write_gate_log(
        &home,
        "donegate",
        &[r#"{"session_id":"s1","event":"skip_consumed","reason":"whatever"}"#],
    );

    let (v, code) = run_json(&home, &work);
    assert_eq!(code, 0);
    let g = gate(&v, "donegate");
    assert_eq!(g["firings"], 0, "{g}");
    assert_eq!(g["overridden"], 0, "{g}");
    assert_eq!(g["overrides_without_firing"], 1, "{g}");
    assert_eq!(
        g["override_reasons"].as_array().unwrap().len(),
        0,
        "orphan overrides must NOT feed override_reasons: {g}"
    );
}

// ---------------------------------------------------------------------
// 11. invariant: complied + overridden + released_by_loop_bound + unresolved == firings
// ---------------------------------------------------------------------
#[test]
fn invariant_holds_on_a_mixed_fixture() {
    let (home, work) = make_sandbox("invariant-mixed");
    write_gate_log(
        &home,
        "donegate",
        &[
            // s1: complied
            r#"{"session":"s1","verdict":"blocked","ts":1}"#,
            r#"{"session":"s1","verdict":"green","ts":2}"#,
            // s2: overridden
            r#"{"session":"s2","verdict":"blocked","ts":3}"#,
            r#"{"session_id":"s2","event":"skip_consumed","reason":"x"}"#,
            // s3: released_by_loop_bound
            r#"{"session":"s3","verdict":"blocked","ts":4}"#,
            r#"{"session":"s3","verdict":"giveup","ts":5}"#,
            // s4: unresolved (EOF)
            r#"{"session":"s4","verdict":"blocked","ts":6}"#,
        ],
    );

    let (v, code) = run_json(&home, &work);
    assert_eq!(code, 0);
    let g = gate(&v, "donegate");
    assert_eq!(g["firings"], 4, "{g}");
    assert_eq!(g["complied"], 1, "{g}");
    assert_eq!(g["overridden"], 1, "{g}");
    assert_eq!(g["released_by_loop_bound"], 1, "{g}");
    assert_eq!(g["unresolved"], 1, "{g}");

    let firings = g["firings"].as_i64().unwrap();
    let sum = g["complied"].as_i64().unwrap()
        + g["overridden"].as_i64().unwrap()
        + g["released_by_loop_bound"].as_i64().unwrap()
        + g["unresolved"].as_i64().unwrap();
    assert_eq!(sum, firings, "invariant violated: {g}");
}

// ---------------------------------------------------------------------
// 12. top-level fields: command, rev, date, scope, gates order
// ---------------------------------------------------------------------
#[test]
fn top_level_json_fields_are_well_formed() {
    let (home, work) = make_sandbox("top-level-fields");
    // No logs seeded: absent is the simplest fixture for structural checks.

    let (v, code) = run_json(&home, &work);
    assert_eq!(code, 0);

    assert_eq!(v["command"], "overwatch gate-outcomes --json");
    let rev = v["rev"].as_str().expect("rev must be a string");
    assert!(
        rev.starts_with("overwatch "),
        "rev must start with 'overwatch ': {rev}"
    );

    let date = v["date"].as_str().expect("date must be a string");
    let re = regex::Regex::new(r"^\d{4}-\d{2}-\d{2}$").unwrap();
    assert!(re.is_match(date), "date must be YYYY-MM-DD: {date}");

    assert_eq!(v["scope"], "machine-global");

    let gates = v["gates"].as_array().expect("gates must be an array");
    assert_eq!(gates.len(), 3, "gates: {gates:?}");
    let names: Vec<&str> = gates.iter().map(|g| g["gate"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["donegate", "tdd", "reviewgate"]);
}

// ---------------------------------------------------------------------
// 13. text mode (no --json)
// ---------------------------------------------------------------------
#[test]
fn text_mode_contains_required_substrings() {
    let (home, work) = make_sandbox("text-mode");
    write_gate_log(
        &home,
        "donegate",
        &[
            r#"{"session":"s1","verdict":"blocked","ts":1}"#,
            r#"{"session":"s1","verdict":"green","ts":2}"#,
        ],
    );
    write_gate_log(&home, "reviewgate", &["not json"]);

    let out = run(&home, &work, false);
    let stdout = String::from_utf8(out.stdout).expect("stdout not utf8");
    let code = out.status.code().unwrap_or(-1);

    assert!(
        stdout.contains("command: overwatch gate-outcomes"),
        "stdout={stdout}"
    );
    assert!(stdout.contains("rev: overwatch "), "stdout={stdout}");
    assert!(stdout.contains("date: "), "stdout={stdout}");
    assert!(stdout.contains("machine-global"), "stdout={stdout}");
    assert!(stdout.contains("escalated n/a"), "stdout={stdout}");
    assert!(
        stdout.contains("firings 1"),
        "one-firing fixture must show firings 1: stdout={stdout}"
    );
    assert!(
        stdout.contains("UNDETERMINED"),
        "an undetermined line/log must show UNDETERMINED: stdout={stdout}"
    );
    assert_eq!(
        code, 3,
        "text mode must still exit 3 when undetermined content is present"
    );
}
