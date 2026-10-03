// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog e23580cd: `interrogate.rs` documents a purity invariant
//! (`crates/harness-core/src/interrogate.rs:18`, "Purity invariant: no file
//! I/O, no network, ..."), and `tests/interrogate_purity.rs` guards it only
//! lexically. But `ScopeDraft::declare`'s Undetermined arms go through
//! `Determination::undetermined`, which calls `crate::undetermined::record`
//! and appends a line to the telemetry sink whenever the sink is active.
//!
//! This test observes the behaviour rather than the source text: with the sink
//! pointed at a temp file (the explicit-sink production path, not the
//! cargo-suppressed default), a single `declare()` on an unasked draft must
//! leave the filesystem untouched if the documented invariant holds.
//!
//! One test per file: the sink is selected through a process-global env var.

use harness_core::interrogate::ScopeDraft;
use harness_core::undetermined::SINK_ENV;

#[test]
#[ignore = "backlog e23580cd: open defect, remove ignore when fixed"]
fn declare_on_an_unasked_draft_performs_no_file_io() {
    let dir = std::env::temp_dir().join(format!(
        "hc-e23580cd-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let sink = dir.join("undetermined.jsonl");
    std::env::set_var(SINK_ENV, &sink);

    let d = ScopeDraft {
        write_paths: None,
        read_paths: None,
    }
    .declare();
    assert!(
        matches!(d, harness_core::verdict::Determination::Undetermined(_)),
        "precondition: an unasked draft must be Undetermined"
    );

    assert!(
        !sink.exists(),
        "interrogate.rs claims `no file I/O`, but ScopeDraft::declare wrote {} ({:?})",
        sink.display(),
        std::fs::read_to_string(&sink).unwrap_or_default()
    );
}
