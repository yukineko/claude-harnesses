// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! End-to-end: does constructing an `Undetermined` actually land a record?
//! (backlog 6d493e39)
//!
//! Its own process, because the sink is selected from a process-global env var
//! and the lib tests must not see it flipped. Everything here runs serially
//! within this binary for the same reason.
//!
//! RED, observed by mutation rather than asserted: deleting the
//! `crate::undetermined::record(..)` call from `Verdict::undetermined` makes
//! `a_verdict_undetermined_is_recorded` fail with zero records. That is the
//! whole claim of this feature, so it gets a test that dies without it.

use harness_core::undetermined::{self, SinkState};
use harness_core::verdict::{Determination, Verdict};

fn read_records(path: &std::path::Path) -> Vec<serde_json::Value> {
    let Ok(txt) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    txt.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("each record must be valid JSON"))
        .collect()
}

struct Sink {
    dir: tempfile::TempDir,
}

impl Sink {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var(undetermined::SINK_ENV, dir.path().join("u.jsonl"));
        Sink { dir }
    }
    fn path(&self) -> std::path::PathBuf {
        self.dir.path().join("u.jsonl")
    }
    fn records(&self) -> Vec<serde_json::Value> {
        read_records(&self.path())
    }
}

impl Drop for Sink {
    fn drop(&mut self) {
        std::env::remove_var(undetermined::SINK_ENV);
    }
}

#[test]
fn the_whole_contract() {
    // One test, sequenced, because the sink env var and the process-wide record
    // counter are both global: splitting these into parallel #[test]s would let
    // them observe each other's state.
    a_verdict_undetermined_is_recorded();
    a_determination_undetermined_is_recorded();
    the_record_is_attributed_to_the_caller_not_the_constructor();
    clean_and_violation_record_nothing();
    off_records_nothing();
}

fn a_verdict_undetermined_is_recorded() {
    let sink = Sink::new();
    assert_eq!(sink.records().len(), 0, "control: sink starts empty");

    let v = Verdict::undetermined("git rev-parse exited 128");
    assert!(matches!(v, Verdict::Undetermined(_)));

    let recs = sink.records();
    assert_eq!(
        recs.len(),
        1,
        "constructing an Undetermined must record exactly one event; got {recs:?}"
    );
    let r = &recs[0];
    assert_eq!(r["reason"], "git rev-parse exited 128");
    assert_eq!(r["capped"], false);
    assert!(
        r["ts"].as_i64().unwrap() > 1_700_000_000,
        "ts looks unset: {r}"
    );
    assert!(r["line"].as_u64().unwrap() > 0);
}

fn a_determination_undetermined_is_recorded() {
    let sink = Sink::new();
    let d: Determination<Vec<u8>> = Determination::undetermined("ledger unreadable");
    assert!(matches!(d, Determination::Undetermined(_)));

    let recs = sink.records();
    assert_eq!(recs.len(), 1, "Determination::undetermined must record too");
    assert_eq!(recs[0]["reason"], "ledger unreadable");
}

/// The attribution claim. Without `#[track_caller]` every record would point at
/// `harness-core/src/verdict.rs`, which would make the by-crate aggregation —
/// the entire point of the feature — report `harness-core` for all of it.
fn the_record_is_attributed_to_the_caller_not_the_constructor() {
    let sink = Sink::new();
    let _ = Verdict::undetermined("attributed to me");

    let recs = sink.records();
    assert_eq!(recs.len(), 1);
    let file = recs[0]["file"].as_str().unwrap();
    assert!(
        file.contains("undetermined_telemetry.rs"),
        "record must point at the CALLER's file; got {file:?}. If this says \
         verdict.rs, #[track_caller] was dropped and every event in the fleet \
         is now attributed to harness-core."
    );
    // The crate must be derived from the CALLER's file. Note it legitimately
    // reads `harness-core` here — this test lives in that crate — so asserting
    // `!= "harness-core"` would be wrong, and was: the first draft did exactly
    // that and failed. What actually matters is that the two agree, since a
    // crate field computed from anything but the recorded file would silently
    // misattribute events fleet-wide.
    assert_eq!(
        recs[0]["crate"].as_str().unwrap(),
        undetermined::crate_of(file),
        "the crate field must be derived from the recorded file"
    );
    assert_eq!(undetermined::crate_of(file), "harness-core");
}

/// Anti-vacuity: if the recorder fired on every verdict, the counts would be a
/// measure of gate traffic rather than of undetermined branches, and the number
/// would look alarming for no reason.
fn clean_and_violation_record_nothing() {
    let sink = Sink::new();
    let _clean = Verdict::from_findings(vec![]);
    let _viol = Verdict::violation("a real finding");
    let _known: Determination<u8> = Determination::known(7);
    assert_eq!(
        sink.records().len(),
        0,
        "only the undetermined branch is telemetry-bearing"
    );
}

fn off_records_nothing() {
    let sink = Sink::new();
    std::env::set_var(undetermined::SINK_ENV, "off");
    assert_eq!(undetermined::sink_state(), SinkState::DisabledByEnv);
    let _ = Verdict::undetermined("should not be recorded");
    assert_eq!(sink.records().len(), 0);
    // And the state is self-describing, so a reader of that zero is warned.
    assert!(SinkState::DisabledByEnv
        .describe()
        .contains("nothing was recorded"));
}
