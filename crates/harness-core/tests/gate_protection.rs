#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED tests for backlog 3a8e3b73, slice 1 (phased ruling): the shared
//! protection-statement type and the canonical blocking-gate list.
//! Written by an author independent of the implementation (CLAUDE.md 2(a)).
//! Deferred to later slices: refusal one-line summary, taxonomy column.

use harness_core::fleet::{BLOCKING_GATES, GATE_CRATES};
use harness_core::gate::Protection;

/// The Protection type carries three `&'static str` fields.
#[test]
fn protection_has_protects_against_grounds() {
    const P: Protection = Protection {
        protects: "the shell and filesystem",
        against: "instructions embedded in fetched content",
        grounds: "observed 2026-08-05",
    };
    assert_eq!(P.protects, "the shell and filesystem");
    assert_eq!(P.against, "instructions embedded in fetched content");
    assert_eq!(P.grounds, "observed 2026-08-05");
}

/// BLOCKING_GATES must cover every canary GATE crate (superset, so it can grow).
#[test]
fn blocking_gates_is_superset_of_gate_crates() {
    for g in GATE_CRATES {
        assert!(
            BLOCKING_GATES.contains(g),
            "GATE crate {g} missing from BLOCKING_GATES"
        );
    }
}

/// Control: no duplicates and no empty names in the list.
#[test]
fn blocking_gates_has_no_empty_or_duplicate_names() {
    let mut seen = std::collections::BTreeSet::new();
    for g in BLOCKING_GATES {
        assert!(!g.trim().is_empty(), "empty gate name");
        assert!(seen.insert(*g), "duplicate gate {g}");
    }
}
