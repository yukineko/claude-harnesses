//! SPEC tests for the `condukt policy answer` gate-decision journal.
//!
//! `condukt policy answer` is the deterministic gate-skip shim: it turns a
//! graded-autonomy verdict into an answer, and exits `0`=auto / `2`=escalate /
//! `3`=block / `1`=invalid input. Today only the `auto` branch appends to
//! `gate-decisions.jsonl`; `escalate` and `block` leave no trace at all, and a
//! failed journal append is swallowed in silence
//! (`gatelog::append_decision` -> `harness_core::append::append_line`, every
//! error `let _ =`'d away).
//!
//! That makes the audit trail unable to distinguish "the gate escalated" from
//! "the gate was never consulted" — the fail-open shape CLAUDE.md §3 names:
//! an absent record reads downstream as "nothing happened". These tests pin the
//! intended behaviour:
//!
//!   1-3. all three verdicts are journaled and are DISTINGUISHABLE in the log;
//!   4.   `policy answers` replays escalate/block rows too;
//!   5.   a journal write that cannot succeed is REPORTED on stderr, while the
//!        primary verdict is still delivered (the gate keeps working; it merely
//!        stops being silent about the lost audit record);
//!   6-7. controls: `auto` still journals exactly as before, and a rejected
//!        malformed invocation journals NOTHING.
//!
//! Every test uses `--journal-dir <tempdir>`; none of them touch the real
//! `~/.condukt/state/`.

use std::path::Path;
use std::process::Command;

// ── harness ─────────────────────────────────────────────────────────────────

struct Run {
    stdout: String,
    stderr: String,
    code: i32,
}

const QUESTION: &str = "Adopt the proposed schedule?";
const OPTIONS: [&str; 2] = ["adopt", "revise"];

/// Drive `condukt policy answer` with the journal pointed at `journal_dir`.
/// The three graded-autonomy levels select the verdict (see `verdict_args`).
fn answer_in(
    journal_dir: &Path,
    risk: &str,
    reversible: &str,
    confidence: &str,
    recommend: &str,
) -> Run {
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
            QUESTION,
            "--option",
            OPTIONS[0],
            "--option",
            OPTIONS[1],
            "--recommend",
            recommend,
            "--journal-dir",
            journal_dir.to_str().expect("utf-8 journal dir"),
        ])
        .output()
        .expect("spawn condukt");
    Run {
        stdout: String::from_utf8_lossy(&out.stdout).trim().to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        code: out.status.code().expect("exited with a code, not a signal"),
    }
}

/// Level triples that produce each verdict. Mirrors `policy_cli.rs`, which
/// pins these same triples against `condukt policy decide`.
fn auto(dir: &Path) -> Run {
    answer_in(dir, "low", "high", "high", "0")
}
fn escalate(dir: &Path) -> Run {
    answer_in(dir, "medium", "medium", "medium", "0")
}
fn block(dir: &Path) -> Run {
    answer_in(dir, "high", "low", "high", "0")
}

fn journal_path(dir: &Path) -> std::path::PathBuf {
    dir.join("gate-decisions.jsonl")
}

/// The journal's non-empty lines, each parsed as one JSON object.
fn journal_rows(dir: &Path) -> Vec<serde_json::Value> {
    let path = journal_path(dir);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "the gate-decision journal {} must exist and be readable: {e}",
            path.display()
        )
    });
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            serde_json::from_str::<serde_json::Value>(l)
                .unwrap_or_else(|e| panic!("journal line is not one JSON object: {e} — {l:?}"))
        })
        .collect()
}

fn field<'a>(row: &'a serde_json::Value, key: &str) -> &'a serde_json::Value {
    row.get(key)
        .unwrap_or_else(|| panic!("journal row has no `{key}` field: {row}"))
}

/// A row for a verdict where nothing was answered must not name a choice.
/// An empty string (or an absent/null field) is fine; naming one of the offered
/// options is a lie in the audit trail.
fn assert_no_choice_claimed(row: &serde_json::Value) {
    let chosen = row.get("chosen");
    match chosen {
        None | Some(serde_json::Value::Null) => {}
        Some(serde_json::Value::String(s)) => {
            assert!(
                s.is_empty(),
                "nothing was answered, so the record must not claim a choice; \
                 `chosen` = {s:?} in row {row}"
            );
        }
        Some(other) => panic!("`chosen` must be a string (or absent), got {other} in row {row}"),
    }
    // Belt and braces: whatever shape it has, it must not be one of the options.
    let serialized = row.to_string();
    for opt in OPTIONS {
        assert!(
            !serialized.contains(&format!("\"chosen\":\"{opt}\"")),
            "nothing was answered, but the record names {opt:?} as chosen: {row}"
        );
    }
}

// ── 1. escalate is journaled ────────────────────────────────────────────────

#[test]
fn escalate_is_journaled_with_the_question_and_options() {
    let dir = tempfile::tempdir().unwrap();
    let run = escalate(dir.path());
    assert_eq!(run.code, 2, "escalate must exit 2; stdout={:?}", run.stdout);

    let rows = journal_rows(dir.path());
    assert_eq!(
        rows.len(),
        1,
        "one escalate verdict must append exactly one record, got {rows:?}"
    );
    let row = &rows[0];
    assert_eq!(
        field(row, "policy"),
        &serde_json::json!("escalate"),
        "the record must state the verdict that produced it: {row}"
    );
    assert_eq!(
        field(row, "question"),
        &serde_json::json!(QUESTION),
        "the escalated question must be preserved: {row}"
    );
    assert_eq!(
        field(row, "options"),
        &serde_json::json!(["adopt", "revise"]),
        "the offered options must be preserved: {row}"
    );
    assert_no_choice_claimed(row);
}

// ── 2. block is journaled ───────────────────────────────────────────────────

#[test]
fn block_is_journaled_with_the_question_and_options() {
    let dir = tempfile::tempdir().unwrap();
    let run = block(dir.path());
    assert_eq!(run.code, 3, "block must exit 3; stdout={:?}", run.stdout);

    let rows = journal_rows(dir.path());
    assert_eq!(
        rows.len(),
        1,
        "one block verdict must append exactly one record, got {rows:?}"
    );
    let row = &rows[0];
    assert_eq!(
        field(row, "policy"),
        &serde_json::json!("block"),
        "the record must state the verdict that produced it: {row}"
    );
    assert_eq!(
        field(row, "question"),
        &serde_json::json!(QUESTION),
        "the blocked question must be preserved: {row}"
    );
    assert_eq!(
        field(row, "options"),
        &serde_json::json!(["adopt", "revise"]),
        "the offered options must be preserved: {row}"
    );
    assert_no_choice_claimed(row);
}

// ── 3. the three verdicts are distinguishable in one log ────────────────────

#[test]
fn all_three_verdicts_are_distinguishable_in_one_journal() {
    let dir = tempfile::tempdir().unwrap();
    let a = auto(dir.path());
    let e = escalate(dir.path());
    let b = block(dir.path());
    assert_eq!(
        (a.code, e.code, b.code),
        (0, 2, 3),
        "exit-code contract precondition: auto=0 escalate=2 block=3"
    );

    let rows = journal_rows(dir.path());
    assert_eq!(
        rows.len(),
        3,
        "three gate consultations must leave three records; \
         a missing row makes 'the gate escalated' indistinguishable from \
         'the gate was never consulted'. got {rows:?}"
    );
    let policies: Vec<String> = rows
        .iter()
        .map(|r| {
            field(r, "policy")
                .as_str()
                .unwrap_or("<not-a-string>")
                .to_string()
        })
        .collect();
    assert_eq!(
        policies,
        vec!["auto", "escalate", "block"],
        "each record must carry its own distinct verdict, in call order"
    );
}

// ── 4. `policy answers` surfaces escalate/block rows ────────────────────────

#[test]
fn answers_reader_surfaces_escalate_and_block_rows() {
    let dir = tempfile::tempdir().unwrap();
    let _ = auto(dir.path());
    let _ = escalate(dir.path());
    let _ = block(dir.path());

    let out = Command::new(env!("CARGO_BIN_EXE_condukt"))
        .args([
            "policy",
            "answers",
            "--journal-dir",
            dir.path().to_str().unwrap(),
        ])
        .output()
        .expect("spawn condukt");
    assert_eq!(out.status.code(), Some(0), "`policy answers` must exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(
        lines.len(),
        3,
        "the review surface must replay all three consultations, not just the \
         self-answered one; got {stdout:?}"
    );
    let policies: Vec<String> = lines
        .iter()
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l)
                .unwrap_or_else(|e| panic!("`policy answers` line is not JSON: {e} — {l:?}"));
            v.get("policy")
                .and_then(|p| p.as_str())
                .unwrap_or("<missing>")
                .to_string()
        })
        .collect();
    assert!(
        policies.iter().any(|p| p == "escalate"),
        "an escalate row must be visible in `policy answers`: {policies:?}"
    );
    assert!(
        policies.iter().any(|p| p == "block"),
        "a block row must be visible in `policy answers`: {policies:?}"
    );
    assert!(
        policies.iter().any(|p| p == "auto"),
        "the auto row must still be visible in `policy answers`: {policies:?}"
    );
}

// ── 5. a journal write failure is not silent ────────────────────────────────
//
// Point `--journal-dir` at an existing REGULAR FILE: `create_dir_all` fails,
// the append cannot happen, and today every error is `let _ =`'d away. The gate
// must keep delivering its verdict (stdout JSON + contract exit code) AND must
// say on stderr that the audit record was lost.

/// A path that exists as a regular file, so it can never become a directory.
fn unusable_journal_dir(dir: &Path) -> std::path::PathBuf {
    let p = dir.join("not-a-directory");
    std::fs::write(&p, b"this is a regular file, not a journal directory\n").unwrap();
    p
}

fn assert_mentions_journal_failure(stderr: &str, ctx: &str) {
    assert!(
        !stderr.is_empty(),
        "{ctx}: a lost audit record must not be silent — stderr was empty"
    );
    let lower = stderr.to_lowercase();
    assert!(
        ["journal", "gate-decision", "decision log", "audit", "log"]
            .iter()
            .any(|needle| lower.contains(needle)),
        "{ctx}: stderr must say WHAT failed (the decision journal), got {stderr:?}"
    );
}

#[test]
fn auto_reports_a_journal_write_failure_yet_still_answers() {
    let tmp = tempfile::tempdir().unwrap();
    let bad = unusable_journal_dir(tmp.path());
    let run = auto(&bad);

    // (b) the primary verdict is still delivered, unchanged.
    assert_eq!(
        run.code, 0,
        "the gate must keep working: auto still exits 0. stderr={:?}",
        run.stderr
    );
    assert!(
        run.stdout.contains("\"answered\":true") && run.stdout.contains("\"chosen\":\"adopt\""),
        "the normal stdout JSON must still be printed, got {:?}",
        run.stdout
    );
    // (a) but the lost audit record is reported.
    assert_mentions_journal_failure(&run.stderr, "auto with an unwritable journal");
}

#[test]
fn escalate_reports_a_journal_write_failure_yet_still_escalates() {
    let tmp = tempfile::tempdir().unwrap();
    let bad = unusable_journal_dir(tmp.path());
    let run = escalate(&bad);

    assert_eq!(
        run.code, 2,
        "the gate must keep working: escalate still exits 2. stderr={:?}",
        run.stderr
    );
    assert!(
        run.stdout.contains("\"answered\":false") && run.stdout.contains("escalate"),
        "the normal stdout JSON must still be printed, got {:?}",
        run.stdout
    );
    assert_mentions_journal_failure(&run.stderr, "escalate with an unwritable journal");
}

#[test]
fn block_reports_a_journal_write_failure_yet_still_blocks() {
    let tmp = tempfile::tempdir().unwrap();
    let bad = unusable_journal_dir(tmp.path());
    let run = block(&bad);

    assert_eq!(
        run.code, 3,
        "the gate must keep working: block still exits 3. stderr={:?}",
        run.stderr
    );
    assert!(
        run.stdout.contains("\"answered\":false") && run.stdout.contains("block"),
        "the normal stdout JSON must still be printed, got {:?}",
        run.stdout
    );
    assert_mentions_journal_failure(&run.stderr, "block with an unwritable journal");
}

// ── 6. CONTROL: auto still journals correctly ───────────────────────────────
//
// Must be GREEN before AND after. Kills a vacuous implementation that "fixes"
// escalate/block journaling by breaking or reshaping the auto path.

#[test]
fn control_auto_still_journals_the_chosen_option_and_prints_its_json() {
    let dir = tempfile::tempdir().unwrap();
    let run = auto(dir.path());
    assert_eq!(run.code, 0, "auto must exit 0; stdout={:?}", run.stdout);
    assert!(
        run.stdout.contains("\"answered\":true"),
        "existing stdout JSON must be unchanged, got {:?}",
        run.stdout
    );
    assert!(
        run.stdout.contains("\"policy\":\"auto\""),
        "existing stdout JSON must be unchanged, got {:?}",
        run.stdout
    );
    assert!(
        run.stdout.contains("\"chosen\":\"adopt\""),
        "auto must self-answer with the recommended option, got {:?}",
        run.stdout
    );

    let rows = journal_rows(dir.path());
    assert_eq!(rows.len(), 1, "one auto = one record, got {rows:?}");
    let row = &rows[0];
    assert_eq!(field(row, "policy"), &serde_json::json!("auto"), "{row}");
    assert_eq!(
        field(row, "chosen"),
        &serde_json::json!("adopt"),
        "the journal must record the option actually chosen: {row}"
    );
    assert_eq!(
        field(row, "recommend_index"),
        &serde_json::json!(0),
        "{row}"
    );
    assert_eq!(
        field(row, "question"),
        &serde_json::json!(QUESTION),
        "{row}"
    );
    assert_eq!(
        field(row, "options"),
        &serde_json::json!(["adopt", "revise"]),
        "{row}"
    );
}

#[test]
fn control_auto_honours_a_nonzero_recommend_index_in_the_journal() {
    let dir = tempfile::tempdir().unwrap();
    let run = answer_in(dir.path(), "low", "high", "high", "1");
    assert_eq!(run.code, 0, "stdout={:?}", run.stdout);
    let rows = journal_rows(dir.path());
    assert_eq!(rows.len(), 1, "got {rows:?}");
    assert_eq!(
        field(&rows[0], "chosen"),
        &serde_json::json!("revise"),
        "recommend=1 must journal the second option: {}",
        rows[0]
    );
    assert_eq!(
        field(&rows[0], "recommend_index"),
        &serde_json::json!(1),
        "{}",
        rows[0]
    );
}

// ── 7. CONTROL: invalid input is NOT journaled ──────────────────────────────
//
// Must be GREEN before AND after. A rejected malformed invocation is not a
// decision; journaling it would let garbage inflate the audit trail.

#[test]
fn control_out_of_range_recommend_exits_1_and_journals_nothing() {
    let dir = tempfile::tempdir().unwrap();
    // `auto` levels, but --recommend 5 names no --option (only 2 were given).
    let run = answer_in(dir.path(), "low", "high", "high", "5");
    assert_eq!(
        run.code, 1,
        "invalid input must exit 1; stdout={:?} stderr={:?}",
        run.stdout, run.stderr
    );
    let path = journal_path(dir.path());
    assert!(
        !path.exists(),
        "a rejected malformed invocation is not a decision and must append \
         nothing; journal contents: {:?}",
        std::fs::read_to_string(&path).unwrap_or_default()
    );
}

#[test]
fn control_unparseable_level_exits_1_and_journals_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let run = answer_in(dir.path(), "catastrophic", "high", "high", "0");
    assert_eq!(
        run.code, 1,
        "an unparseable level must exit 1; stdout={:?} stderr={:?}",
        run.stdout, run.stderr
    );
    let path = journal_path(dir.path());
    assert!(
        !path.exists(),
        "a rejected malformed invocation is not a decision and must append \
         nothing; journal contents: {:?}",
        std::fs::read_to_string(&path).unwrap_or_default()
    );
}
