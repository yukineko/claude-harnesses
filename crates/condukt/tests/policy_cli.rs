//! Integration coverage for `condukt policy decide` — the graded autonomy
//! policy engine's CLI surface and its exit-code contract. Spawns the built
//! binary so it exercises argument parsing + the process exit codes that
//! callers (skills, `state autonomy-check`) branch on. This test fails before
//! the `policy` subcommand exists (unrecognized subcommand -> clap exit 2 with
//! no `auto`/`block` stdout) and passes once it is wired = a genuine
//! Fail->Pass reproduction oracle for the wiring task.

use std::process::Command;

fn decide(risk: &str, reversible: &str, confidence: &str) -> (String, i32) {
    let out = Command::new(env!("CARGO_BIN_EXE_condukt"))
        .args([
            "policy",
            "decide",
            "--risk",
            risk,
            "--reversible",
            reversible,
            "--confidence",
            confidence,
        ])
        .output()
        .expect("spawn condukt");
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let code = out.status.code().expect("exited with a code");
    (stdout, code)
}

#[test]
fn high_risk_irreversible_blocks_exit_3() {
    let (stdout, code) = decide("high", "low", "high");
    assert_eq!(stdout, "block");
    assert_eq!(code, 3);
}

#[test]
fn low_risk_reversible_confident_autos_exit_0() {
    let (stdout, code) = decide("low", "high", "high");
    assert_eq!(stdout, "auto");
    assert_eq!(code, 0);
}

#[test]
fn ambiguous_middle_escalates_exit_2() {
    let (stdout, code) = decide("medium", "medium", "medium");
    assert_eq!(stdout, "escalate");
    assert_eq!(code, 2);
}

#[test]
fn unparseable_level_exits_1_without_panic() {
    let (stdout, code) = decide("catastrophic", "low", "high");
    // Invalid input: no decision word on stdout, nonzero-but-not-a-verdict exit.
    assert!(stdout.is_empty(), "expected no verdict, got {stdout:?}");
    assert_eq!(code, 1);
}

// --- `condukt policy answer` — the non-interactive question shim ------------
//
// The keystone of hands-off-the-loop autonomy: instead of the model obeying
// prose to "skip the AskUserQuestion when autonomous", a caller runs this shim
// with the question + options + the graded-autonomy levels. On an `auto`
// verdict it self-answers with the recommended option (no prompt) and journals
// the choice; on `escalate`/`block` it prints `answered:false` so the caller
// falls through to a real AskUserQuestion. These tests fail before the `answer`
// subcommand exists (unknown subcommand -> clap exit 2, no `answered:true`
// stdout, no journal) and pass once it is wired = a genuine Fail->Pass oracle.

fn answer_in(
    dir: &std::path::Path,
    risk: &str,
    reversible: &str,
    confidence: &str,
    recommend: &str,
) -> (String, i32) {
    let out = Command::new(env!("CARGO_BIN_EXE_condukt"))
        .args([
            "policy",
            "answer",
            "--risk",
            risk,
            "--reversible",
            reversible,
            "--confidence",
            confidence,
            "--question",
            "Adopt the proposed schedule?",
            "--option",
            "adopt",
            "--option",
            "revise",
            "--recommend",
            recommend,
            "--journal-dir",
            dir.to_str().unwrap(),
        ])
        .output()
        .expect("spawn condukt");
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let code = out.status.code().expect("exited with a code");
    (stdout, code)
}

#[test]
fn auto_verdict_self_answers_recommended_option_and_journals() {
    let dir = tempfile::tempdir().unwrap();
    let (stdout, code) = answer_in(dir.path(), "low", "high", "high", "0");
    assert_eq!(code, 0, "auto must exit 0; stdout={stdout:?}");
    assert!(
        stdout.contains("\"answered\":true"),
        "auto must self-answer; got {stdout:?}"
    );
    assert!(
        stdout.contains("\"chosen\":\"adopt\""),
        "must choose the recommended option; got {stdout:?}"
    );
    // The self-answer is journaled for audit ({question, chosen, policy}).
    let log = std::fs::read_to_string(dir.path().join("gate-decisions.jsonl"))
        .expect("decision must be journaled");
    assert!(log.contains("\"chosen\":\"adopt\""), "journal: {log:?}");
    assert!(
        log.contains("Adopt the proposed schedule?"),
        "journal must record the question: {log:?}"
    );
}

#[test]
fn auto_verdict_honours_the_recommend_index() {
    let dir = tempfile::tempdir().unwrap();
    let (stdout, code) = answer_in(dir.path(), "low", "high", "high", "1");
    assert_eq!(code, 0, "stdout={stdout:?}");
    assert!(
        stdout.contains("\"chosen\":\"revise\""),
        "recommend=1 must choose the second option; got {stdout:?}"
    );
}

/// Every row of the decision journal under `dir`, parsed. A missing file is a
/// failure: a resolved verdict must be on the record (see
/// [`assert_journaled_as_non_answer`] for why silence is not acceptable here).
fn journal_rows(dir: &std::path::Path) -> Vec<serde_json::Value> {
    let path = dir.join("gate-decisions.jsonl");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "a resolved verdict must be journaled at {}: {e}",
            path.display()
        )
    });
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("each journal line is one JSON object"))
        .collect()
}

/// The verdict was RECORDED but nothing was ANSWERED.
///
/// These tests used to assert "the journal file does not exist". That proxy
/// stood for "no self-answer was recorded", and it broke on 2026-09-07 when
/// condukt started journaling every resolved verdict — while the property it
/// stood for held. Asserting the property directly is strictly stronger: the
/// old check could not distinguish an unwritten journal from one containing a
/// forged `auto` row, and it also could not catch the opposite failure (a real
/// decision that left no trace at all).
fn assert_journaled_as_non_answer(dir: &std::path::Path, expected_policy: &str) {
    let rows = journal_rows(dir);
    assert_eq!(rows.len(), 1, "exactly one decision was made: {rows:?}");
    assert_eq!(
        rows[0]["policy"],
        serde_json::json!(expected_policy),
        "the row must record the verdict that actually happened: {rows:?}"
    );
    assert!(
        rows[0]["chosen"].is_null(),
        "a {expected_policy} answered nothing, so `chosen` must be null: {rows:?}"
    );
    assert!(
        !rows
            .iter()
            .any(|r| r["policy"] == serde_json::json!("auto")),
        "no row may claim `auto` — that is what downstream counts as \
         \"passed a gate without a human\": {rows:?}"
    );
}

#[test]
fn escalate_verdict_falls_through_and_is_journaled_as_a_non_answer() {
    let dir = tempfile::tempdir().unwrap();
    let (stdout, code) = answer_in(dir.path(), "medium", "medium", "medium", "0");
    assert_eq!(code, 2, "escalate must exit 2; stdout={stdout:?}");
    assert!(
        stdout.contains("\"answered\":false"),
        "escalate must not self-answer; got {stdout:?}"
    );
    assert!(stdout.contains("escalate"), "got {stdout:?}");
    // Nothing was self-answered — but the DECISION is still recorded, because
    // "the gate never fired" and "the gate fired and a human was asked" must
    // not be the same silence in `condukt policy answers`.
    assert_journaled_as_non_answer(dir.path(), "escalate");
}

#[test]
fn block_verdict_is_a_hard_stop_exit_3() {
    let dir = tempfile::tempdir().unwrap();
    let (stdout, code) = answer_in(dir.path(), "high", "low", "high", "0");
    assert_eq!(code, 3, "block must exit 3; stdout={stdout:?}");
    assert!(stdout.contains("\"answered\":false"), "got {stdout:?}");
    assert!(stdout.contains("block"), "got {stdout:?}");
    assert_journaled_as_non_answer(dir.path(), "block");
}

#[test]
fn answers_reader_replays_the_audit_trail() {
    let dir = tempfile::tempdir().unwrap();
    // Self-answer two questions...
    let _ = answer_in(dir.path(), "low", "high", "high", "0");
    let _ = answer_in(dir.path(), "low", "high", "high", "1");
    // ...and one escalate, which IS logged (as a decision, not an answer)...
    let _ = answer_in(dir.path(), "medium", "medium", "medium", "0");
    // ...then the reader replays all three rows as JSONL, in file order, with
    // the two self-answers distinguishable from the escalate by `chosen`.
    let out = Command::new(env!("CARGO_BIN_EXE_condukt"))
        .args([
            "policy",
            "answers",
            "--journal-dir",
            dir.path().to_str().unwrap(),
        ])
        .output()
        .expect("spawn condukt");
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(
        lines.len(),
        3,
        "two self-answers plus the escalate decision are logged: {stdout:?}"
    );
    let rows: Vec<serde_json::Value> = lines
        .iter()
        .map(|l| serde_json::from_str(l).expect("each replayed line is one JSON object"))
        .collect();
    // The two self-answers carry the option they picked...
    assert_eq!(rows[0]["policy"], serde_json::json!("auto"), "{}", lines[0]);
    assert_eq!(
        rows[0]["chosen"],
        serde_json::json!("adopt"),
        "{}",
        lines[0]
    );
    assert_eq!(rows[1]["policy"], serde_json::json!("auto"), "{}", lines[1]);
    assert_eq!(
        rows[1]["chosen"],
        serde_json::json!("revise"),
        "{}",
        lines[1]
    );
    // ...and the escalate carries none: it is a recorded DECISION to ask a
    // human, and must never be readable as an auto-approval.
    assert_eq!(
        rows[2]["policy"],
        serde_json::json!("escalate"),
        "{}",
        lines[2]
    );
    assert!(
        rows[2]["chosen"].is_null(),
        "an escalate answered nothing: {}",
        lines[2]
    );
    assert_eq!(
        rows.iter()
            .filter(|r| r["policy"] == serde_json::json!("auto"))
            .count(),
        2,
        "exactly two rows may read as auto-approved: {stdout:?}"
    );
}

#[test]
fn answers_reader_is_empty_when_no_log_exists() {
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_condukt"))
        .args([
            "policy",
            "answers",
            "--journal-dir",
            dir.path().to_str().unwrap(),
        ])
        .output()
        .expect("spawn condukt");
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty(), "no log -> no output");
}

#[test]
fn recommend_index_out_of_range_exits_1() {
    let dir = tempfile::tempdir().unwrap();
    // Auto verdict but the recommend index has no matching option -> invalid,
    // never silently picks the wrong option, never panics.
    let (stdout, code) = answer_in(dir.path(), "low", "high", "high", "9");
    assert_eq!(
        code, 1,
        "out-of-range recommend must exit 1; stdout={stdout:?}"
    );
    assert!(
        !stdout.contains("\"answered\":true"),
        "must not self-answer with a bogus index; got {stdout:?}"
    );
    assert!(!dir.path().join("gate-decisions.jsonl").exists());
}
