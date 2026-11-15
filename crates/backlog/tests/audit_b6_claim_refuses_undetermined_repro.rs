#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Backlog 78cee053, companion of `audit_b6_claim_rejects_green_repro.rs`.
//! `next --claim` re-runs the top candidate's stored repro. User ruling
//! (2026-11-07, "Refuse the claim"): when that re-run CANNOT BE DETERMINED the
//! claim is refused: the ticket is NOT handed out, the exit is non-zero and the
//! reason is printed (CLAUDE.md §3: undetermined resolves to the restrictive side).
//!
//! The undetermined trigger is an INFRASTRUCTURE one: the re-run is killed by
//! a signal (no exit code), which `evidence::run_repro_classified` classifies
//! as `undeterminable`. The script is RED when the ticket is filed and, after
//! a committed state flip, kills itself with SIGKILL at the re-run, so the tree
//! stays clean and the only cause is the signal. (Until 2026-11-13 the trigger
//! was a repro script removed from HEAD; user ruling 2026-11-13
//! 「保留はありえない実証されないならclose,実証されたらopen」 reclassified that as
//! NOT DEMONSTRATED, which auto-cancels instead of refusing. The property
//! pinned here — an undeterminable re-run is refused — is unchanged.)

mod common;

use common::Fixture;

#[test]
fn next_claim_refuses_a_ticket_whose_repro_rerun_is_undetermined() {
    let f = Fixture::new("b78cee053");
    f.write("src/mode.txt", "red\n");
    f.write_script(
        "tests/repro_gone.sh",
        "grep -q '^kill$' src/mode.txt && kill -KILL $$\necho 'bug present'; exit 1",
    );
    f.commit_paths(&["src/mode.txt", "tests/repro_gone.sh"], "add repro");

    let p = f.project();
    let o = f.run(&[
        "add",
        "--title",
        "repro vanishes",
        "--project",
        &p,
        "--repro-test",
        "bash tests/repro_gone.sh",
    ]);
    assert_eq!(
        o.code,
        0,
        "control: add must file the RED ticket: {}",
        o.both()
    );
    let id = common::parse_added(&o.stdout);

    // The re-run is now killed by a signal: undeterminable (infrastructure).
    f.write("src/mode.txt", "kill\n");
    f.commit_paths(&["src/mode.txt"], "re-run is killed");

    let c = f.run(&["next", "--claim"]);
    assert_ne!(
        c.code,
        0,
        "next --claim must exit non-zero when the repro re-run is undetermined: {}",
        c.both()
    );
    assert!(
        !c.stdout.contains(&id),
        "next --claim handed out {id} although its repro re-run is undetermined: {}",
        c.both()
    );
    assert!(
        c.stderr.contains("signal") || c.stdout.contains("signal"),
        "the refusal must print the reason (the re-run was killed by a signal): {}",
        c.both()
    );

    assert_eq!(
        f.status(&id),
        "pending",
        "an undeterminable re-run must NOT auto-cancel {id}: {}",
        c.both()
    );

    let claimed = f.run(&["list", "--status", "claimed"]);
    assert!(
        !claimed.both().contains(&id),
        "{id} must not be claimed after a refused claim: {}",
        claimed.both()
    );
}

#[test]
fn next_claim_hands_out_a_still_red_ticket_control() {
    let f = Fixture::new("b78cee053c");
    let id = f.add("still red");
    let c = f.run(&["next", "--claim"]);
    assert_eq!(c.code, 0, "control: claim must succeed: {}", c.both());
    assert!(
        c.stdout.contains(&id),
        "control: the still-RED ticket {id} must be handed out: {}",
        c.both()
    );
}
