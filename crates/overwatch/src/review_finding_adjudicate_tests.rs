// IMPLEMENTER-WRITTEN (backlog 80a46e9f). These unit tests were written by the
// agent that implemented `adjudicate`, so they are NOT the independent oracle
// for this change — `tests/refuted_witness.rs` (committed separately, d23f52be)
// is. Each test here was observed failing against a named mutation of the
// production code before being accepted (see the per-test `mutation:` notes).
//
// Full table covered: asserted {Confirmed, Refuted, Unverified} x probe
// {None, Undetermined, NotReproduced, Reproduced} x signoff {Absent, By}.

use super::*;

fn undetermined() -> Option<Determination<ProbeOutcome>> {
    Some(parse_probe("not json"))
}
fn not_reproduced() -> Option<Determination<ProbeOutcome>> {
    Some(Determination::known(ProbeOutcome::NotReproduced))
}
fn reproduced() -> Option<Determination<ProbeOutcome>> {
    Some(Determination::known(ProbeOutcome::Reproduced))
}
fn probes() -> Vec<(&'static str, Option<Determination<ProbeOutcome>>)> {
    vec![
        ("none", None),
        ("undetermined", undetermined()),
        ("not_reproduced", not_reproduced()),
        ("reproduced", reproduced()),
    ]
}
fn signoffs() -> Vec<SignOff> {
    vec![SignOff::Absent, SignOff::By("yuki".to_string())]
}

/// mutation: `AuditVerdict::Refuted => as_asserted(AuditVerdict::Refuted)`
/// (return the LLM verdict unchanged) — this test goes RED.
#[test]
fn refuted_without_a_witness_never_survives_adjudication() {
    let witnessless = [
        (None, SignOff::Absent),
        (None, SignOff::By("yuki".to_string())),
        (undetermined(), SignOff::Absent),
        (undetermined(), SignOff::By("yuki".to_string())),
        (not_reproduced(), SignOff::Absent),
    ];
    for (probe, signoff) in witnessless {
        let label = format!("{probe:?} / {signoff:?}");
        let a = adjudicate(AuditVerdict::Refuted, probe, &signoff);
        assert_eq!(a.stored, AuditVerdict::Unverified, "{label}");
        assert!(a.note.is_some(), "a demotion must carry a note: {label}");
    }
}

/// mutation: `SignOff::By(_) => as_asserted(AuditVerdict::Refuted)` changed to
/// `demote(..)` — this control goes RED (guards against over-blocking).
#[test]
fn refuted_with_not_reproduced_probe_and_signoff_stands() {
    let a = adjudicate(
        AuditVerdict::Refuted,
        not_reproduced(),
        &SignOff::By("yuki".to_string()),
    );
    assert_eq!(
        a,
        Adjudication {
            stored: AuditVerdict::Refuted,
            note: None
        }
    );
}

/// mutation: the `Reproduced` arm storing `Unverified` — RED.
#[test]
fn reproduced_probe_overturns_refuted_to_confirmed_with_or_without_signoff() {
    for signoff in signoffs() {
        let a = adjudicate(AuditVerdict::Refuted, reproduced(), &signoff);
        assert_eq!(a.stored, AuditVerdict::Confirmed, "{signoff:?}");
        assert!(
            a.note.as_deref().is_some_and(|n| n.contains("CONFIRMED")),
            "{a:?}"
        );
    }
}

/// mutation: `AuditVerdict::Confirmed => as_asserted(AuditVerdict::Unverified)`
/// — RED.
#[test]
fn confirmed_is_stored_as_asserted_whatever_the_witness() {
    for (name, probe) in probes() {
        for signoff in signoffs() {
            let a = adjudicate(AuditVerdict::Confirmed, probe.clone(), &signoff);
            assert_eq!(
                a,
                Adjudication {
                    stored: AuditVerdict::Confirmed,
                    note: None
                },
                "{name} / {signoff:?}"
            );
        }
    }
}

/// mutation: `AuditVerdict::Unverified => as_asserted(AuditVerdict::Refuted)`
/// — RED. (A witness does not promote an Unverified claim to Refuted.)
#[test]
fn unverified_is_stored_as_asserted_whatever_the_witness() {
    for (name, probe) in probes() {
        for signoff in signoffs() {
            let a = adjudicate(AuditVerdict::Unverified, probe.clone(), &signoff);
            assert_eq!(
                a,
                Adjudication {
                    stored: AuditVerdict::Unverified,
                    note: None
                },
                "{name} / {signoff:?}"
            );
        }
    }
}

/// mutation: `demote` ignoring `missing` (fixed text without the missing
/// piece) — RED.
#[test]
fn demotion_note_names_exactly_the_missing_pieces() {
    let note = |p, s: SignOff| {
        adjudicate(AuditVerdict::Refuted, p, &s)
            .note
            .expect("demotion note")
    };
    let n = note(None, SignOff::Absent);
    assert!(
        n.contains("missing: probe result (--probe); human sign-off"),
        "{n}"
    );

    let n = note(None, SignOff::By("yuki".to_string()));
    assert!(n.contains("missing: probe result (--probe)"), "{n}");
    assert!(!n.contains("missing: probe result (--probe); human"), "{n}");

    let n = note(not_reproduced(), SignOff::Absent);
    assert!(
        n.contains("missing: human sign-off (--signed-off-by)"),
        "{n}"
    );

    let n = note(undetermined(), SignOff::By("yuki".to_string()));
    assert!(n.contains("undetermined"), "{n}");
    assert!(!n.ends_with("(--signed-off-by)"), "{n}");
}

/// mutation: the `Some(other)` arm of `parse_probe` returning
/// `NotReproduced` — RED.
#[test]
fn only_the_two_exact_probe_results_parse_as_known() {
    assert_eq!(
        parse_probe(r#"{"result":"not_reproduced"}"#),
        Determination::known(ProbeOutcome::NotReproduced)
    );
    assert_eq!(
        parse_probe(r#"{"result":"reproduced"}"#),
        Determination::known(ProbeOutcome::Reproduced)
    );
    for bad in [
        "",
        "   ",
        "this is not json",
        "{}",
        r#"{"result":null}"#,
        r#"{"result":1}"#,
        r#"{"result":"probably_fine"}"#,
        r#"{"result":"NOT_REPRODUCED"}"#,
        r#""not_reproduced""#,
    ] {
        assert!(
            matches!(parse_probe(bad), Determination::Undetermined(_)),
            "{bad:?} must be undetermined"
        );
    }
}

/// mutation: `read_probe`'s `Determined(None)` (file does not exist) arm
/// returning `known(NotReproduced)` — RED.
#[test]
fn missing_probe_file_is_undetermined() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let missing = dir.path().join("nope.json");
    assert!(matches!(
        read_probe(&missing),
        Determination::Undetermined(_)
    ));
    let ok = dir.path().join("ok.json");
    std::fs::write(&ok, r#"{"result":"not_reproduced"}"#).expect("write");
    assert_eq!(
        read_probe(&ok),
        Determination::known(ProbeOutcome::NotReproduced)
    );
}

/// mutation: `SignOff::from_flag` treating any `Some(_)` as `By` — RED.
#[test]
fn blank_signoff_is_absent() {
    assert_eq!(SignOff::from_flag(None), SignOff::Absent);
    assert_eq!(SignOff::from_flag(Some("")), SignOff::Absent);
    assert_eq!(SignOff::from_flag(Some("  \t")), SignOff::Absent);
    assert_eq!(
        SignOff::from_flag(Some(" yuki ")),
        SignOff::By("yuki".to_string())
    );
}
