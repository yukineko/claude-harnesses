// 丸ごと integration test なので unwrap/expect/panic を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Fault-injection fail-closed check for blastguard's approval memory
//! (backlog 8696dd7e, slice 4).
//!
//! `approve::Store::lookup(..).is_approved()` is the one answer that may
//! downgrade an `Ask` to an `Allow`. This test forces EVERY boundary IO entry to
//! `Undetermined` (`degrade::assert_fails_closed`) and requires the lookup not to
//! stay approving. The store holds a genuinely approved entry, so a lookup that
//! ignores the fault and reads it anyway is observably permissive.
//!
//! Needs `harness-core = { path = "../harness-core", features =
//! ["fault-injection"] }` under blastguard's `[dev-dependencies]`.

use blastguard::approve::{command_key, Store};
use harness_core::degrade::assert_fails_closed;

#[test]
fn blind_boundary_never_leaves_an_approval_lookup_permissive() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path());
    let fp = "f".repeat(64);
    let key = command_key("Bash", "rm -rf build");
    store.put_pending(&key, &fp, "rm -rf build").unwrap();
    assert!(
        matches!(
            store.promote(&key),
            harness_core::verdict::Determination::Known(_)
        ),
        "setup: promote must succeed"
    );

    // Anti-vacuity control: with no fault the entry really approves.
    assert!(
        store.lookup(&fp).is_approved(),
        "control: a promoted approval must read as approved"
    );

    assert_fails_closed(|| store.lookup(&fp).is_approved());
}

/// Second verdict: is the file a redirect would clobber recoverable from git?
/// (reversible::probe). true lets the write through; a blind probe must not.
#[test]
fn blind_boundary_never_leaves_a_recoverability_probe_permissive() {
    use std::process::Command;
    let repo = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let st = Command::new("git")
            .args(args)
            .current_dir(repo.path())
            .status()
            .unwrap();
        assert!(st.success(), "setup: git {args:?}");
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@example.invalid"]);
    git(&["config", "user.name", "t"]);
    std::fs::write(repo.path().join("tracked.txt"), "x\n").unwrap();
    git(&["add", "tracked.txt"]);
    git(&["commit", "-q", "-m", "init"]);
    let target = repo.path().join("tracked.txt");
    let target = target.to_str().unwrap();

    let verdict = || blastguard::reversible::probe(target, None).is_recoverable();
    assert!(
        verdict(),
        "control: a clean tracked file must read as recoverable"
    );
    assert_fails_closed(verdict);
}
