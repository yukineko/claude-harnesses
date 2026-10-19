#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro: `lock acquire` is documented as the way to "exclude every other
//! session from a project", but `next --claim` from another session never reads
//! the exclusive lock, so a held lock does not stop claims.

mod common;

#[test]
#[ignore = "RED repro: next --claim ignores a held exclusive lock"]
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
