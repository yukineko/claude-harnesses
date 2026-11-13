#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (audit 2026-11-13 K1): `backlog next --claim` refuses the WHOLE
//! queue when ONE candidate's ticket `project` path is foreign/nonexistent
//! (e.g. a label recorded on another machine). `claim_recheck::claim_rechecked`
//! calls `rerun_dir(&cand)` for the top candidate and returns the
//! "REFUSED to claim" error on its `Err`, instead of skipping that candidate
//! and handing out the next valid one. Same class as the plain-`next`
//! regression fixed by 97d5f45c (a04b2b87), which only covered plain `next`.
//!
//! Fixture: isolated HOME + linked-worktree git repo (tests/common). X (p0)
//! carries project `/some/old/root` (does not exist); Y (p2) is a valid ticket
//! of this repo. Measurement point: 09dd0c8c, 2026-11-13.
//!
//! Does NOT prove: that the foreign ticket must be skipped silently (only that
//! the valid ticket Y is claimed with exit 0 and X is not leased), nor the
//! pinned (multi-project) store path, nor what a foreign ticket whose project
//! DOES exist does.

mod common;

const FOREIGN: &str = "/some/old/root";

fn setup(tag: &str) -> (common::Fixture, String, String) {
    let fx = common::Fixture::new(tag);
    let mk = |title: &str, prio: &str| {
        let (o, id) = fx.add_extra(
            title,
            &["--priority", prio, "--repro-test", common::REPRO_YES],
        );
        id.unwrap_or_else(|| panic!("setup: `{title}` not filed: {}", o.both()))
    };
    let x = mk("alpha foreign project ticket", "p0");
    let y = mk("zulu local ticket", "p2");
    // `add` refuses a foreign --project, so a label recorded on another
    // machine is planted by rewriting X's `project` line in the store.
    let p = fx.tasks_path();
    let text = std::fs::read_to_string(&p).unwrap();
    let mut in_x = false;
    let mut replaced = false;
    let out: Vec<String> = text
        .lines()
        .map(|l| {
            if l.starts_with("[[task]]") {
                in_x = false;
            }
            if l.trim() == format!("id = \"{x}\"") {
                in_x = true;
            }
            if in_x && l.starts_with("project = ") && !replaced {
                replaced = true;
                return format!("project = \"{FOREIGN}\"");
            }
            l.to_string()
        })
        .collect();
    assert!(replaced, "setup: X's project line not found in:\n{text}");
    std::fs::write(&p, out.join("\n") + "\n").unwrap();
    (fx, x, y)
}

#[test]
fn control_plain_next_does_not_refuse_queue_for_foreign_project() {
    let (fx, x, y) = setup("k1-ctl");
    let o = fx.run(&["next"]);
    assert_eq!(
        o.code,
        0,
        "plain next must not refuse the queue: {}",
        o.both()
    );
    assert!(
        o.stdout.contains(&x) || o.stdout.contains(&y),
        "plain next handed out neither X ({x}) nor Y ({y}): {}",
        o.both()
    );
}

#[test]
fn next_claim_hands_out_valid_ticket_despite_foreign_project_on_another() {
    let (fx, x, y) = setup("k1-claim");
    let o = fx.run(&["next", "--claim"]);
    assert_eq!(
        o.code,
        0,
        "`next --claim` must claim the valid ticket Y ({y}) instead of refusing the whole queue \
         because X ({x}) has foreign project {FOREIGN}: {}",
        o.both()
    );
    assert!(
        o.stdout.contains(&y),
        "`next --claim` exited 0 but did not name Y ({y}): {}",
        o.both()
    );
    assert_eq!(fx.status(&y), "claimed", "Y must be the claimed task");
    assert_ne!(fx.status(&x), "claimed", "X must not be claimed");
}
