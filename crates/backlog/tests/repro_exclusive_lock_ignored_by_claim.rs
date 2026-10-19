#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro: `lock acquire` is documented as the way to "exclude every other
//! session from a project", but `next --claim` from another session never reads
//! the exclusive lock, so a held lock does not stop claims.

mod common;

#[test]
fn held_exclusive_lock_stops_other_sessions_claiming() {
    let f = common::Fixture::new("excl-lock");
    let id = f.add("exclusive lock vs claim");
    let p = f.project();

    let acq = f.run(&[
        "lock",
        "acquire",
        "--session-id",
        "session-A",
        "--project",
        &p,
    ]);
    assert_eq!(
        acq.code,
        0,
        "apparatus: lock acquire failed: {}",
        acq.both()
    );
    let st = f.run(&["lock", "status", "--project", &p]);
    assert!(
        st.stdout.contains("session-A"),
        "apparatus: lock must be held by session-A: {}",
        st.both()
    );

    // A DIFFERENT session claims while A holds the exclusive lock.
    let claim = f.run_env(
        &["next", "--claim", "--project", &p],
        &[("CLAUDE_CODE_SESSION_ID", "session-B")],
    );
    let got_task = claim.code == 0 && claim.stdout.contains(&id);
    assert!(
        !got_task,
        "session-B was handed task {id} while session-A holds the exclusive lock. {}",
        claim.both()
    );
}

/// Control: the exclusion is against OTHER sessions. The holder's own session
/// still claims, and once the lock is released another session claims again,
/// so "refuse every claim" cannot pass the repro above.
#[test]
fn lock_holder_claims_and_release_restores_others() {
    let f = common::Fixture::new("excl-lock-holder");
    let a = f.add("first");
    let b = f.add("second");
    let p = f.project();
    let acq = f.run(&[
        "lock",
        "acquire",
        "--session-id",
        "session-A",
        "--project",
        &p,
    ]);
    assert_eq!(acq.code, 0, "apparatus: {}", acq.both());

    let own = f.run_env(
        &["next", "--claim", "--project", &p],
        &[("CLAUDE_CODE_SESSION_ID", "session-A")],
    );
    assert_eq!(own.code, 0, "the holder must still claim: {}", own.both());
    assert!(
        own.stdout.contains(&a) || own.stdout.contains(&b),
        "holder got no task: {}",
        own.both()
    );

    let other = f.run_env(
        &["next", "--claim", "--project", &p],
        &[("CLAUDE_CODE_SESSION_ID", "session-B")],
    );
    assert_ne!(other.code, 0, "{}", other.both());
    assert!(
        other.both().contains("exclusive lock") && other.both().contains("session-A"),
        "the refusal must name the lock and its holder: {}",
        other.both()
    );

    let rel = f.run(&["lock", "release", "--project", &p]);
    assert_eq!(rel.code, 0, "apparatus: {}", rel.both());
    let after = f.run_env(
        &["next", "--claim", "--project", &p],
        &[("CLAUDE_CODE_SESSION_ID", "session-B")],
    );
    assert_eq!(
        after.code,
        0,
        "released lock must not exclude: {}",
        after.both()
    );
    assert!(
        after.stdout.contains(&a) || after.stdout.contains(&b),
        "session-B got no task after release: {}",
        after.both()
    );
}
