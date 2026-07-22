// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! FAULT INJECTION — C1 (first half): `harness_core::store::load_json` collapses
//! "cannot determine" into `T::default()`.
//!
//! CLAUDE.md §3: IO failure / unreadable file / unparseable content must NEVER
//! map to the same output as "fine". `load_json`'s signature
//! (`-> T`, no `Result`, no tri-state) makes that distinction *unrepresentable*
//! at the type level, so every consumer inherits the collapse.
//!
//! These tests inject three faults — (1) unreadable file, (2) unsearchable
//! parent dir, (3) corrupt content — and assert the returned value is
//! distinguishable from the value you get for a genuinely absent store. They
//! are EXPECTED TO FAIL against current code; the failure IS the observation.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Stand-in for any plugin state struct: `Default + DeserializeOwned`, exactly
/// what `load_json`'s bound requires.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
struct State {
    seq: u64,
    events: Vec<String>,
}

fn populated() -> State {
    State {
        seq: 7,
        events: vec!["a".into(), "b".into(), "c".into()],
    }
}

/// What a caller gets when the store genuinely does not exist. Any fault
/// injection that produces THIS value has erased the difference between
/// "nothing recorded" and "could not read what was recorded".
fn absent_store_value(tmp: &Path) -> State {
    harness_core::store::load_json::<State>(&tmp.join("definitely-absent.json"))
}

#[cfg(unix)]
#[test]
fn unreadable_file_is_distinguishable_from_absent_store() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("state.json");
    std::fs::write(&path, serde_json::to_string(&populated()).unwrap()).unwrap();

    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o000);
    std::fs::set_permissions(&path, perms).unwrap();

    let got = harness_core::store::load_json::<State>(&path);

    // Restore BEFORE asserting so a failing assert never leaves an
    // undeletable tempdir (mirrors specguard/src/decision.rs:183-203).
    let mut restore = std::fs::metadata(&path).unwrap().permissions();
    restore.set_mode(0o600);
    let _ = std::fs::set_permissions(&path, restore);

    let absent = absent_store_value(tmp.path());
    assert_ne!(
        got,
        absent,
        "FAIL-OPEN: an UNREADABLE store (EACCES) yielded the exact same value as \
         an ABSENT store ({absent:?}). The real content was {:?}. A consumer \
         cannot tell 'nothing happened yet' from 'I could not read what happened'.",
        populated()
    );
}

#[cfg(unix)]
#[test]
fn unsearchable_parent_dir_is_distinguishable_from_absent_store() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("state.json");
    std::fs::write(&path, serde_json::to_string(&populated()).unwrap()).unwrap();

    let mut perms = std::fs::metadata(&dir).unwrap().permissions();
    perms.set_mode(0o000); // unsearchable: open() on any child fails with EACCES
    std::fs::set_permissions(&dir, perms).unwrap();

    let got = harness_core::store::load_json::<State>(&path);

    let mut restore = std::fs::metadata(&dir).unwrap().permissions();
    restore.set_mode(0o700);
    let _ = std::fs::set_permissions(&dir, restore);

    let absent = absent_store_value(tmp.path());
    assert_ne!(
        got,
        absent,
        "FAIL-OPEN: an UNSEARCHABLE parent dir yielded the exact same value as \
         an ABSENT store ({absent:?}); real content was {:?}.",
        populated()
    );
}

#[test]
fn corrupt_content_is_distinguishable_from_absent_store() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("state.json");
    // Valid path, readable, but not parseable as `State` — a real schema-drift
    // / truncated-write scenario, not a hypothetical.
    std::fs::write(&path, b"\x00\x01 not json at all {{{").unwrap();

    let got = harness_core::store::load_json::<State>(&path);

    let absent = absent_store_value(tmp.path());
    assert_ne!(
        got, absent,
        "FAIL-OPEN: CORRUPT content yielded the exact same value as an ABSENT \
         store ({absent:?}). A truncated/schema-drifted store reads as a fresh one."
    );
}
