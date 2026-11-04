//! Ruling U7 (docs/specs/sitecraft-capabilities-rulings.md):
//!
//! 1. the R7 rule evaluator (`sitecraft::decide::rules::evaluate_with`)
//!    applies any-of to a multi answer: an answer term over a multi answer is
//!    true iff some element is in the term's set; a single (non-multi) value
//!    behaves as before;
//! 2. every option of every select / multi catalog question is processable
//!    through the CLI: `decide record` records it (exit 0), `decide check` on
//!    the result is decidable (not exit 2), for a multi question all options
//!    together too, and every follow-up gated on an option becomes active
//!    (is asked by `decide next`) once that option is recorded.
//!
//! The catalog is read from the source (`sitecraft::decide::catalog::areas`),
//! never hardcoded here, so a catalog change is covered without editing this
//! file.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use serde_json::Value;
use sitecraft::decide::catalog::{areas, Area, Question};
use sitecraft::decide::record::{AnswerValue, Decisions};
use sitecraft::decide::rules::{evaluate_with, Fired, Rule, Severity, Term};
use sitecraft::Determination;
use tempfile::TempDir;

// ------------------------------------------------------------------ catalog

fn catalog() -> Vec<Area> {
    match areas() {
        Determination::Known(a) => a,
        Determination::Undetermined(why) => {
            panic!("catalog::areas() is Undetermined: {}", why.as_str())
        }
    }
}

fn question<'a>(cat: &'a [Area], area: &str, qid: &str) -> &'a Question {
    cat.iter()
        .find(|a| a.id == area)
        .unwrap_or_else(|| panic!("catalog has no area `{area}`"))
        .questions
        .iter()
        .find(|q| q.id == qid)
        .unwrap_or_else(|| panic!("catalog area `{area}` has no question `{qid}`"))
}

// ------------------------------------------------------------- 1. evaluator

const MULTI_AREA: &str = "notification";
const MULTI_Q: &str = "channel";
const ONE_AREA: &str = "exposure";
const ONE_Q: &str = "hosting";

/// Synthetic rule: fires iff `notification/channel` contains `email`.
const MULTI_RULE: [Rule; 1] = [Rule {
    id: "U7-MULTI",
    severity: Severity::Violation,
    text: "synthetic: channel includes email",
    when: &[&[Term::AnswerIn(MULTI_AREA, MULTI_Q, &["email"])]],
}];

/// Synthetic rule over a single-valued question.
const ONE_RULE: [Rule; 1] = [Rule {
    id: "U7-ONE",
    severity: Severity::Violation,
    text: "synthetic: hosting is static or server",
    when: &[&[Term::AnswerIn(ONE_AREA, ONE_Q, &["static", "server"])]],
}];

/// The test's premise, read from the catalog: `channel` is multi and its
/// choices hold the values used below; `hosting` is not multi.
fn assert_premise() {
    let cat = catalog();
    let m = question(&cat, MULTI_AREA, MULTI_Q);
    assert!(m.multi, "{MULTI_AREA}/{MULTI_Q} must be multi: {m:?}");
    for c in ["email", "web-push", "in-app", "chat"] {
        assert!(m.choices.iter().any(|x| x == c), "{c} not a choice: {m:?}");
    }
    let o = question(&cat, ONE_AREA, ONE_Q);
    assert!(!o.multi, "{ONE_AREA}/{ONE_Q} must not be multi: {o:?}");
    for c in ["static", "server", "serverless"] {
        assert!(o.choices.iter().any(|x| x == c), "{c} not a choice: {o:?}");
    }
}

fn multi_decisions(elements: &[&str]) -> Decisions {
    let arr: Vec<String> = elements.iter().map(|e| format!("{e:?}")).collect();
    let body = format!(
        "[areas.{MULTI_AREA}]\nrelevance = \"relevant\"\n[areas.{MULTI_AREA}.answers]\n{MULTI_Q} = [{}]\n",
        arr.join(", ")
    );
    let d: Decisions = toml::from_str(&body).unwrap_or_else(|e| panic!("{e}\n{body}"));
    // The evaluator must be fed a Many value, or this would not test any-of.
    match &d.areas[MULTI_AREA].answers[MULTI_Q] {
        AnswerValue::Many(xs) => assert_eq!(xs.len(), elements.len()),
        other => panic!("expected a multi (array) answer, got {other:?}"),
    }
    d
}

fn one_decisions(value: &str) -> Decisions {
    let body = format!(
        "[areas.{ONE_AREA}]\nrelevance = \"relevant\"\n[areas.{ONE_AREA}.answers]\n{ONE_Q} = {value:?}\n"
    );
    let d: Decisions = toml::from_str(&body).unwrap_or_else(|e| panic!("{e}\n{body}"));
    match &d.areas[ONE_AREA].answers[ONE_Q] {
        AnswerValue::One(s) => assert_eq!(s, value),
        other => panic!("expected a single answer, got {other:?}"),
    }
    d
}

fn ids(f: &[Fired]) -> Vec<&'static str> {
    f.iter().map(|x| x.id).collect()
}

#[test]
fn multi_answer_fires_when_any_element_is_in_the_set() {
    assert_premise();
    for elements in [
        &["email"][..],
        &["email", "web-push"][..],
        &["web-push", "email"][..],
        &["web-push", "chat", "email"][..],
    ] {
        let fired = evaluate_with(&MULTI_RULE, &multi_decisions(elements));
        assert_eq!(
            ids(&fired),
            ["U7-MULTI"],
            "{elements:?} contains email: the rule must fire"
        );
        assert_eq!(
            fired[0].involved,
            vec![(
                MULTI_AREA.to_string(),
                MULTI_Q.to_string(),
                elements.join(", ")
            )],
            "involved must name the recorded multi answer"
        );
    }
}

#[test]
fn multi_answer_with_no_element_in_the_set_does_not_fire() {
    assert_premise();
    // Positive control with the same table: an element in the set fires.
    assert_eq!(
        ids(&evaluate_with(
            &MULTI_RULE,
            &multi_decisions(&["web-push", "email"])
        )),
        ["U7-MULTI"],
        "control: web-push+email must fire"
    );
    for elements in [
        &["web-push"][..],
        &["web-push", "in-app"][..],
        &["web-push", "in-app", "chat"][..],
    ] {
        assert!(
            evaluate_with(&MULTI_RULE, &multi_decisions(elements)).is_empty(),
            "{elements:?} has no element in [email]: the rule must not fire"
        );
    }
}

#[test]
fn single_value_answer_behaves_as_before() {
    assert_premise();
    for v in ["static", "server"] {
        let fired = evaluate_with(&ONE_RULE, &one_decisions(v));
        assert_eq!(ids(&fired), ["U7-ONE"], "hosting = {v} is in the set");
        assert_eq!(
            fired[0].involved,
            vec![(ONE_AREA.to_string(), ONE_Q.to_string(), v.to_string())]
        );
    }
    assert!(
        evaluate_with(&ONE_RULE, &one_decisions("serverless")).is_empty(),
        "hosting = serverless is not in the set"
    );
    // A multi rule does not see a single-valued question and vice versa.
    assert!(evaluate_with(&MULTI_RULE, &one_decisions("static")).is_empty());
    assert!(evaluate_with(&ONE_RULE, &multi_decisions(&["email"])).is_empty());
}

// ----------------------------------------------------------- 2. CLI options

const PURPOSE_TOML: &str = "[purpose]\ntext = \"u7 options test purpose\"\n\n";
const MAX_STEPS: usize = 200;

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run(root: &Path, args: &[&str]) -> Out {
    let o = Command::new(env!("CARGO_BIN_EXE_sitecraft"))
        .current_dir(root)
        .arg("--root")
        .arg(root)
        .arg("decide")
        .args(args)
        .env("SITECRAFT_STATE_DIR", root.join(".state"))
        .env_remove("SITECRAFT_MARKETPLACE")
        .output()
        .expect("spawn sitecraft");
    Out {
        code: o.status.code().expect("sitecraft killed by a signal"),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}

fn expect_code(o: &Out, want: i32, what: &str) {
    assert_eq!(
        o.code, want,
        "{what}: expected exit {want}, got {}\nstdout: {}\nstderr: {}",
        o.code, o.stdout, o.stderr
    );
}

fn record(root: &Path, area: &str, q: &str, answers: &[&str]) -> Out {
    let mut args = vec!["record", "--area", area, "--question", q];
    for a in answers {
        args.push("--answer");
        args.push(a);
    }
    run(root, &args)
}

/// A root with a purpose, every area but `area` judged irrelevant, and
/// `area` judged relevant.
fn root_with_relevant(cat: &[Area], area: &str) -> TempDir {
    let t = TempDir::new().unwrap();
    let mut body = PURPOSE_TOML.to_string();
    for a in cat {
        let rel = if a.id == area {
            "relevant"
        } else {
            "irrelevant"
        };
        body.push_str(&format!("[areas.{}]\nrelevance = \"{rel}\"\n\n", a.id));
    }
    std::fs::create_dir_all(t.path().join("sitecraft")).unwrap();
    std::fs::write(t.path().join("sitecraft/decisions.toml"), body).unwrap();
    t
}

/// Record the gate chain that makes `q` active (first gate value at each
/// level). Each step must record (exit 0).
fn activate(root: &Path, area: &Area, q: &Question) {
    if let Some(c) = &q.when {
        let gate = area
            .questions
            .iter()
            .find(|g| g.id == c.question_id)
            .unwrap_or_else(|| panic!("{}/{}: gate `{}` missing", area.id, q.id, c.question_id));
        activate(root, area, gate);
        let v = c
            .answer_in
            .first()
            .unwrap_or_else(|| panic!("{}/{}: empty gate set", area.id, q.id));
        expect_code(
            &record(root, &area.id, &gate.id, &[v]),
            0,
            &format!("activate {}/{}: record gate {}={v}", area.id, q.id, gate.id),
        );
    }
}

/// Answer `decide next` (first choice / free text) until done; returns every
/// `(area, question_id)` asked.
fn walk_to_done(root: &Path) -> Vec<(String, String)> {
    let mut asked = Vec::new();
    for _ in 0..MAX_STEPS {
        let o = run(root, &["next"]);
        expect_code(&o, 0, "decide next");
        let line = o
            .stdout
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or_else(|| panic!("no stdout; stderr: {}", o.stderr));
        let v: Value = serde_json::from_str(line).unwrap();
        match v["status"].as_str() {
            Some("done") => return asked,
            Some("ask") => {
                let area = v["area"].as_str().unwrap().to_string();
                let qid = v["question_id"].as_str().unwrap().to_string();
                let ans = v["choices"]
                    .as_array()
                    .unwrap()
                    .first()
                    .map_or("free text answer".to_string(), |c| {
                        c.as_str().unwrap().to_string()
                    });
                expect_code(
                    &record(root, &area, &qid, &[&ans]),
                    0,
                    &format!("walk: record {area}/{qid}={ans}"),
                );
                let key = (area, qid);
                assert!(!asked.contains(&key), "{key:?} asked twice");
                asked.push(key);
            }
            other => panic!("next status must be ask|done, got {other:?}: {v}"),
        }
    }
    panic!("decide next did not reach done within {MAX_STEPS} steps: {asked:?}");
}

fn recorded(root: &Path, area: &str, q: &str) -> toml::Value {
    let body = std::fs::read_to_string(root.join("sitecraft/decisions.toml")).unwrap();
    let v: toml::Value = toml::from_str(&body).unwrap();
    v["areas"][area]["answers"]
        .get(q)
        .cloned()
        .unwrap_or_else(|| panic!("{area}/{q} not recorded:\n{body}"))
}

/// Record `selection` for `area/q` in a fresh root (gate chain activated),
/// then assert: recorded as given, `check` decidable, and exactly the
/// follow-ups gated on `q` with a selected option are asked by `next`.
fn exercise(cat: &[Area], area: &Area, q: &Question, selection: &[&str]) {
    let what = format!("{}/{} = {selection:?}", area.id, q.id);
    let t = root_with_relevant(cat, &area.id);
    let root = t.path();
    activate(root, area, q);
    expect_code(&record(root, &area.id, &q.id, selection), 0, &what);

    let got = recorded(root, &area.id, &q.id);
    if q.multi {
        let want: Vec<toml::Value> = selection
            .iter()
            .map(|s| toml::Value::String((*s).to_string()))
            .collect();
        assert_eq!(got, toml::Value::Array(want), "{what}: stored value");
    } else {
        assert_eq!(got.as_str(), Some(selection[0]), "{what}: stored value");
    }

    let c = run(root, &["check"]);
    assert!(
        c.code == 0 || c.code == 1,
        "{what}: check must be decidable (0 or 1), got {}\nstdout: {}\nstderr: {}",
        c.code,
        c.stdout,
        c.stderr
    );

    let asked: BTreeSet<String> = walk_to_done(root)
        .into_iter()
        .filter(|(a, _)| *a == area.id)
        .map(|(_, q)| q)
        .collect();
    for f in &area.questions {
        let Some(cond) = &f.when else { continue };
        if cond.question_id != q.id {
            continue;
        }
        let gated_on = selection
            .iter()
            .any(|s| cond.answer_in.iter().any(|x| x == s));
        assert_eq!(
            asked.contains(&f.id),
            gated_on,
            "{what}: follow-up {}/{} (gated on {:?}) active={gated_on} expected; asked: {asked:?}",
            area.id,
            f.id,
            cond.answer_in
        );
    }
    // Fully answered: still decidable. Not asserted 0 — with the other areas
    // irrelevant a consistency rule (e.g. V3/V4 with auth irrelevant) may
    // legitimately fire (exit 1).
    let c = run(root, &["check"]);
    assert!(
        c.code == 0 || c.code == 1,
        "{what}: check after walk must be decidable (0 or 1), got {}\nstderr: {}",
        c.code,
        c.stderr
    );
}

#[test]
fn every_catalog_option_is_processable_through_the_cli() {
    let cat = catalog();
    assert!(!cat.is_empty(), "catalog must be non-empty");
    let selects: Vec<(&Area, &Question)> = cat
        .iter()
        .flat_map(|a| a.questions.iter().map(move |q| (a, q)))
        .filter(|(_, q)| !q.choices.is_empty())
        .collect();
    assert!(
        selects.iter().any(|(_, q)| !q.multi),
        "catalog must have a select question"
    );
    assert!(
        selects.iter().any(|(_, q)| q.multi),
        "catalog must have a multi question"
    );
    let mut exercised = 0usize;
    for (area, q) in &selects {
        for c in &q.choices {
            exercise(&cat, area, q, &[c.as_str()]);
            exercised += 1;
        }
        if q.multi {
            let all: Vec<&str> = q.choices.iter().map(String::as_str).collect();
            exercise(&cat, area, q, &all);
            exercised += 1;
        }
    }
    let options: usize = selects.iter().map(|(_, q)| q.choices.len()).sum();
    assert!(exercised > options, "every option plus each multi-all set");
}

/// From the follow-up side: every value a follow-up is gated on must be a
/// recordable option of its gate question, and recording it activates the
/// follow-up. (A gate value that is not an option would leave the follow-up
/// unreachable, which the option loop alone cannot see.)
#[test]
fn every_follow_up_gate_value_is_recordable_and_activates_it() {
    let cat = catalog();
    let mut follow_ups = 0usize;
    for area in &cat {
        for f in &area.questions {
            let Some(cond) = &f.when else { continue };
            follow_ups += 1;
            let gate = area
                .questions
                .iter()
                .find(|g| g.id == cond.question_id)
                .unwrap_or_else(|| panic!("{}/{}: gate missing", area.id, f.id));
            assert!(
                !cond.answer_in.is_empty(),
                "{}/{}: empty gate set",
                area.id,
                f.id
            );
            for v in &cond.answer_in {
                // Through the CLI first, so an unreachable gate value shows
                // up as the `decide record` failure it causes.
                exercise(&cat, area, gate, &[v.as_str()]);
                assert!(
                    gate.choices.iter().any(|c| c == v),
                    "{}/{} is gated on {}={v}, which is not an option {:?}",
                    area.id,
                    f.id,
                    gate.id,
                    gate.choices
                );
            }
        }
    }
    assert!(follow_ups > 0, "catalog must have follow-ups");
}
