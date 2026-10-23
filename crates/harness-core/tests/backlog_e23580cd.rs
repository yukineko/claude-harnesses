// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog e23580cd: `interrogate.rs` documented a purity invariant ("no file
//! I/O, ...", and `ScopeDraft::declare` as "Pure — no IO of any kind"), but
//! `declare`'s Undetermined arms go through `Determination::undetermined`,
//! which calls `crate::undetermined::record` and appends a line to the
//! telemetry sink whenever the sink is active.
//!
//! User ruling 2026-10-22: the doc is aligned to the actual behaviour
//! (creating an Undetermined value writes one telemetry line), and this repro's
//! expectation changed to match the corrected doc. It now pins BOTH halves of
//! the agreement: the observed behaviour (one telemetry line, attributed to
//! interrogate.rs) and the doc text that states it, so doc and behaviour cannot
//! drift apart again silently.
//!
//! One test per file: the sink is selected through a process-global env var.

use harness_core::interrogate::{ScopeDraft, SCOPE_WRITE_PATHS_UNASKED};
use harness_core::undetermined::SINK_ENV;

/// The source whose doc comments make the claim.
const INTERROGATE_RS: &str = include_str!("../src/interrogate.rs");

#[test]
fn declare_on_an_unasked_draft_writes_one_telemetry_line() {
    // Doc half: the corrected doc states the telemetry write, and the old
    // "no IO of any kind" claim is gone.
    assert!(
        INTERROGATE_RS.contains("writes one telemetry line"),
        "interrogate.rs must document that an Undetermined writes one telemetry line"
    );
    assert!(
        !INTERROGATE_RS.contains("no IO of any kind"),
        "interrogate.rs still claims `declare` performs no IO of any kind"
    );

    // Behaviour half, with the sink pointed at a temp file (the explicit-sink
    // production path, not the cargo-suppressed default).
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

    let text = std::fs::read_to_string(&sink)
        .unwrap_or_else(|e| panic!("expected a telemetry line at {}: {e}", sink.display()));
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 1, "exactly one telemetry line: {text:?}");
    assert!(
        lines[0].contains("crates/harness-core/src/interrogate.rs"),
        "attributed to interrogate.rs: {}",
        lines[0]
    );
    assert!(
        lines[0].contains(SCOPE_WRITE_PATHS_UNASKED),
        "carries the rule tag: {}",
        lines[0]
    );
    let _ = std::fs::remove_dir_all(&dir);
}
