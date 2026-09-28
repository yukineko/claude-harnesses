//! Pins the `/flow` SKILL's queue-driving contract as *text*.
//!
//! Lived in `crates/flow/tests/` until 2026-08-20, when `flow` became a
//! skills-only plugin and stopped being a Rust package.
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

/// `crates/flow/skills/flow/SKILL.md`, resolved from THIS crate's manifest dir.
///
/// The test used to live in `crates/flow` and read `skills/flow/SKILL.md`
/// relative to that package. `flow` is a skills-only plugin since 2026-08-20 (its
/// binary was the retired `propose` nudge and nothing else), so there is no
/// `crates/flow` package to host a test any more — but the contract this pins is
/// about the skill TEXT, not the binary, so it survives here instead of being
/// deleted with the package.
fn skill() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("flow")
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

/// Every `condukt state claim-task` invocation in the fenced code blocks must
/// include the `--stateless` flag (backlog 9b7cb342). `/flow` claims under a
/// synthetic run id (`flow-<session>`) and never runs `condukt state init`, so
/// no run-state JSON exists for that run. An UNMARKED claim whose run state is
/// missing is kept forever (missing run state is "cannot determine", because
/// claims are repo-wide while run state is per-worktree), so a dead `/flow`
/// session's claims would never be reaped and would starve the queue. The
/// marker makes condukt judge the claim on the owning session's transcript
/// alone. This pins the skill TEXT only; the reaping behaviour itself is tested
/// in condukt's `claim::tests::stateless_*`.
#[test]
fn every_claim_task_invocation_is_marked_stateless() {
    let md = skill();
    let fenced = fenced_lines(&md);

    // Collect lines containing 'condukt state claim-task' and handle backslash
    // continuations to form complete commands.
    let mut invocations = Vec::new();
    let mut current_cmd = String::new();

    for line in fenced {
        // If we're continuing from a previous line, add a space separator
        if !current_cmd.is_empty() {
            current_cmd.push(' ');
        }

        // Check if this line ends with backslash (line continuation)
        if line.ends_with('\\') {
            // Remove the trailing backslash and add to accumulator
            current_cmd.push_str(&line[..line.len() - 1]);
        } else {
            // No continuation; this completes the command (or is a single line)
            current_cmd.push_str(&line);

            // If the completed command contains the invocation, record it
            if current_cmd.contains("condukt state claim-task") {
                invocations.push(current_cmd.clone());
            }

            current_cmd.clear();
        }
    }

    // If there's a leftover command being built (shouldn't happen in well-formed
    // markdown, but handle it anyway), check it too.
    if !current_cmd.is_empty() && current_cmd.contains("condukt state claim-task") {
        invocations.push(current_cmd);
    }

    // Assert we found at least one invocation (if zero, the test data is invalid).
    assert!(
        !invocations.is_empty(),
        "expected to find at least one 'condukt state claim-task' invocation in \
         the fenced code blocks of SKILL.md, but found none. If this is \
         correct, the contract test itself may need updating."
    );

    // Assert every invocation has the --stateless flag.
    for invocation in &invocations {
        assert!(
            invocation.contains("--stateless"),
            "every 'condukt state claim-task' invocation must include the \
             '--stateless' flag: /flow has no condukt run state, so an \
             unmarked claim can never be reaped after its session dies \
             (backlog 9b7cb342).\n\
             Offending invocation: {invocation}"
        );
    }
}
