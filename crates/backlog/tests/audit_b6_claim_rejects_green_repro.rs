#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Audit B item 6 (memory `backlog-evidence-rules`, "RED tickets are often
//! already fixed"). A ticket is filed only when its repro is RED at add time,
//! but nothing re-observes that RED before the ticket is handed to a worker.
//! Requirement: `next --claim` re-runs the stored repro and does NOT hand out
//! a ticket whose repro now passes.
//!
//! Drives the real built binary (common::Fixture: isolated HOME, linked git
//! worktree, committed repro scripts).

mod common;

use common::Fixture;

#[test]
fn next_claim_does_not_hand_out_a_ticket_whose_repro_went_green() {
    let f = Fixture::new("auditb6");
    // A repro whose verdict follows a committed file: RED while state=broken.
    f.write("src/state.txt", "broken\n");
    f.write_script(
        "tests/repro_flip.sh",
        "grep -q '^fixed$' src/state.txt && { echo 'no bug'; exit 0; }\necho 'bug present'; exit 1",
    );
    f.commit_paths(&["src/state.txt", "tests/repro_flip.sh"], "flip repro");

    // Filed FIRST, so plain (priority, created_at) ranking would pick it.
    let p = f.project();
    let o = f.run(&[
        "add",
        "--title",
        "fixed since filing",
        "--project",
        &p,
        "--repro-test",
        "bash tests/repro_flip.sh",
    ]);
    assert_eq!(
        o.code,
        0,
        "control: add must file the RED ticket: {}",
        o.both()
    );
    let went_green = common::parse_added(&o.stdout);
    let still_red = f.add("still red");

    // The defect is fixed at HEAD; the ticket's stored repro now passes.
    f.write("src/state.txt", "fixed\n");
    f.commit_paths(&["src/state.txt"], "fix");

    let c1 = f.run(&["next", "--claim"]);
    assert_eq!(c1.code, 0, "next --claim must succeed: {}", c1.both());
    assert!(
        !c1.stdout.contains(&went_green),
        "next --claim handed out {went_green}, whose stored repro passes at HEAD: {}",
        c1.both()
    );
    // CONTROL: a ticket whose repro is still RED is handed out.
    assert!(
        c1.stdout.contains(&still_red),
        "control: the still-RED ticket {still_red} must be handed out: {}",
        c1.both()
    );
}
