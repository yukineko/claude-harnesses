//! Fault-injection fail-closed check for parallelguard's PreToolUse verdict
//! (backlog 8696dd7e, slice 4 adopter #4).
//!
//! Path under test: [`crate::decide_with`] -> `store::load` ->
//! `harness_core::store::load_json_determined` -> `boundary::read_to_string`.
//! An in-flight ledger the boundary could not read is `Undetermined`, which
//! must resolve to `Decision::Deny`, never to `Allow`.
//!
//! Why this gate in particular: parallelguard's whole contract is that an
//! UNKNOWN count is not a free slot. Its PreToolUse hook signals "allow" by
//! exiting 0 with no output, so silence is literally the permissive answer and
//! cannot be used as a degradation. That makes "blind => still Deny" the one
//! invariant worth pinning mechanically rather than in prose.
//!
//! The state root is supplied INLINE through `decide_with`'s `root` parameter,
//! so no boundary read has to succeed before the ledger read is reached — the
//! only faulted call on the path is the one under test.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use harness_core::boundary::fault::{with_fault_plan, Entry, FaultPlan};
use harness_core::degrade::{assert_fails_closed, Permissiveness};
use harness_core::hook::HookInput;

use crate::model::Decision;
use crate::{decide_with, store};

/// `Decision` scored for `degrade`: `Allow` is permissive, `Deny` is not.
///
/// This is the polarity parallelguard's own `deny_json` assumes, and it is the
/// natural direction here (unlike stuckguard's `Option<Trip>`, where `None` is
/// the permissive answer and has to be inverted at the call site).
impl Permissiveness for Decision {
    fn permissiveness(&self) -> u8 {
        match self {
            Decision::Allow => 1,
            Decision::Deny(_) => 0,
        }
    }
}

/// A metered PreToolUse payload: `Bash` maps to `SlotClass::Shell`.
fn input() -> HookInput {
    HookInput::parse(
        r#"{"session_id":"fault-injection-session",
             "hook_event_name":"PreToolUse",
             "tool_name":"Bash",
             "tool_input":{"command":"echo hi"}}"#,
    )
    .expect("the fixture payload must parse")
}

const CAP: usize = 3;

#[test]
fn control_an_intact_ledger_read_allows_the_first_call() {
    let root = tempfile::tempdir().unwrap();
    let run = with_fault_plan(FaultPlan::none(), || {
        decide_with(&input(), root.path(), CAP)
    });
    assert_eq!(run.injected, 0, "FaultPlan::none() must inject nothing");
    assert_eq!(
        run.value,
        Decision::Allow,
        "control: the first metered call under a cap of {CAP} must be allowed, \
         otherwise the faulted case below proves nothing"
    );
}

#[test]
fn blind_boundary_never_leaves_the_pretooluse_verdict_permissive() {
    let root = tempfile::tempdir().unwrap();
    // Panics with VACUOUS if the verdict path makes no boundary call at all,
    // and with FAIL-OPEN if a fully blind run still returns Allow.
    let _verdict = assert_fails_closed(|| decide_with(&input(), root.path(), CAP));
}

#[test]
fn a_faulted_ledger_read_is_the_unknown_count_deny() {
    let root = tempfile::tempdir().unwrap();
    let run = with_fault_plan(FaultPlan::only(Entry::ReadFile), || {
        decide_with(&input(), root.path(), CAP)
    });
    assert!(
        run.injected >= 1,
        "the ledger read must have hit the faulted seam (injected = {}); if this \
         is 0 the read is bypassing harness_core::boundary",
        run.injected
    );
    match &run.value {
        Decision::Deny(reason) => assert!(
            reason.contains("in-flight ledger could not be read"),
            "the deny must name the unreadable ledger, got: {reason}"
        ),
        Decision::Allow => panic!(
            "an unreadable ledger is an UNKNOWN count, which is not a free slot \
             (CLAUDE.md 3) — it must not be Allow"
        ),
    }
}

#[test]
fn an_unmetered_tool_is_passed_through_without_reading_the_ledger() {
    // Guards the control's meaning: `Allow` for a non-metered tool is a
    // pass-through decided BEFORE any boundary call, so it is not evidence
    // that the ledger read succeeded, and it must not be read as fail-open.
    let root = tempfile::tempdir().unwrap();
    let unmetered =
        HookInput::parse(r#"{"session_id":"s","hook_event_name":"PreToolUse","tool_name":"Read"}"#)
            .unwrap();
    let run = with_fault_plan(FaultPlan::blind(), || {
        decide_with(&unmetered, root.path(), CAP)
    });
    assert_eq!(
        run.injected, 0,
        "an unmetered tool must not reach a boundary"
    );
    assert_eq!(run.value, Decision::Allow);
}

#[test]
fn the_ledger_path_is_under_the_supplied_root() {
    // `decide_with`'s extraction is only behaviour-preserving if `root` is the
    // directory actually used. Pin it so a future edit cannot quietly go back
    // to the per-user state dir and make every test above vacuous.
    let root = tempfile::tempdir().unwrap();
    let p = store::session_path(root.path(), &input().session_key());
    assert!(
        p.starts_with(root.path()),
        "{} must live under the supplied root {}",
        p.display(),
        root.path().display()
    );
}
