// Whole-file integration test: expect/unwrap/panic are fine here (the
// workspace clippy denies are for production code).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED(ask-learning): contract of the pure `harness_core::asklearn` module.
//!
//! Spec: docs/specs/blastguard-ask-learning.md (user ruling 2026-11-28),
//! component 1. Written by an agent that does NOT implement the module
//! (CLAUDE.md §2(a)).
//!
//! THIS FILE DOES NOT COMPILE UNTIL `harness_core::asklearn` EXISTS. The
//! expected RED is `error[E0432]: unresolved import harness_core::asklearn`.
//! Any OTHER compile error after the module lands means the implementation
//! chose a different surface than the one pinned below — read the list, and
//! if the spec does not decide the point, ask rather than edit the test.
//!
//! Rust surface pinned here (names from the spec where it gives them):
//!   * `Shape { rule_id: String, target_class: TargetClass, verbs: Vec<String> }`
//!     and `Shape::key(&self) -> String` — the canonical key.
//!   * `TargetClass::{Worktree, Scratchpad, ProtectedConfig, System, Home,
//!     Undetermined, None}` and `TargetClass::as_str(&self) -> &str` (the
//!     class segment of the key). The spec does NOT fix the label spelling,
//!     so these tests pin only that labels are distinct and separator-free.
//!   * `OutcomeRecord { shape: Shape, session_id: String, outcome: Outcome,
//!     ts: u64, sample_command: String, system_harm: bool }`.
//!     `system_harm` is NOT in the spec's record list, but the spec's learn()
//!     rule reads "not system_harm", so the record has to carry it somewhere.
//!     This is the test author's placement; contest it if the implementation
//!     needs another one.
//!   * `Outcome::{Approved, Declined}`.
//!   * `learn(&[OutcomeRecord]) -> LearnedTable`, `LearnedTable: Serialize`.
//!   * `lookup(&LearnedTable, &Shape) -> Option<LearnedDecision>` with
//!     variants `LearnedDecision::Allow { .. }` / `Deny { .. }` (matched with
//!     `{ .. }`, so unit, tuple or struct variants all satisfy it).
//!   * `read_table(&Path) -> TableRead` with `TableRead::Absent`,
//!     `TableRead::Undetermined(..)`, `TableRead::Known(LearnedTable)`.
//!
//! What this file does NOT prove: that blastguard computes the same shape the
//! learner does, that the key is stable across analyser versions, or anything
//! about the on-disk table path. Those are pinned (black-box) in
//! crates/blastguard/tests/ask_learning_*.rs and
//! crates/overwatch/tests/ask_learn_*.rs.

use harness_core::asklearn::{
    learn, lookup, read_table, LearnedDecision, LearnedTable, Outcome, OutcomeRecord, Shape,
    TableRead, TargetClass,
};
use serde_json::Value;
use std::path::PathBuf;

const ALL_CLASSES: [TargetClass; 7] = [
    TargetClass::Worktree,
    TargetClass::Scratchpad,
    TargetClass::ProtectedConfig,
    TargetClass::System,
    TargetClass::Home,
    TargetClass::Undetermined,
    TargetClass::None,
];

fn shape(rule: &str, class: TargetClass, verbs: &[&str]) -> Shape {
    Shape {
        rule_id: rule.to_string(),
        target_class: class,
        verbs: verbs.iter().map(|v| v.to_string()).collect(),
    }
}

fn rm_shape() -> Shape {
    shape("rm-undetermined-target", TargetClass::Undetermined, &["rm"])
}

fn rec(s: &Shape, sid: &str, outcome: Outcome, ts: u64, cmd: &str) -> OutcomeRecord {
    OutcomeRecord {
        shape: s.clone(),
        session_id: sid.to_string(),
        outcome,
        ts,
        sample_command: cmd.to_string(),
        system_harm: false,
    }
}

fn approvals(s: &Shape, sessions: &[&str]) -> Vec<OutcomeRecord> {
    sessions
        .iter()
        .enumerate()
        .map(|(i, sid)| rec(s, sid, Outcome::Approved, 1000 + i as u64, &format!("rm \"$f{i}\"")))
        .collect()
}

fn table_json(t: &LearnedTable) -> Value {
    serde_json::to_value(t).expect("LearnedTable serializes")
}

fn entries(t: &LearnedTable) -> Vec<Value> {
    let v = table_json(t);
    assert_eq!(v["version"], 1, "table version must be 1: {v}");
    v["entries"]
        .as_array()
        .unwrap_or_else(|| panic!("entries must be an array: {v}"))
        .clone()
}

fn entry_for(t: &LearnedTable, s: &Shape) -> Option<Value> {
    let key = s.key();
    entries(t).into_iter().find(|e| e["shape"] == key.as_str())
}

const U1: &str = "0b8f2c3e-1111-4a4a-9c9c-000000000001";
const U2: &str = "0b8f2c3e-2222-4a4a-9c9c-000000000002";
const U3: &str = "0b8f2c3e-3333-4a4a-9c9c-000000000003";

// ------------------------------------------------------------ shape key

#[test]
#[ignore = "RED(ask-learning): Shape::key canonical form <rule_id>|<class>|<sorted,deduped verbs>"]
fn shape_key_is_rule_class_and_sorted_deduped_verbs() {
    let s = shape("some-rule", TargetClass::Home, &["tee", "echo", "tee", "cat"]);
    let class = TargetClass::Home.as_str().to_string();
    assert_eq!(s.key(), format!("some-rule|{class}|cat,echo,tee"));
    // Order of the input verbs must not matter.
    let t = shape("some-rule", TargetClass::Home, &["cat", "tee", "echo"]);
    assert_eq!(s.key(), t.key());
    // A single verb has no trailing comma.
    assert_eq!(
        shape("r", TargetClass::None, &["Write"]).key(),
        format!("r|{}|Write", TargetClass::None.as_str())
    );
}

#[test]
#[ignore = "RED(ask-learning): TargetClass labels are distinct and contain no key separators"]
fn class_labels_are_distinct_and_separator_free() {
    let labels: Vec<&str> = ALL_CLASSES.iter().map(|c| c.as_str()).collect();
    for l in &labels {
        assert!(!l.is_empty(), "empty class label in {labels:?}");
        assert!(
            !l.contains('|') && !l.contains(','),
            "class label {l:?} contains a key separator"
        );
    }
    let mut d = labels.clone();
    d.sort();
    d.dedup();
    assert_eq!(d.len(), labels.len(), "class labels collide: {labels:?}");
    // Different class or rule → different key.
    let a = shape("r", TargetClass::Worktree, &["rm"]).key();
    let b = shape("r", TargetClass::Scratchpad, &["rm"]).key();
    let c = shape("q", TargetClass::Worktree, &["rm"]).key();
    assert_ne!(a, b);
    assert_ne!(a, c);
}

// ------------------------------------------------------------ thresholds

#[test]
#[ignore = "RED(ask-learning): 2 approvals → no entry"]
fn two_approvals_give_no_entry() {
    let s = rm_shape();
    let t = learn(&approvals(&s, &[U1, U2]));
    assert!(entry_for(&t, &s).is_none(), "{}", table_json(&t));
    assert!(lookup(&t, &s).is_none());
}

#[test]
#[ignore = "RED(ask-learning): 3 approvals from ONE session → no entry"]
fn three_approvals_one_session_give_no_entry() {
    let s = rm_shape();
    let t = learn(&approvals(&s, &[U1, U1, U1]));
    assert!(entry_for(&t, &s).is_none(), "{}", table_json(&t));
    assert!(lookup(&t, &s).is_none());
}

#[test]
#[ignore = "RED(ask-learning): 3 approvals / 2 sessions / 0 declines → allow"]
fn three_approvals_two_sessions_give_allow() {
    let s = rm_shape();
    let t = learn(&approvals(&s, &[U1, U1, U2]));
    let e = entry_for(&t, &s).unwrap_or_else(|| panic!("no entry: {}", table_json(&t)));
    assert_eq!(e["decision"], "allow", "{e}");
    assert_eq!(e["approvals"], 3, "{e}");
    assert_eq!(e["declines"], 0, "{e}");
    assert_eq!(e["sessions"], 2, "{e}");
    assert!(
        matches!(lookup(&t, &s), Some(LearnedDecision::Allow { .. })),
        "lookup must be Allow"
    );
}

#[test]
#[ignore = "RED(ask-learning): any decline → deny, even with 10 approvals"]
fn one_decline_gives_deny_even_with_ten_approvals() {
    let s = rm_shape();
    let mut rs = approvals(&s, &[U1, U2, U3, U1, U2, U3, U1, U2, U3, U1]);
    rs.push(rec(&s, U2, Outcome::Declined, 5000, "rm \"$x\""));
    let t = learn(&rs);
    let e = entry_for(&t, &s).unwrap_or_else(|| panic!("no entry: {}", table_json(&t)));
    assert_eq!(e["decision"], "deny", "{e}");
    assert_eq!(e["approvals"], 10, "{e}");
    assert_eq!(e["declines"], 1, "{e}");
    assert!(matches!(lookup(&t, &s), Some(LearnedDecision::Deny { .. })));
}

#[test]
#[ignore = "RED(ask-learning): a single decline alone → deny"]
fn single_decline_alone_gives_deny() {
    let s = rm_shape();
    let t = learn(&[rec(&s, U1, Outcome::Declined, 1, "rm \"$f\"")]);
    assert!(matches!(lookup(&t, &s), Some(LearnedDecision::Deny { .. })));
}

#[test]
#[ignore = "RED(ask-learning): System / ProtectedConfig classes never allow, but can deny"]
fn system_and_protected_config_never_allow_but_deny() {
    for class in [TargetClass::System, TargetClass::ProtectedConfig] {
        let s = shape("some-rule", class, &["tee"]);
        let t = learn(&approvals(&s, &[U1, U2, U3, U1, U2, U3]));
        assert!(
            !matches!(lookup(&t, &s), Some(LearnedDecision::Allow { .. })),
            "class {} must never be learned Allow: {}",
            s.key(),
            table_json(&t)
        );
        if let Some(e) = entry_for(&t, &s) {
            assert_ne!(e["decision"], "allow", "{e}");
        }
        let mut rs = approvals(&s, &[U1, U2, U3]);
        rs.push(rec(&s, U1, Outcome::Declined, 9, "x"));
        let t = learn(&rs);
        assert!(
            matches!(lookup(&t, &s), Some(LearnedDecision::Deny { .. })),
            "deny learning still applies to {}",
            s.key()
        );
    }
}

#[test]
#[ignore = "RED(ask-learning): system_harm records never allow, but can deny"]
fn system_harm_never_allows_but_denies() {
    let s = shape("rm-system", TargetClass::Home, &["rm"]);
    let harm = |sid: &str, o: Outcome, ts: u64| OutcomeRecord {
        system_harm: true,
        ..rec(&s, sid, o, ts, "rm -rf x")
    };
    let rs = vec![
        harm(U1, Outcome::Approved, 1),
        harm(U2, Outcome::Approved, 2),
        harm(U3, Outcome::Approved, 3),
        harm(U1, Outcome::Approved, 4),
    ];
    let t = learn(&rs);
    assert!(
        !matches!(lookup(&t, &s), Some(LearnedDecision::Allow { .. })),
        "{}",
        table_json(&t)
    );
    let mut rs = rs;
    rs.push(harm(U2, Outcome::Declined, 5));
    assert!(matches!(
        lookup(&learn(&rs), &s),
        Some(LearnedDecision::Deny { .. })
    ));
}

#[test]
#[ignore = "RED(ask-learning): other classes are allowable (Undetermined/Worktree/Scratchpad/Home/None)"]
fn non_protected_classes_are_allowable() {
    for class in [
        TargetClass::Worktree,
        TargetClass::Scratchpad,
        TargetClass::Home,
        TargetClass::Undetermined,
        TargetClass::None,
    ] {
        let s = shape("r", class, &["rm"]);
        let t = learn(&approvals(&s, &[U1, U2, U3]));
        assert!(
            matches!(lookup(&t, &s), Some(LearnedDecision::Allow { .. })),
            "{} should be learned Allow: {}",
            s.key(),
            table_json(&t)
        );
    }
}

#[test]
#[ignore = "RED(ask-learning): lookup is keyed by shape — another shape's entry does not apply"]
fn lookup_does_not_leak_across_shapes() {
    let s = rm_shape();
    let other = shape("rm-undetermined-target", TargetClass::Undetermined, &["rm", "xargs"]);
    let t = learn(&approvals(&s, &[U1, U2, U3]));
    assert!(lookup(&t, &other).is_none());
}

// ------------------------------------------------------------ table form

#[test]
#[ignore = "RED(ask-learning): samples ≤3 and each truncated to 300"]
fn samples_are_capped_and_truncated() {
    let s = rm_shape();
    let long = "x".repeat(1000);
    let rs: Vec<_> = (0..6)
        .map(|i| {
            rec(
                &s,
                [U1, U2][i % 2],
                Outcome::Approved,
                i as u64,
                &format!("rm \"$f{i}\" {long}"),
            )
        })
        .collect();
    let t = learn(&rs);
    let e = entry_for(&t, &s).unwrap();
    let samples = e["samples"].as_array().expect("samples array");
    assert!(!samples.is_empty() && samples.len() <= 3, "{e}");
    for x in samples {
        let x = x.as_str().expect("sample is string");
        assert!(x.chars().count() <= 300, "sample not truncated: {} chars", x.chars().count());
    }
}

#[test]
#[ignore = "RED(ask-learning): table carries no timestamps; entries sorted by shape key"]
fn table_has_no_timestamps_and_sorted_entries() {
    let a = shape("a-rule", TargetClass::Home, &["rm"]);
    let b = shape("b-rule", TargetClass::Home, &["rm"]);
    let mut rs = approvals(&b, &[U1, U2, U3]);
    rs.extend(approvals(&a, &[U1, U2, U3]));
    let t = learn(&rs);
    let es = entries(&t);
    let keys: Vec<String> = es.iter().map(|e| e["shape"].as_str().unwrap().to_string()).collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);
    let text = serde_json::to_string(&t).unwrap();
    assert!(!text.contains("\"ts\""), "table must not contain timestamps: {text}");
    // The exact object form of one entry.
    let e = &es[0];
    let mut fields: Vec<&str> = e.as_object().unwrap().keys().map(String::as_str).collect();
    fields.sort();
    assert_eq!(
        fields,
        ["approvals", "decision", "declines", "samples", "sessions", "shape"],
        "{e}"
    );
}

#[test]
#[ignore = "RED(ask-learning): learn is deterministic — shuffled input → byte-identical JSON"]
fn learn_is_deterministic_under_shuffle() {
    let a = rm_shape();
    let b = shape("other", TargetClass::Worktree, &["git", "rm"]);
    let c = shape("third", TargetClass::Scratchpad, &["tee"]);
    let mut rs = approvals(&a, &[U1, U2, U3, U1]);
    rs.extend(approvals(&b, &[U2, U3, U1]));
    rs.push(rec(&b, U3, Outcome::Declined, 77, "git rm x"));
    rs.extend(approvals(&c, &[U1]));
    let base = serde_json::to_string(&learn(&rs)).unwrap();
    // Deterministic permutations (no rand dependency): every rotation, and
    // every rotation reversed.
    let n = rs.len();
    for k in 1..n {
        let mut p = rs.clone();
        p.rotate_left(k);
        assert_eq!(serde_json::to_string(&learn(&p)).unwrap(), base, "rotation {k}");
        p.reverse();
        assert_eq!(serde_json::to_string(&learn(&p)).unwrap(), base, "rotation {k} reversed");
    }
    let mut rev = rs.clone();
    rev.reverse();
    assert_eq!(serde_json::to_string(&learn(&rev)).unwrap(), base);
}

// ------------------------------------------------------------ table read

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hc-asklearn-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
#[ignore = "RED(ask-learning): read_table on a missing file → Absent"]
fn read_absent() {
    let p = tmp("absent").join("table.json");
    assert!(matches!(read_table(&p), TableRead::Absent));
}

#[test]
#[ignore = "RED(ask-learning): read_table on corrupt / wrong-version / directory → Undetermined"]
fn read_corrupt_or_wrong_version_is_undetermined() {
    let d = tmp("undet");
    for (name, body) in [
        ("garbage", "{not json"),
        ("empty", ""),
        ("wrong-version", r#"{"version":2,"entries":[]}"#),
        ("no-version", r#"{"entries":[]}"#),
        ("entries-not-array", r#"{"version":1,"entries":{}}"#),
    ] {
        let p = d.join(format!("{name}.json"));
        std::fs::write(&p, body).unwrap();
        assert!(
            matches!(read_table(&p), TableRead::Undetermined(..)),
            "{name}: must be Undetermined"
        );
    }
    // A directory where the file should be is unreadable, not absent.
    let dir_as_file = d.join("dir.json");
    std::fs::create_dir_all(&dir_as_file).unwrap();
    assert!(matches!(read_table(&dir_as_file), TableRead::Undetermined(..)));
}

#[test]
#[ignore = "RED(ask-learning): read_table round-trips a learned table → Known, lookups preserved"]
fn read_valid_table_is_known_and_round_trips() {
    let s = rm_shape();
    let d = shape("d", TargetClass::Home, &["rm"]);
    let mut rs = approvals(&s, &[U1, U2, U3]);
    rs.push(rec(&d, U1, Outcome::Declined, 1, "rm y"));
    let t = learn(&rs);
    let p = tmp("known").join("table.json");
    std::fs::write(&p, serde_json::to_string(&t).unwrap()).unwrap();
    match read_table(&p) {
        TableRead::Known(back) => {
            assert!(matches!(lookup(&back, &s), Some(LearnedDecision::Allow { .. })));
            assert!(matches!(lookup(&back, &d), Some(LearnedDecision::Deny { .. })));
            assert_eq!(
                serde_json::to_string(&back).unwrap(),
                serde_json::to_string(&t).unwrap()
            );
        }
        _ => panic!("a valid table must read as Known"),
    }
    // An empty-but-valid table is Known (not Absent, not Undetermined).
    let e = p.with_file_name("empty.json");
    std::fs::write(&e, r#"{"version":1,"entries":[]}"#).unwrap();
    match read_table(&e) {
        TableRead::Known(t) => assert!(lookup(&t, &s).is_none()),
        _ => panic!("empty valid table must be Known"),
    }
}
