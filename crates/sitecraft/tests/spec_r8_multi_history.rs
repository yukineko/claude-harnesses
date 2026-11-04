//! Spec `sitecraft-capabilities` R8 — multi-select answers, notes, any-of
//! follow-ups and the change history (rulings D8).
//!
//! # CLI surface assumed (rulings D8)
//!
//! * `sitecraft --root <root> decide record --area A --question Q --answer V
//!   [--answer V2 …] [--note TEXT]`. `--answer` is repeatable.
//! * Multi questions: in the catalog only `notification.channel`; domain
//!   questions via `decide add-question --id I --text T [--choice C]… --multi`.
//!   A multi answer is stored as a TOML array (also for one value); an empty
//!   element, a duplicate, or an element outside `choices` → non-zero exit
//!   with decisions.toml byte-identical. Two `--answer`s for a non-multi
//!   question → the same.
//! * Reading: a legacy string under a multi question is a 1-element array;
//!   an array under a non-multi question → `decide check` exit 2.
//! * Follow-ups are any-of: `channel = ["web-push", "email"]` makes
//!   `decide next` ask `notification` / `email-sender`.
//! * `--note`: catalog answer → `[areas.<a>.notes] <q> = TEXT`; domain
//!   question → its `[[questions]]` entry `note = TEXT`. Re-recording without
//!   `--note` keeps the note.
//! * History: every record that changes a value (answer, relevance, note,
//!   purpose, capability) appends one `[[history]]` row
//!   `{area, question, old, new, at}` (`at` UTC RFC 3339; a note change uses
//!   `question = "<q>#note"`), and stdout gets exactly one line
//!   `changed: <old> -> <new>`. Re-recording the same value adds no row and
//!   prints no `changed:` line. An answer dropped by follow-up pruning is a
//!   row with `new = ""` and `pruned = true`.
//!
//! The record starts with every area irrelevant except the ones a test makes
//! relevant through the CLI (no purpose: R8 does not depend on R1).
//!
//! Already-true guard: `array_under_a_non_multi_question_is_exit_2` (HEAD
//! already rejects arrays everywhere; kept so the multi work cannot loosen it).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod spec_common;

use sitecraft::decide::catalog;
use sitecraft::Determination;
use spec_common::*;

/// Every area irrelevant except `relevant`, recorded through the
/// CLI (so the relevance decision itself is ordinary).
fn project(relevant: &[&str]) -> Project {
    let p = Project::new();
    let mut s = String::new();
    for a in AREAS {
        if !relevant.contains(&a) {
            s.push_str(&format!("[areas.{a}]\nrelevance = \"irrelevant\"\n\n"));
        }
    }
    p.write_decisions(&s);
    for a in relevant {
        let o = p.run(&[
            "decide",
            "record",
            "--area",
            a,
            "--question",
            "relevance",
            "--answer",
            "relevant",
        ]);
        expect_code(&o, 0, &format!("setup: {a} relevant"));
    }
    p
}

fn record(p: &Project, area: &str, q: &str, answers: &[&str], note: Option<&str>) -> Out {
    let mut args = vec!["decide", "record", "--area", area, "--question", q];
    for a in answers {
        args.push("--answer");
        args.push(a);
    }
    if let Some(n) = note {
        args.push("--note");
        args.push(n);
    }
    p.run(&args)
}

fn answer(p: &Project, area: &str, q: &str) -> toml::Value {
    p.decisions()["areas"][area]["answers"]
        .get(q)
        .cloned()
        .unwrap_or_else(|| panic!("no answer for {area}/{q}"))
}

fn strs(v: &toml::Value) -> Vec<String> {
    v.as_array()
        .unwrap_or_else(|| panic!("expected an array, got {v:?}"))
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect()
}

fn changed_lines(o: &Out) -> Vec<String> {
    o.stdout
        .lines()
        .filter(|l| l.starts_with("changed: "))
        .map(str::to_string)
        .collect()
}

#[test]
fn channel_is_multi_and_stores_an_array() {
    let p = project(&["notification"]);
    let o = record(&p, "notification", "channel", &["web-push", "email"], None);
    expect_code(&o, 0, "record channel = web-push + email");
    assert_eq!(
        strs(&answer(&p, "notification", "channel")),
        vec!["web-push", "email"]
    );
    let o = record(&p, "notification", "channel", &["in-app"], None);
    expect_code(&o, 0, "record channel = in-app");
    assert_eq!(
        strs(&answer(&p, "notification", "channel")),
        vec!["in-app"],
        "a single multi answer is still an array"
    );
}

#[test]
fn only_channel_is_multi_in_the_catalog() {
    let all = match catalog::areas() {
        Determination::Known(a) => a,
        Determination::Undetermined(u) => panic!("catalog: {}", u.as_str()),
    };
    let p = project(&AREAS);
    let mut checked = 0;
    for area in &all {
        for q in area
            .questions
            .iter()
            .filter(|q| q.when.is_none() && q.choices.len() >= 2)
        {
            let before = p.decisions_bytes();
            let o = record(&p, &area.id, &q.id, &[&q.choices[0], &q.choices[1]], None);
            if area.id == "notification" && q.id == "channel" {
                expect_code(&o, 0, "channel accepts two answers");
            } else {
                expect_failure(&o, &format!("{}/{} with two answers", area.id, q.id));
                assert_eq!(
                    p.decisions_bytes(),
                    before,
                    "{}/{} must not change the record",
                    area.id,
                    q.id
                );
                checked += 1;
            }
        }
    }
    assert!(
        checked >= 15,
        "setup: too few questions checked ({checked})"
    );
}

#[test]
fn invalid_multi_answers_change_nothing() {
    let p = project(&["notification"]);
    expect_code(
        &record(&p, "notification", "channel", &["email"], None),
        0,
        "setup",
    );
    for (what, answers) in [
        ("empty element", vec!["email", ""]),
        ("duplicate", vec!["email", "email"]),
        ("outside choices", vec!["email", "fax"]),
    ] {
        let before = p.decisions_bytes();
        let o = record(&p, "notification", "channel", &answers, None);
        expect_failure(&o, what);
        assert_eq!(
            p.decisions_bytes(),
            before,
            "{what}: the record must not change"
        );
    }
    // Positive control in the same root: a valid two-element answer is
    // accepted, so the failures above are about the elements, not about
    // `--answer` being repeated.
    expect_code(
        &record(&p, "notification", "channel", &["email", "web-push"], None),
        0,
        "valid multi answer",
    );
}

#[test]
fn domain_question_can_be_multi() {
    let p = project(&[]);
    let o = p.run(&[
        "decide",
        "add-question",
        "--id",
        "formats-in",
        "--text",
        "Which formats come in?",
        "--choice",
        "glb",
        "--choice",
        "fbx",
        "--choice",
        "obj",
        "--multi",
    ]);
    expect_code(&o, 0, "add-question --multi");
    let o = record(&p, "domain", "formats-in", &["glb", "obj"], None);
    expect_code(&o, 0, "record two answers to a multi domain question");
    let q = p.decisions()["questions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|q| q["id"].as_str() == Some("formats-in"))
        .cloned()
        .unwrap();
    assert_eq!(strs(&q["answer"]), vec!["glb", "obj"], "{q:?}");
}

#[test]
fn non_multi_domain_question_rejects_two_answers() {
    let p = project(&[]);
    let o = p.run(&[
        "decide",
        "add-question",
        "--id",
        "one",
        "--text",
        "Pick one",
        "--choice",
        "a",
        "--choice",
        "b",
    ]);
    expect_code(&o, 0, "add-question");
    let before = p.decisions_bytes();
    expect_failure(
        &record(&p, "domain", "one", &["a", "b"], None),
        "two answers, not multi",
    );
    assert_eq!(p.decisions_bytes(), before);
    // Positive control.
    expect_code(&record(&p, "domain", "one", &["a"], None), 0, "one answer");
}

#[test]
fn legacy_string_under_channel_reads_as_one_element() {
    let p = Project::new();
    let mut s = String::new();
    for a in AREAS.iter().filter(|a| **a != "notification") {
        s.push_str(&format!("[areas.{a}]\nrelevance = \"irrelevant\"\n\n"));
    }
    s.push_str("[areas.notification]\nrelevance = \"relevant\"\n\n[areas.notification.answers]\nchannel = \"email\"\nopt-out = \"yes\"\n");
    p.write_decisions(&s);
    let o = p.run(&["decide", "next"]);
    expect_code(&o, 0, "decide next");
    let v = o.json_objects().pop().expect("next JSON");
    assert_eq!(v["area"], "notification", "{v}");
    assert_eq!(
        v["question_id"], "email-sender",
        "legacy `email` activates email-sender: {v}"
    );
}

#[test]
fn array_under_a_non_multi_question_is_exit_2() {
    let p = Project::new();
    let mut base = String::new();
    for a in AREAS.iter().filter(|a| **a != "auth") {
        base.push_str(&format!("[areas.{a}]\nrelevance = \"irrelevant\"\n\n"));
    }
    p.write_decisions(&format!(
        "{base}[areas.auth]\nrelevance = \"relevant\"\n\n[areas.auth.answers]\nneeds-login = [\"no\"]\n"
    ));
    expect_code(
        &p.run(&["decide", "check"]),
        2,
        "decide check, array under needs-login",
    );
    expect_code(
        &p.run(&["decide", "next"]),
        2,
        "decide next, array under needs-login",
    );
    // Positive control: the string form is readable.
    p.write_decisions(&format!(
        "{base}[areas.auth]\nrelevance = \"relevant\"\n\n[areas.auth.answers]\nneeds-login = \"no\"\n"
    ));
    expect_code(
        &p.run(&["decide", "next"]),
        0,
        "decide next, string needs-login",
    );
}

#[test]
fn any_of_channel_activates_email_sender() {
    let p = project(&["notification"]);
    expect_code(
        &record(&p, "notification", "channel", &["web-push", "email"], None),
        0,
        "channel = [web-push, email]",
    );
    let o = p.run(&["decide", "next"]);
    let v = o.json_objects().pop().expect("next JSON");
    assert_eq!(v["area"], "notification", "{v}");
    assert_eq!(v["question_id"], "email-sender", "{v}");
}

#[test]
fn notes_are_stored_and_kept() {
    let p = project(&["auth"]);
    let o = record(
        &p,
        "auth",
        "needs-login",
        &["yes"],
        Some("only staff log in"),
    );
    expect_code(&o, 0, "record with --note");
    let d = p.decisions();
    assert_eq!(
        d["areas"]["auth"]["notes"]["needs-login"].as_str(),
        Some("only staff log in"),
        "{d:?}"
    );
    expect_code(
        &record(&p, "auth", "needs-login", &["yes"], None),
        0,
        "re-record without --note",
    );
    assert_eq!(
        p.decisions()["areas"]["auth"]["notes"]["needs-login"].as_str(),
        Some("only staff log in"),
        "a re-record without --note keeps the note"
    );

    let o = p.run(&[
        "decide",
        "add-question",
        "--id",
        "dq",
        "--text",
        "Free question",
    ]);
    expect_code(&o, 0, "add-question");
    expect_code(
        &record(&p, "domain", "dq", &["an answer"], Some("domain note")),
        0,
        "domain note",
    );
    expect_code(
        &record(&p, "domain", "dq", &["an answer"], None),
        0,
        "domain re-record",
    );
    let q = p.decisions()["questions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|q| q["id"].as_str() == Some("dq"))
        .cloned()
        .unwrap();
    assert_eq!(q["note"].as_str(), Some("domain note"), "{q:?}");
}

fn is_utc_rfc3339(s: &str) -> bool {
    regex::Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|\+00:00)$")
        .unwrap()
        .is_match(s)
}

#[test]
fn changed_answer_appends_history_and_prints_one_changed_line() {
    let p = project(&["auth"]);
    expect_code(
        &record(&p, "auth", "needs-login", &["yes"], None),
        0,
        "first answer",
    );
    let before = p.history().len();
    let o = record(&p, "auth", "needs-login", &["no"], None);
    expect_code(&o, 0, "changed answer");
    assert_eq!(
        changed_lines(&o),
        vec!["changed: yes -> no".to_string()],
        "{}",
        o.describe()
    );
    let h = p.history();
    assert_eq!(h.len(), before + 1, "{h:?}");
    let last = h.last().unwrap();
    assert_eq!(last["area"].as_str(), Some("auth"));
    assert_eq!(last["question"].as_str(), Some("needs-login"));
    assert_eq!(last["old"].as_str(), Some("yes"));
    assert_eq!(last["new"].as_str(), Some("no"));
    let at = last["at"].as_str().unwrap_or("");
    assert!(is_utc_rfc3339(at), "`at` must be UTC RFC 3339: {at:?}");
}

#[test]
fn same_value_rerecord_adds_no_history_and_no_changed_line() {
    let p = project(&["auth"]);
    expect_code(
        &record(&p, "auth", "needs-login", &["no"], None),
        0,
        "first answer",
    );
    let before = p.history().len();
    let o = record(&p, "auth", "needs-login", &["no"], None);
    expect_code(&o, 0, "same answer again");
    assert!(changed_lines(&o).is_empty(), "{}", o.describe());
    assert_eq!(p.history().len(), before);
    // Positive control: a different value does add a row.
    expect_code(
        &record(&p, "auth", "needs-login", &["yes"], None),
        0,
        "different answer",
    );
    assert_eq!(
        p.history().len(),
        before + 1,
        "a changed value must add a row"
    );
}

#[test]
fn relevance_and_note_changes_are_history_rows() {
    let p = project(&["auth"]);
    expect_code(
        &record(&p, "auth", "needs-login", &["no"], Some("first note")),
        0,
        "with note",
    );
    let before = p.history().len();
    let o = record(&p, "auth", "needs-login", &["no"], Some("second note"));
    expect_code(&o, 0, "note changed");
    assert_eq!(
        changed_lines(&o),
        vec!["changed: first note -> second note".to_string()],
        "{}",
        o.describe()
    );
    let h = p.history();
    assert_eq!(h.len(), before + 1, "{h:?}");
    assert_eq!(
        h.last().unwrap()["question"].as_str(),
        Some("needs-login#note")
    );

    let before = p.history().len();
    let o = record(&p, "auth", "relevance", &["irrelevant"], None);
    expect_code(&o, 0, "relevance changed");
    let h = p.history();
    assert!(h.len() > before, "{h:?}");
    assert!(
        h[before..]
            .iter()
            .any(|r| r["area"].as_str() == Some("auth")
                && r["question"].as_str() == Some("relevance")
                && r["old"].as_str() == Some("relevant")
                && r["new"].as_str() == Some("irrelevant")),
        "a relevance change must be a history row: {h:?}"
    );
}

#[test]
fn pruned_follow_up_is_kept_in_history() {
    let p = project(&["auth"]);
    expect_code(
        &record(&p, "auth", "needs-login", &["yes"], None),
        0,
        "needs-login yes",
    );
    expect_code(
        &record(&p, "auth", "auth-method", &["passkey"], None),
        0,
        "auth-method",
    );
    let before = p.history().len();
    expect_code(
        &record(&p, "auth", "needs-login", &["no"], None),
        0,
        "needs-login no",
    );
    let h = p.history();
    assert!(
        h[before..]
            .iter()
            .any(|r| r["area"].as_str() == Some("auth")
                && r["question"].as_str() == Some("auth-method")
                && r["old"].as_str() == Some("passkey")
                && r["new"].as_str() == Some("")
                && r["pruned"].as_bool() == Some(true)),
        "the pruned auth-method must be a `pruned = true` row: {h:?}"
    );
}
