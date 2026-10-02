//! Fault-injection fail-closed check for stuckguard's stuck-loop verdict
//! (backlog 8696dd7e, slice 4 adopter).
//!
//! Path under test: `state::load` -> `harness_core::store::load_json_determined`
//! -> `boundary::read_to_string` (`Entry::ReadFile`). A history the boundary
//! could not read must resolve to "no verdict, nudge out of caution"
//! (`allowed_to_carry_on` == `None`, mirroring `main::watch`), never to the
//! permissive `Some(true)` an empty window would produce.
//!
//! The session is seeded already stuck so the permissive answer is reachable
//! (the control asserts the intact read nudges).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crate::verdict_monotonicity::{allowed_to_carry_on, cfg_for, event, seed_stuck_session};
use harness_core::boundary::fault::{with_fault_plan, Entry, FaultPlan};
use harness_core::degrade::assert_fails_closed;

const SESSION: &str = "s";

#[test]
fn control_unfaulted_read_of_a_stuck_session_nudges() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = cfg_for(dir.path());
    seed_stuck_session(dir.path(), SESSION, 5);
    let run = with_fault_plan(FaultPlan::none(), || {
        allowed_to_carry_on(dir.path(), SESSION, event("Bash", "same-command"), &cfg)
    });
    assert_eq!(run.injected, 0, "FaultPlan::none() must inject nothing");
    assert_eq!(
        run.value,
        Some(false),
        "control: intact stuck history nudges"
    );
}

#[test]
fn blind_boundary_never_leaves_the_verdict_permissive() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = cfg_for(dir.path());
    seed_stuck_session(dir.path(), SESSION, 5);
    assert_fails_closed(|| {
        allowed_to_carry_on(dir.path(), SESSION, event("Bash", "same-command"), &cfg)
    });
}

#[test]
fn faulted_history_read_is_undetermined_not_all_clear() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = cfg_for(dir.path());
    seed_stuck_session(dir.path(), SESSION, 5);
    let run = with_fault_plan(FaultPlan::only(Entry::ReadFile), || {
        allowed_to_carry_on(dir.path(), SESSION, event("Bash", "same-command"), &cfg)
    });
    assert!(
        run.injected >= 1,
        "the history read must have hit the faulted seam (injected = {})",
        run.injected
    );
    assert_eq!(
        run.value, None,
        "a faulted history read must be undetermined (nudge), never Some(true) all-clear"
    );
}
