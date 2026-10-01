//! stuckguard's protection statement (backlog 3a8e3b73): what this gate
//! protects, from what, and on what grounds. Printed verbatim by
//! `stuckguard --protects`, and read out of this file by
//! `scripts/check-gate-protection.py`, which requires every field non-empty.
//!
//! Keep each field one ordinary string literal (`\` line continuations are
//! fine): the checker parses the literal without building the crate and
//! reports anything else as undetermined. `grounds` must cite something that
//! can be checked — code or a measured incident — never a promise.
//!
//! stuckguard is listed in `harness_core::fleet::BLOCKING_GATES`, but it does
//! not block anything: it only injects advice. The statement says so rather
//! than inheriting the list's wording.

use harness_core::gate::Protection;

pub const PROTECTION: Protection = Protection {
    protects: "The session's forward progress and the user's time: a turn \
        should not be spent repeating the same tool call, or undoing and \
        redoing the same edit, without converging.",
    against: "Stuck loops seen on PostToolUse of Bash, Edit, MultiEdit, Write, \
        Read, Grep and Glob (hooks/hooks.json runs `stuckguard watch`): the \
        same normalized (tool, input) repeat_threshold times in the recent \
        window, and edit oscillation where a file is changed X->Y then Y->X \
        oscillation_threshold times (src/detect.rs detect; oscillation \
        outranks repeat). The response is ADVISORY ONLY: it injects \
        additionalContext that escalates to 'stop and ask the user' after \
        escalate_after nudges, and it never blocks a tool call or ends a turn \
        (src/main.rs module doc; README.md 'It only ever **injects advice**. \
        It cannot block a tool call or end a turn'). An unreadable session \
        history also produces a nudge instead of a silent 'no loop' (src/main.rs \
        watch, Determination::Undetermined arm). Off by default: a \
        progress-score stall advisory and a scope-drift advisory against the \
        session's overwatch lease.",
    grounds: "Recorded incident: CLAUDE.md section 7 'Why' (2026-07-24) - \
        while waiting on CI an agent polled `gh run list` each time the \
        user asked whether it was done, and was 'stuckguard に同一操作の繰り返しとして検知された' (caught \
        by the repeat detector). Otherwise rationale: README.md 'Agents get \
        stuck: they rerun the same failing command, or edit a file back and \
        forth without converging.' The escalation counter is grounded in a \
        measured defect of stuckguard itself: commit 375e59e6 (CA-stuckguard-01) \
        - a slowly drifting near-repeat loop never escalated because its key \
        was recomputed from window content and the count reset; escalation \
        now runs on a persistent streak (src/main.rs watch, \
        record_repeat_run).",
};
