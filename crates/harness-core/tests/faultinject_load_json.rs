// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! FAULT INJECTION — C1 (first half): a load fault must stay distinguishable
//! from an absent store.
//!
//! CLAUDE.md §3: IO failure / unreadable file / unparseable content must NEVER
//! map to the same output as "fine". The original `store::load_json`
//! (`-> T`, no `Result`, no tri-state) made that distinction *unrepresentable*
//! at the type level, so every consumer inherited the collapse — these three
//! tests were RED against it (fault value == absent value == `State::default()`).
//!
//! They now target [`store::load_json_determined`] (`-> Determination<T>`), the
//! representation that fix added: an ABSENT store is `Known(default)`, an
//! unreadable / unsearchable / corrupt one is `Undetermined`. The same three
//! faults are injected and the returned value is asserted DISTINCT from the
//! value for a genuinely absent store — the assertion the disinterested oracle
//! made, now satisfiable because the type can finally hold the difference.
//!
//! The permission-based faults (unreadable file, unsearchable dir) are asserted
//! with `!=` rather than a hard `Undetermined` match on purpose: under a root
//! uid the mode bits do not deny the read, so the store comes back as
//! `Known(populated)` — still distinct from `Known(default)`. Only the corrupt
//! case is `Undetermined` on every uid.

use harness_core::verdict::Determination;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Stand-in for any plugin state struct: `Default + DeserializeOwned`, exactly
/// what `load_json_determined`'s bound requires.
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

/// What a caller gets when the store genuinely does not exist: `Known(default)`.
/// Any fault injection that produces THIS value has erased the difference
/// between "nothing recorded" and "could not read what was recorded".
fn absent_store_value(tmp: &Path) -> Determination<State> {
    harness_core::store::load_json_determined::<State>(&tmp.join("definitely-absent.json"))
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

    let got = harness_core::store::load_json_determined::<State>(&path);

    // Restore BEFORE asserting so a failing assert never leaves an
    // undeletable tempdir (mirrors specguard/src/decision.rs:183-203).
    let mut restore = std::fs::metadata(&path).unwrap().permissions();
    restore.set_mode(0o600);
    let _ = std::fs::set_permissions(&path, restore);

    let absent = absent_store_value(tmp.path());
    assert_ne!(
        got,
        absent,
        "REGRESSION: an UNREADABLE store (EACCES) yielded the exact same value as \
         an ABSENT store ({absent:?}). load_json_determined must return \
         Undetermined for it, not collapse to the absent-store value. Real \
         content was {:?}.",
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

    let got = harness_core::store::load_json_determined::<State>(&path);

    let mut restore = std::fs::metadata(&dir).unwrap().permissions();
    restore.set_mode(0o700);
    let _ = std::fs::set_permissions(&dir, restore);

    let absent = absent_store_value(tmp.path());
    assert_ne!(
        got,
        absent,
        "REGRESSION: an UNSEARCHABLE parent dir yielded the exact same value as \
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

    let got = harness_core::store::load_json_determined::<State>(&path);

    let absent = absent_store_value(tmp.path());
    assert_ne!(
        got, absent,
        "REGRESSION: CORRUPT content yielded the exact same value as an ABSENT \
         store ({absent:?}). A truncated/schema-drifted store must be \
         Undetermined, not read as a fresh one."
    );
    // Corruption is uid-independent, so here the exact tri-state answer is
    // pinned: an unparseable store is Undetermined, never Known.
    assert!(
        matches!(got, Determination::Undetermined(_)),
        "corrupt content must be Undetermined, got {got:?}"
    );
}
