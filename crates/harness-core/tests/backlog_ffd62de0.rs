#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog ffd62de0: the undetermined telemetry sink (`undetermined.jsonl`)
//! has no rotation or compaction. The only bound is per PROCESS
//! (`MAX_RECORDS_PER_PROCESS`); across processes the file grows forever — the
//! live store measured 33.8 MB / 131k lines (2026-09-29) and 47.4 MB / 178k
//! lines (2026-10-02).
//!
//! Repro: point the sink (HARNESS_UNDETERMINED_SINK) at a file that is ALREADY
//! 1 GiB (sparse, so the test is cheap) and record one more event. Any bound
//! at all — rotation, compaction, truncation, refusing to grow — keeps the
//! active file from growing past 1 GiB; today it simply appends.
//!
//! One test per binary: it sets a process-global env var.

use harness_core::undetermined;

#[test]
#[ignore = "backlog ffd62de0: open defect, remove ignore when fixed"]
fn sink_does_not_grow_without_bound() {
    let dir = tempfile::tempdir().unwrap();
    let sink = dir.path().join("undetermined.jsonl");
    const GIB: u64 = 1 << 30;
    {
        let f = std::fs::File::create(&sink).unwrap();
        f.set_len(GIB).unwrap();
    }
    std::env::set_var(undetermined::SINK_ENV, &sink);
    assert!(
        matches!(
            undetermined::sink_state(),
            undetermined::SinkState::Active(_)
        ),
        "precondition: recording is active"
    );

    undetermined::record("backlog ffd62de0 probe", std::panic::Location::caller());
    assert_eq!(
        undetermined::written(),
        1,
        "precondition: the event was taken"
    );
    assert_eq!(
        undetermined::dropped_writes(),
        0,
        "precondition: no write failure"
    );

    let after = std::fs::metadata(&sink).unwrap().len();
    assert!(
        after <= GIB,
        "the undetermined sink grew past an already-1GiB file ({after} bytes): no \
         rotation/compaction bounds it"
    );
}
