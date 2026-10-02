#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro for backlog 66fb376a: `harness_core::boundary::read_dir_entries`
//! maps `NotFound` to `Known(vec![])`, the same value an existing empty
//! directory returns, so a caller that must tell "absent" from "empty" (e.g.
//! blastguard retro's mistyped `--dir`) has to re-derive it per call site with
//! an `is_dir()` probe. The property pinned here: the two are distinguishable
//! from the return value alone.
//!
//! Note: the ticket leaves type-change vs docstring-guidance to the owner; a
//! docstring-only resolution would leave this test red by design.

use harness_core::boundary::read_dir_entries;

#[test]
#[ignore = "backlog 66fb376a: open defect, remove ignore when fixed"]
fn backlog_66fb376a_absent_dir_is_distinguishable_from_empty_dir() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "hc-backlog-66fb376a-{}-{nanos}",
        std::process::id()
    ));
    let empty = root.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let absent = root.join("does-not-exist");
    let e = read_dir_entries(&empty);
    let a = read_dir_entries(&absent);
    let _ = std::fs::remove_dir_all(&root);
    assert_ne!(
        a, e,
        "an absent directory and an empty directory return the same value ({a:?})"
    );
}
