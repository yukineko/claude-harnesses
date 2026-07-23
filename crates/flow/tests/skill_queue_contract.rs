//! Pins the `/flow` SKILL's queue-driving contract as *text*.
//!
//! # What this test does and does not prove
//!
//! `SKILL.md` is a prompt, not enforcement. **This test cannot prove that an LLM
//! reading it will behave correctly** — nothing in a text file can. The
//! guarantees that actually hold are in the binaries and are tested there:
//!
//! * two concurrent drivers get disjoint tasks — `backlog`'s
//!   `concurrent_drivers_claim_disjoint_tasks_and_none_is_refused` and
//!   `two_concurrent_driver_processes_get_disjoint_tasks`;
//! * liveness answers at 0/1/2+ drivers and reaps stale ones — `backlog`'s
//!   `driver` and `liveness` unit tests plus
//!   `lock_status_answers_liveness_for_zero_one_and_many_drivers`;
//! * the consumers read the new shape — `autoflow`'s and `daily`'s parser tests.
//!
//! What this test DOES prove is narrower and still worth having: that the
//! instruction which caused the monopoly cannot silently come back. The skill
//! told the driver to take an exclusive project-wide lock for the whole loop and
//! to pick with a pure read (`backlog list`); if either instruction reappears,
//! this goes red.

use std::path::PathBuf;

fn skill() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("skills")
        .join("flow")
        .join("SKILL.md");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn lines_containing(md: &str, needle: &str) -> Vec<String> {
    md.lines()
        .filter(|l| l.contains(needle))
        .map(|l| l.trim().to_string())
        .collect()
}

/// Lines inside ``` fences — the commands the driver is told to *run*, as
/// opposed to prose that merely discusses a command (e.g. the note that the
/// exclusive lock still exists as a deliberate human escape hatch).
fn fenced_lines(md: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in md.lines() {
        if line.trim_start().starts_with("```") {
            inside = !inside;
            continue;
        }
        if inside {
            out.push(line.trim().to_string());
        }
    }
    out
}

/// The core regression: the loop must not acquire the exclusive project lock.
/// `lock acquire` may still be *discussed* in prose (it remains a deliberate
/// human escape hatch, and `--force` belongs to it), but it must not appear as a
/// command the driver is told to run.
#[test]
fn the_loop_does_not_acquire_the_exclusive_project_lock() {
    let md = skill();
    for line in fenced_lines(&md) {
        assert!(
            !line.contains("backlog lock acquire"),
            "`/flow` must not take the project-wide exclusive lock to drive the \
             queue — that is what shut a second session out of the whole backlog. \
             Offending command: {line}"
        );
    }
    // The Step 2 fenced command block must register presence, not acquire.
    assert!(
        md.contains("backlog driver register --session-id"),
        "Step 2 must register non-exclusive driver presence"
    );
    assert!(
        md.contains("backlog driver unregister --session-id"),
        "Step 4 must release that registration"
    );
    assert!(
        md.contains("backlog driver heartbeat --session-id"),
        "the loop must heartbeat its registration, or it is reaped after the TTL \
         and autoflow/daily start driving the same queue"
    );
    assert!(
        !md.contains("backlog lock release --project \"$PWD\"\n```"),
        "the unconditional `lock release` step must be gone with the acquire"
    );
}

/// The pick must reserve, not merely read. `backlog list` is a documented pure
/// read: two concurrent drivers reading it are handed the same task, which is
/// precisely why the exclusive lock looked necessary.
#[test]
fn the_pick_reserves_the_task_instead_of_only_reading_it() {
    let md = skill();
    assert!(
        md.contains("backlog next --claim --project"),
        "the backlog pick must use `next --claim`"
    );
    for line in lines_containing(&md, "backlog list --status pending") {
        assert!(
            line.contains("禁止") || line.contains("いけない"),
            "`backlog list` must not be the way the driver picks work (it is a \
             pure read and hands concurrent drivers the same task). Offending \
             line: {line}"
        );
    }
}

/// The skill must not tell the driver to stand down merely because another
/// driver is registered — standing down is the monopoly, restated.
#[test]
fn a_peer_driver_is_not_a_reason_to_stand_down() {
    let md = skill();
    assert!(
        md.contains("他セッションが driver として登録済みでも、見送らない"),
        "the skill must state that a registered peer driver is not a reason to \
         stand down"
    );
    assert!(
        md.contains("待つのは解ではない"),
        "the skill must keep the 'waiting is not a solution' rule (CLAUDE.md §8)"
    );
}

/// The per-task cross-session guards are the real exclusivity mechanism now, so
/// they must stay in the skill.
#[test]
fn the_per_task_claim_guards_are_retained() {
    let md = skill();
    for needle in [
        "condukt state claim-task",
        "condukt state release-task",
        "condukt state heartbeat",
        "condukt state is-claimed",
    ] {
        assert!(
            md.contains(needle),
            "`{needle}` is now the TOCTOU guard and must not be dropped"
        );
    }
}

/// §3: the skill must not read an undetermined liveness answer as "no driver".
#[test]
fn undetermined_liveness_is_not_read_as_free() {
    let md = skill();
    assert!(
        md.contains("undetermined") && md.contains("「driver 不在」とは読まない"),
        "the skill must state that an undetermined liveness answer is not an \
         observation that nobody is driving"
    );
}
