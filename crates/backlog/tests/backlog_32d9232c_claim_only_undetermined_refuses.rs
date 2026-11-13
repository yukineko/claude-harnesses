#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (audit 2026-11-13 32d9232c): `backlog next --claim` with ONLY
//! undetermined candidate (single ticket in queue with foreign/nonexistent
//! project path) must REFUSE TO CLAIM the whole queue (non-zero exit, "REFUSED"
//! message in output, ticket NOT claimed, store status still pending).
//!
//! Invariant: when the only candidate is undetermined (cannot determine its
//! project identity or project root to re-run its repro), the claim is refused
//! non-zero with a "REFUSED" message. This is NOT a "no pending tasks" scenario
//! — the task is pending, but the claim cannot be fulfilled because the task's
//! information is insufficient. The task remains unclaimed and in pending status.
//!
//! The fix will make `claim_rechecked` skip candidates with undetermined `rerun_dir`
//! and consider the next one. When the ONLY candidate is undetermined, after that
//! skip there are no more candidates, so the loop exhausts all candidates and
//! must return an error REFUSED (not Ok(None) which would render "no pending tasks").
//!
//! Fixture: isolated HOME + linked-worktree git repo (tests/common). Single
//! ticket X (p0) carries project `/some/old/root` (does not exist, foreign,
//! undetermined). Measurement point: 2026-11-13.
//!
//! Does NOT prove: that the undetermined candidate must be silently skipped in
//! the future (only that the queue must refuse when only that candidate exists),
//! nor what happens when multiple candidates exist (one undetermined, others valid),
//! nor the correct behavior when project identity is undetermined for reasons other
//! than nonexistent path.

mod common;

const FOREIGN: &str = "/some/old/root";

fn setup(tag: &str) -> (common::Fixture, String) {
    let fx = common::Fixture::new(tag);
    let (o, id) = fx.add_extra(
        "sole ticket with foreign project",
        &["--priority", "p0", "--repro-test", common::REPRO_YES],
    );
    let id = id.unwrap_or_else(|| panic!("setup: ticket not filed: {}", o.both()));

    // Plant the foreign project path in the store (same technique as K1 test).
    let p = fx.tasks_path();
    let text = std::fs::read_to_string(&p).unwrap();
    let mut in_ticket = false;
    let mut replaced = false;
    let out: Vec<String> = text
        .lines()
        .map(|l| {
            if l.starts_with("[[task]]") {
                in_ticket = false;
            }
            if l.trim() == format!("id = \"{id}\"") {
                in_ticket = true;
            }
            if in_ticket && l.starts_with("project = ") && !replaced {
                replaced = true;
                return format!("project = \"{FOREIGN}\"");
            }
            l.to_string()
        })
        .collect();
    assert!(
        replaced,
        "setup: ticket's project line not found in:\n{text}"
    );
    std::fs::write(&p, out.join("\n") + "\n").unwrap();

    (fx, id)
}

#[test]
fn next_claim_with_only_undetermined_candidate_refuses() {
    let (fx, id) = setup("32d9232c");

    // Run `next --claim` against a queue containing ONLY an undetermined ticket.
    let o = fx.run(&["next", "--claim"]);

    // Must exit non-zero (REFUSED, not empty queue).
    assert_ne!(
        o.code,
        0,
        "`next --claim` with only undetermined candidate must refuse (exit non-zero): {}",
        o.both()
    );

    // Must not output the ticket id to stdout (no claim happened).
    assert!(
        !o.stdout.contains(&id),
        "`next --claim` refused but stdout contains the id {id}: {}",
        o.both()
    );

    // Must not print "no pending tasks" to stdout.
    // The task IS pending; the claim was refused due to insufficient info,
    // not because the queue is empty.
    assert!(
        !o.stdout.contains("no pending tasks"),
        "`next --claim` stdout must not say 'no pending tasks' when refusal is due to undetermined candidate: {}",
        o.both()
    );

    // Must contain "REFUSED" in the combined output (error message on stderr).
    let combined = o.both().to_lowercase();
    assert!(
        combined.contains("refused"),
        "`next --claim` combined output must contain 'REFUSED': {}",
        o.both()
    );

    // Must NOT have claimed the ticket.
    assert_ne!(
        fx.status(&id),
        "claimed",
        "the ticket must NOT be claimed when `next --claim` refused: status is {}",
        fx.status(&id)
    );

    // Store row must still have status "pending" (unchanged).
    assert_eq!(
        fx.status(&id),
        "pending",
        "a refused claim must leave the ticket status unchanged (still pending)"
    );
}
