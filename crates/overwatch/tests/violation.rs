//! Integration tests for fleet-level correlated-error detection: recording
//! gate-violation events to the project-wide store, then aggregating them
//! into recurrence/escalation reports.
use overwatch::violation::{self, RawViolation, RecurrencePolicy, ViolationEvent, ViolationSource};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn make_test_dir(tag: &str) -> PathBuf {
    let n = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "overwatch-viol-test-{tag}-{}-{n}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("failed to create test dir");
    dir
}

/// Simulates persisting violation events across multiple "tasks"/sessions to
/// a shared project-wide store, then reading them back and detecting a
/// systemic recurrence — end to end through the store's jsonl format.
#[test]
fn violations_persist_and_round_trip_through_store() {
    let temp_dir = make_test_dir("roundtrip");
    let storage_dir = temp_dir.join("overwatch-data");
    fs::create_dir_all(&storage_dir).unwrap();
    let violations_path = storage_dir.join("violations.jsonl");

    // Three different tasks/sessions each hit the SAME blastguard rule.
    let events = [
        violation::build_event(
            ViolationSource::Blastguard,
            &RawViolation {
                rule_id: Some("rm-rf"),
                ..Default::default()
            },
            "task-a".to_string(),
            "session-1".to_string(),
            1_000,
            Some("denied: rm -rf /tmp/x".to_string()),
        ),
        violation::build_event(
            ViolationSource::Blastguard,
            &RawViolation {
                rule_id: Some("RM-RF"), // different case, same signature
                ..Default::default()
            },
            "task-b".to_string(),
            "session-2".to_string(),
            2_000,
            None,
        ),
        violation::build_event(
            ViolationSource::Blastguard,
            &RawViolation {
                rule_id: Some("rm-rf"),
                ..Default::default()
            },
            "task-c".to_string(),
            "session-3".to_string(),
            3_000,
            None,
        ),
        // A one-off propguard failure that should NOT be flagged systemic.
        violation::build_event(
            ViolationSource::Propguard,
            &RawViolation {
                property_id: Some("PROP-007"),
                ..Default::default()
            },
            "task-a".to_string(),
            "session-1".to_string(),
            1_500,
            None,
        ),
    ];

    // Append each event as a JSON line (mirrors store::append_violation).
    let mut contents = String::new();
    for ev in &events {
        contents.push_str(&serde_json::to_string(ev).unwrap());
        contents.push('\n');
    }
    fs::write(&violations_path, contents).unwrap();

    // Read back (mirrors store::read_violations).
    let text = fs::read_to_string(&violations_path).unwrap();
    let loaded: Vec<ViolationEvent> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(loaded.len(), 4);

    // All three blastguard events normalize to the same signature despite
    // the case difference in the raw rule id.
    let sigs: Vec<&str> = loaded
        .iter()
        .filter(|e| e.source == ViolationSource::Blastguard)
        .map(|e| e.signature.as_str())
        .collect();
    assert_eq!(sigs, vec!["blastguard:rm-rf"; 3]);

    // Detect recurrence at "now" = 4000, with a policy requiring 3+ occurrences
    // in a 24h window.
    let policy = RecurrencePolicy {
        threshold: 3,
        window_secs: 86_400,
    };
    let report = violation::detect_recurrence(&loaded, 4_000, policy);

    let rm_rf = report
        .iter()
        .find(|r| r.signature == "blastguard:rm-rf")
        .expect("rm-rf signature present");
    assert_eq!(rm_rf.occurrences, 3);
    assert_eq!(rm_rf.distinct_tasks, 3);
    assert_eq!(rm_rf.distinct_sessions, 3);
    assert!(
        rm_rf.is_systemic,
        "3 occurrences across 3 tasks is systemic"
    );

    let prop = report
        .iter()
        .find(|r| r.signature == "propguard:prop-007")
        .expect("propguard signature present");
    assert_eq!(prop.occurrences, 1);
    assert!(!prop.is_systemic, "single occurrence is not systemic");

    let systemic = violation::systemic_issues(&loaded, 4_000, policy);
    assert_eq!(systemic.len(), 1);
    assert_eq!(systemic[0].signature, "blastguard:rm-rf");
}

/// A recurrence just outside the configured window must NOT be counted,
/// proving the window is respected and time is fully injectable (no
/// wall-clock reads) — deterministic and reproducible across runs.
#[test]
fn recurrence_window_excludes_stale_events_deterministically() {
    let events = vec![
        violation::build_event(
            ViolationSource::Mutategate,
            &RawViolation {
                mutation_operator: Some("arithmetic-op-swap"),
                ..Default::default()
            },
            "task-x".to_string(),
            "session-x".to_string(),
            0, // far in the past
            None,
        ),
        violation::build_event(
            ViolationSource::Mutategate,
            &RawViolation {
                mutation_operator: Some("arithmetic-op-swap"),
                ..Default::default()
            },
            "task-y".to_string(),
            "session-y".to_string(),
            9_950,
            None,
        ),
        violation::build_event(
            ViolationSource::Mutategate,
            &RawViolation {
                mutation_operator: Some("arithmetic-op-swap"),
                ..Default::default()
            },
            "task-z".to_string(),
            "session-z".to_string(),
            9_990,
            None,
        ),
    ];

    let policy = RecurrencePolicy {
        threshold: 3,
        window_secs: 100, // only the last ~100s count
    };

    // now = 10_000: event at ts=0 is far outside window; ts=9950,9990 are inside.
    let report = violation::detect_recurrence(&events, 10_000, policy);
    let sig = &report[0];
    assert_eq!(sig.occurrences, 2, "only 2 of 3 events fall in the window");
    assert!(
        !sig.is_systemic,
        "2 occurrences below threshold=3 must not escalate"
    );

    // Calling again with the same inputs must yield the identical result
    // (pure/deterministic — no hidden clock reads).
    let report2 = violation::detect_recurrence(&events, 10_000, policy);
    assert_eq!(report, report2);
}

/// Specguard drift findings normalize by (kind, symbol) so that the SAME
/// drift on the SAME symbol recurs as one signature, while drift on a
/// DIFFERENT symbol is tracked separately.
#[test]
fn specguard_signatures_distinguish_by_symbol() {
    let events = vec![
        violation::build_event(
            ViolationSource::Specguard,
            &RawViolation {
                drift_kind: Some("spec-without-impl"),
                symbol: Some("crate::foo::bar"),
                ..Default::default()
            },
            "task-1".to_string(),
            "session-1".to_string(),
            100,
            None,
        ),
        violation::build_event(
            ViolationSource::Specguard,
            &RawViolation {
                drift_kind: Some("spec-without-impl"),
                symbol: Some("crate::baz::qux"),
                ..Default::default()
            },
            "task-2".to_string(),
            "session-2".to_string(),
            200,
            None,
        ),
    ];

    assert_ne!(events[0].signature, events[1].signature);

    let policy = RecurrencePolicy {
        threshold: 2,
        window_secs: 10_000,
    };
    let report = violation::detect_recurrence(&events, 1_000, policy);
    assert_eq!(report.len(), 2);
    assert!(
        report.iter().all(|r| !r.is_systemic),
        "each symbol only has 1 occurrence, below threshold=2"
    );
}
