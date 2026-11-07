#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Backlog 78cee053, companion of `audit_b6_claim_rejects_green_repro.rs`.
//! `next --claim` re-runs the top candidate's stored repro. User ruling
//! (2026-11-07, "Refuse the claim"): when that re-run CANNOT BE DETERMINED the
//! claim is refused: the ticket is NOT handed out, the exit is non-zero and the
//! reason is printed (CLAUDE.md §3: undetermined resolves to the restrictive side).
//!
//! The undetermined trigger is `evidence::preflight`: `run_repro` returns
//! `undetermined(e, Some(head))` when
//! `tracked_at(root, head, script)` is false ("script {script} is not committed
//! at HEAD ({head}); only a git-tracked, committed test script is evidence").
//! The script is committed when the ticket is filed (so `add` sees RED) and is
//! then removed in a later commit, so the tree stays clean and the only cause
//! is the missing committed script.

mod common;

use common::Fixture;

#[test]
#[ignore = "backlog 78cee053: RED, remove ignore when fixed"]
fn next_claim_refuses_a_ticket_whose_repro_rerun_is_undetermined() {
    let f = Fixture::new("b78cee053");
    f.write_script("tests/repro_gone.sh", "echo 'bug present'; exit 1");
    f.commit_paths(&["tests/repro_gone.sh"], "add repro");

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

    // The committed script disappears from HEAD: the re-run is undetermined.
    f.commit_rm("tests/repro_gone.sh", "drop repro");

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
        c.stderr.contains("not committed") || c.stdout.contains("not committed"),
        "the refusal must print the reason (script not committed at HEAD): {}",
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
