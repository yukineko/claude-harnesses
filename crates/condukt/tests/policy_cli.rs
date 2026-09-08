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

#[test]
fn escalate_verdict_falls_through_but_is_still_journaled() {
    let dir = tempfile::tempdir().unwrap();
    let (stdout, code) = answer_in(dir.path(), "medium", "medium", "medium", "0");
    assert_eq!(code, 2, "escalate must exit 2; stdout={stdout:?}");
    assert!(
        stdout.contains("\"answered\":false"),
        "escalate must not self-answer; got {stdout:?}"
    );
    assert!(stdout.contains("escalate"), "got {stdout:?}");
    // Nothing was self-answered, but the consultation IS recorded: the audit
    // trail must distinguish "escalated to a human" from "never consulted".
    assert_journaled_but_not_self_answered(dir.path(), "escalate");
}

#[test]
fn block_verdict_is_a_hard_stop_exit_3() {
    let dir = tempfile::tempdir().unwrap();
    let (stdout, code) = answer_in(dir.path(), "high", "low", "high", "0");
    assert_eq!(code, 3, "block must exit 3; stdout={stdout:?}");
    assert!(stdout.contains("\"answered\":false"), "got {stdout:?}");
    assert!(stdout.contains("block"), "got {stdout:?}");
    assert_journaled_but_not_self_answered(dir.path(), "block");
}

#[test]
fn answers_reader_replays_the_audit_trail() {
    let dir = tempfile::tempdir().unwrap();
    // Self-answer two questions...
    let _ = answer_in(dir.path(), "low", "high", "high", "0");
    let _ = answer_in(dir.path(), "low", "high", "high", "1");
    // ...and an escalate, which is logged too (as a consultation)...
    let _ = answer_in(dir.path(), "medium", "medium", "medium", "0");
    // ...then the reader replays all three as JSONL.
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
        "all three consultations are logged, not just the self-answers: {stdout:?}"
    );
    assert!(lines[0].contains("\"chosen\":\"adopt\""), "{}", lines[0]);
    assert!(lines[1].contains("\"chosen\":\"revise\""), "{}", lines[1]);
    // The escalate is recorded as a consultation with no choice taken.
    assert!(lines[2].contains("\"policy\":\"escalate\""), "{}", lines[2]);
    assert!(lines[2].contains("\"chosen\":\"\""), "{}", lines[2]);
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
    // Still journals NOTHING: a malformed invocation is a rejected input,
    // not a decision, so it must not inflate the audit trail.
    assert!(!dir.path().join("gate-decisions.jsonl").exists());
}

/// Assert this consultation is journaled **without** being recorded as a
/// self-answer: exactly one row, carrying `expected_policy`, naming no chosen
/// option.
///
/// This replaces an older `assert!(!gate-decisions.jsonl.exists())`. That check
/// and this one protect the same named invariant — the message on the old
/// assertion was "must not be journaled *as a self-answer*" — but file-absence
/// was a proxy that happened to hold only because escalate/block were not
/// recorded at all. Now that every consultation is journaled, the proxy would
/// fail while the invariant holds, so it is asserted directly. This form is
/// strictly stronger on the invariant's own axis: absence proved nothing about
/// what a row would have said, whereas this rejects a row claiming `auto` or
/// naming a choice.
fn assert_journaled_but_not_self_answered(dir: &std::path::Path, expected_policy: &str) {
    let path = dir.join("gate-decisions.jsonl");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "the consultation must be journaled at {}: {e} — an unrecorded escalate is \
             indistinguishable from a gate that never fired",
            path.display()
        )
    });
    let rows: Vec<serde_json::Value> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            serde_json::from_str(l)
                .unwrap_or_else(|e| panic!("journal line is not JSON: {e} — {l:?}"))
        })
        .collect();
    assert_eq!(rows.len(), 1, "exactly one consultation expected: {rows:?}");
    let row = &rows[0];
    assert_eq!(
        row["policy"],
        serde_json::json!(expected_policy),
        "wrong verdict recorded: {row}"
    );
    assert_eq!(
        row["chosen"],
        serde_json::json!(""),
        "nothing was self-answered, so no option may be recorded as chosen: {row}"
    );
}
