//! Spec `sitecraft-capabilities` R1 — the purpose record (rulings D1, D8).
//!
//! # CLI surface assumed (rulings D1 unless noted)
//!
//! * `sitecraft --root <root> decide purpose --text <summary>` stores
//!   `[purpose] text = <summary trimmed>` in `<root>/sitecraft/decisions.toml`,
//!   exit 0. A whitespace-only `--text` leaves the file byte-identical
//!   (absent stays absent) and exits **1**.
//! * `sitecraft decide check`: `[purpose]` missing, or its `text` blank, is
//!   exit 1 even when every catalog decision is recorded; its output
//!   (stdout+stderr) contains `` sitecraft decide purpose --text ``.
//! * `sitecraft decide next` never returns `purpose` as a question
//!   (`area` / `question_id` never `purpose`; with every area settled and no
//!   purpose it prints `{"status":"done"}`).
//! * Re-recording a different purpose appends exactly one `[[history]]` row
//!   with `area = "purpose"`, `question = "text"`, `old` = previous text,
//!   `new` = new text (D1, D8).
//!
//! Already-true guards (hold at HEAD, kept next to RED tests of the same
//! requirement): `next_never_asks_purpose_*`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod spec_common;

use spec_common::*;

fn purpose_text(p: &Project) -> Option<String> {
    p.decisions()
        .get("purpose")
        .and_then(|t| t.get("text"))
        .and_then(|t| t.as_str())
        .map(str::to_string)
}

#[test]
fn purpose_text_is_recorded_trimmed() {
    let p = Project::new();
    p.write_decisions(&areas_all_irrelevant());
    let o = p.run(&[
        "decide",
        "purpose",
        "--text",
        "  A VRM viewer for my avatars.  ",
    ]);
    expect_code(&o, 0, "decide purpose --text");
    assert_eq!(
        purpose_text(&p).as_deref(),
        Some("A VRM viewer for my avatars."),
        "[purpose] text must hold the trimmed summary"
    );
}

#[test]
fn purpose_on_absent_record_creates_it() {
    let p = Project::new();
    let o = p.run(&[
        "decide",
        "purpose",
        "--text",
        "Convert GLB files to previews.",
    ]);
    expect_code(&o, 0, "decide purpose on an absent record");
    assert_eq!(
        purpose_text(&p).as_deref(),
        Some("Convert GLB files to previews.")
    );
}

#[test]
fn blank_purpose_text_is_exit_1_and_changes_nothing() {
    let p = Project::new();
    let body = format!("{}{}", purpose("Original purpose."), areas_all_irrelevant());
    p.write_decisions(&body);
    let before = p.decisions_bytes();
    let o = p.run(&["decide", "purpose", "--text", "   \t "]);
    expect_code(&o, 1, "decide purpose with a whitespace-only --text");
    assert_eq!(
        p.decisions_bytes(),
        before,
        "decisions.toml must not change"
    );

    // Positive control in the same root: a real text is accepted.
    let ok = p.run(&["decide", "purpose", "--text", "Replaced purpose."]);
    expect_code(&ok, 0, "decide purpose with a real text");
}

#[test]
fn blank_purpose_on_absent_record_is_exit_1_and_creates_nothing() {
    let p = Project::new();
    let o = p.run(&["decide", "purpose", "--text", "  "]);
    expect_code(
        &o,
        1,
        "decide purpose with a blank --text on an absent record",
    );
    assert!(
        p.decisions_bytes().is_none(),
        "no decisions.toml may be created by a rejected purpose"
    );
}

#[test]
fn check_without_purpose_is_exit_1_with_the_purpose_hint() {
    let p = Project::new();
    p.write_decisions(&areas_all_irrelevant());
    let o = p.run(&["decide", "check"]);
    expect_code(
        &o,
        1,
        "decide check with every area recorded but no [purpose]",
    );
    assert!(
        o.all().contains("sitecraft decide purpose --text"),
        "decide check must say how to record the purpose\n{}",
        o.describe()
    );
    // Positive control: recording the purpose makes the same record clean.
    expect_code(
        &p.run(&["decide", "purpose", "--text", "A site."]),
        0,
        "decide purpose",
    );
    expect_code(
        &p.run(&["decide", "check"]),
        0,
        "decide check after purpose",
    );
}

#[test]
fn check_with_blank_purpose_text_is_exit_1_with_the_purpose_hint() {
    let p = Project::new();
    p.write_decisions(&format!(
        "[purpose]\ntext = \"   \"\n\n{}",
        areas_all_irrelevant()
    ));
    let o = p.run(&["decide", "check"]);
    expect_code(&o, 1, "decide check with a whitespace-only [purpose] text");
    assert!(
        o.all().contains("sitecraft decide purpose --text"),
        "decide check must say how to record the purpose\n{}",
        o.describe()
    );
}

#[test]
fn check_with_purpose_and_all_areas_is_clean() {
    let p = Project::new();
    p.write_decisions(&complete_record());
    expect_code(
        &p.run(&["decide", "check"]),
        0,
        "decide check on purpose + every area recorded",
    );
}

#[test]
fn next_never_asks_purpose_when_everything_else_is_settled() {
    let p = Project::new();
    p.write_decisions(&areas_all_irrelevant());
    let o = p.run(&["decide", "next"]);
    expect_code(&o, 0, "decide next");
    let v = o.json_objects().pop().expect("decide next prints JSON");
    assert_eq!(
        v["status"], "done",
        "decide next must not turn the missing purpose into a question: {v}"
    );
}

#[test]
fn next_never_asks_purpose_on_an_empty_record() {
    let p = Project::new();
    let o = p.run(&["decide", "next"]);
    expect_code(&o, 0, "decide next");
    let v = o.json_objects().pop().expect("decide next prints JSON");
    assert_ne!(v["area"], "purpose", "{v}");
    assert_ne!(v["question_id"], "purpose", "{v}");
}

#[test]
fn rerecording_a_different_purpose_appends_one_history_row() {
    let p = Project::new();
    p.write_decisions(&areas_all_irrelevant());
    expect_code(
        &p.run(&["decide", "purpose", "--text", "First purpose."]),
        0,
        "first purpose",
    );
    let before = p.history().len();
    expect_code(
        &p.run(&["decide", "purpose", "--text", "Second purpose."]),
        0,
        "second purpose",
    );
    let h = p.history();
    assert_eq!(
        h.len(),
        before + 1,
        "exactly one [[history]] row per changed purpose: {h:?}"
    );
    let last = h.last().unwrap();
    assert_eq!(last["area"].as_str(), Some("purpose"), "{last:?}");
    assert_eq!(last["question"].as_str(), Some("text"), "{last:?}");
    assert_eq!(last["old"].as_str(), Some("First purpose."), "{last:?}");
    assert_eq!(last["new"].as_str(), Some("Second purpose."), "{last:?}");
    assert_eq!(purpose_text(&p).as_deref(), Some("Second purpose."));
}
