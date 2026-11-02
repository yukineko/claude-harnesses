//! Pins the capability-area catalog (`sitecraft::decide::catalog`) and its
//! answer-conditional question activation.
//!
//! User rulings pinned here:
//! - the catalog holds exactly the 12 capability areas the user named, each
//!   with at least one representative question (questions are chosen per the
//!   purpose, so every area must be askable);
//! - follow-ups depend on answers (auth: `yes` brings the auth-method
//!   follow-up, `no` does not; formats: the follow-up depends on the answer);
//! - unknown areas are `Undetermined`, never an empty (= "nothing to ask") list.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;

use sitecraft::decide::catalog::{
    active_questions, areas, Answers, Area, Question, RELEVANCE_QUESTION,
};
use sitecraft::Determination;

const AREA_IDS: [&str; 12] = [
    "auth",
    "users-roles",
    "admin",
    "storage",
    "data-analytics",
    "web-analytics",
    "processing",
    "workflow",
    "notification",
    "ai",
    "formats",
    "exposure",
];

fn catalog() -> Vec<Area> {
    match areas() {
        Determination::Known(a) => a,
        Determination::Undetermined(why) => {
            panic!(
                "catalog::areas() must be Known, got Undetermined: {}",
                why.as_str()
            )
        }
    }
}

fn area(id: &str) -> Area {
    catalog()
        .into_iter()
        .find(|a| a.id == id)
        .unwrap_or_else(|| panic!("catalog has no area `{id}`"))
}

fn active(area_id: &str, answers: &Answers) -> Vec<Question> {
    match active_questions(area_id, answers) {
        Determination::Known(q) => q,
        Determination::Undetermined(why) => panic!(
            "active_questions({area_id}) must be Known, got Undetermined: {}",
            why.as_str()
        ),
    }
}

fn ids(qs: &[Question]) -> BTreeSet<String> {
    qs.iter().map(|q| q.id.clone()).collect()
}

fn representatives(a: &Area) -> Vec<&Question> {
    a.questions.iter().filter(|q| q.when.is_none()).collect()
}

fn answers(pairs: &[(&str, &str)]) -> Answers {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

#[test]
fn catalog_has_exactly_the_twelve_user_named_areas() {
    let got: Vec<String> = catalog().into_iter().map(|a| a.id).collect();
    let set: BTreeSet<&str> = got.iter().map(String::as_str).collect();
    assert_eq!(got.len(), 12, "exactly 12 areas, no duplicates: {got:?}");
    assert_eq!(set.len(), 12, "area ids must be unique: {got:?}");
    let want: BTreeSet<&str> = AREA_IDS.into_iter().collect();
    assert_eq!(set, want);
}

#[test]
fn every_area_has_a_titled_representative_question() {
    for a in catalog() {
        assert!(!a.title.trim().is_empty(), "area {} has no title", a.id);
        let reps = representatives(&a);
        assert!(
            !reps.is_empty(),
            "area {} needs at least one representative question (when == None)",
            a.id
        );
        for q in &a.questions {
            assert!(
                !q.text.trim().is_empty(),
                "{}/{} has empty text",
                a.id,
                q.id
            );
            assert_ne!(
                q.id, RELEVANCE_QUESTION,
                "{}: `relevance` is a pseudo question, not a catalog question",
                a.id
            );
        }
        let qids: BTreeSet<&str> = a.questions.iter().map(|q| q.id.as_str()).collect();
        assert_eq!(
            qids.len(),
            a.questions.len(),
            "{}: duplicate question ids",
            a.id
        );
    }
}

#[test]
fn follow_up_conditions_refer_to_a_real_answer_in_the_same_area() {
    for a in catalog() {
        for q in &a.questions {
            let Some(cond) = &q.when else { continue };
            let gate = a
                .questions
                .iter()
                .find(|g| g.id == cond.question_id)
                .unwrap_or_else(|| {
                    panic!(
                        "{}/{} is gated on `{}`, which is not a question of the area",
                        a.id, q.id, cond.question_id
                    )
                });
            assert_ne!(gate.id, q.id, "{}/{} gates itself", a.id, q.id);
            assert!(
                !cond.answer_in.is_empty(),
                "{}/{}: empty answer_in can never activate",
                a.id,
                q.id
            );
            if !gate.choices.is_empty() {
                for ans in &cond.answer_in {
                    assert!(
                        gate.choices.contains(ans),
                        "{}/{} activates on `{ans}`, not a choice of {}: {:?}",
                        a.id,
                        q.id,
                        gate.id,
                        gate.choices
                    );
                }
            }
        }
    }
}

#[test]
fn with_no_answers_every_area_activates_exactly_its_representatives() {
    for a in catalog() {
        let got = active(&a.id, &Answers::new());
        assert!(!got.is_empty(), "area {} activated nothing", a.id);
        let reps: BTreeSet<String> = representatives(&a).iter().map(|q| q.id.clone()).collect();
        assert_eq!(ids(&got), reps, "area {}", a.id);
        assert!(got.iter().all(|q| q.id != RELEVANCE_QUESTION));
    }
}

/// The auth representative yes/no question (the one gating a follow-up).
fn auth_gate() -> (Area, Question) {
    let a = area("auth");
    let gate = representatives(&a)
        .into_iter()
        .find(|q| {
            q.choices.iter().any(|c| c == "yes")
                && q.choices.iter().any(|c| c == "no")
                && a.questions.iter().any(|f| {
                    f.when.as_ref().is_some_and(|w| {
                        w.question_id == q.id && w.answer_in.iter().any(|v| v == "yes")
                    })
                })
        })
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "auth needs a representative yes/no question with a follow-up on `yes`: {:?}",
                a.questions
            )
        });
    (a, gate)
}

#[test]
fn auth_yes_activates_the_auth_method_follow_up_and_no_does_not() {
    let (_, gate) = auth_gate();
    let base = ids(&active("auth", &Answers::new()));

    let yes = active("auth", &answers(&[(gate.id.as_str(), "yes")]));
    let yes_ids = ids(&yes);
    let added: Vec<&Question> = yes.iter().filter(|q| !base.contains(&q.id)).collect();
    assert!(
        !added.is_empty(),
        "answering {} = yes must add a follow-up (auth method); got {yes_ids:?}",
        gate.id
    );
    for f in &added {
        let w = f.when.as_ref().expect("added question must be a follow-up");
        assert_eq!(w.question_id, gate.id);
        assert!(
            !f.choices.is_empty(),
            "the auth-method follow-up should offer concrete methods as choices"
        );
    }

    let no = ids(&active("auth", &answers(&[(gate.id.as_str(), "no")])));
    assert_eq!(
        no, base,
        "answering no must not add the auth-method follow-up"
    );
    assert!(yes_ids.is_superset(&no));
}

#[test]
fn formats_follow_up_depends_on_the_answer() {
    let a = area("formats");
    let follow_ups: Vec<&Question> = a.questions.iter().filter(|q| q.when.is_some()).collect();
    assert!(
        !follow_ups.is_empty(),
        "formats must have an answer-conditional follow-up: {:?}",
        a.questions
    );
    // Some gating question has two answers yielding different active sets.
    let mut differs = false;
    for f in &follow_ups {
        let w = f.when.as_ref().unwrap();
        let gate = a.questions.iter().find(|q| q.id == w.question_id).unwrap();
        let on = w.answer_in[0].clone();
        let with_on = ids(&active("formats", &answers(&[(gate.id.as_str(), &on)])));
        assert!(
            with_on.contains(&f.id),
            "answering {}={on} must activate {}",
            gate.id,
            f.id
        );
        let off = gate.choices.iter().find(|c| !w.answer_in.contains(c));
        if let Some(off) = off {
            let with_off = ids(&active("formats", &answers(&[(gate.id.as_str(), off)])));
            assert!(
                !with_off.contains(&f.id),
                "answering {}={off} must not activate {}",
                gate.id,
                f.id
            );
            differs = true;
        }
    }
    assert!(
        differs,
        "formats follow-up must be off for at least one answer"
    );
}

#[test]
fn follow_up_inactive_until_its_gate_is_answered() {
    for a in catalog() {
        let got = ids(&active(&a.id, &Answers::new()));
        for q in a.questions.iter().filter(|q| q.when.is_some()) {
            assert!(
                !got.contains(&q.id),
                "{}/{} active without an answer",
                a.id,
                q.id
            );
        }
    }
}

#[test]
fn unknown_area_is_undetermined_not_empty() {
    assert!(
        !AREA_IDS.contains(&"payments-xyz"),
        "control: the probe id must not be a catalog area"
    );
    // Control: a known area is Known (so the Undetermined below is about the id).
    let _ = active("auth", &Answers::new());
    assert!(matches!(
        active_questions("payments-xyz", &Answers::new()),
        Determination::Undetermined(_)
    ));
    assert!(matches!(
        active_questions("domain", &Answers::new()),
        Determination::Undetermined(_)
    ));
}
