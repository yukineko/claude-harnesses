//! propguard's protection statement (backlog 3a8e3b73): what this gate
//! protects, from what, and on what grounds. Printed verbatim by
//! `propguard --protects`, and read out of this file by
//! `scripts/check-gate-protection.py`, which requires every field non-empty.
//!
//! Keep each field one ordinary string literal (`\` line continuations are
//! fine): the checker parses the literal without building the crate and
//! reports anything else as undetermined. `grounds` must cite something that
//! can be checked — code or a recorded incident — never a promise.

use harness_core::gate::Protection;

pub const PROTECTION: Protection = Protection {
    protects: "The meaning of a stopped turn as 'done': that the uncommitted \
        changes in this work tree (git::changed_files) were checked against the \
        semantic properties derived from the task's done_criteria (derive.rs: \
        error-path, output-schema, determinism, idempotence, \
        bounds-monotonicity, no-partial-write) before the agent is allowed to \
        stop.",
    against: "An agent declaring done on code that concrete tests pass but \
        whose invariants were never checked, or that fails them: fewer than \
        threshold properties satisfied (gate::below_threshold); and every path \
        on which propguard cannot tell - a checker that crashes, times out, \
        exits non-zero or prints unparseable output, a diff or git scan that \
        cannot be read, a truncated diff, a config or criteria file that \
        exists but cannot be read, a gate panic - which it blocks rather than \
        allows. The blocks are bounded differently: below-threshold, checker, \
        truncated-diff, git-scan and diff-read blocks give up to allow after \
        max_attempts (a systemic checker outage turns that give-up back into a \
        block); config-unreadable and criteria-unreadable have no give-up; a \
        gate panic blocks once and allows on the consecutive retry \
        (stop_hook_active). Any block is escapable via a session-scoped \
        `propguard skip --reason`.",
    grounds: "The threat is propguard's own recorded fail-opens, since fixed: \
        1a09f3ff (CA-propguard-01/02) - a failing \
        subprocess check cached its diff hash so the next identical unfixed \
        stop was auto-allowed, and an unanchored substring match let another \
        property's PASS text override a real FAIL; 275a4a20 - a checker that \
        exited non-zero after writing a 'PROP <id>: PASS' line had that \
        partial stdout trusted as a verdict; a231146a - a checker-error \
        give-up always allowed, silently shipping unverified code; backlog \
        87dbfbb8 (v0.1.38) - a tracked file whose content was not valid UTF-8 \
        gave an empty diff_text and the gate returned ALLOW tag=empty-diff, measured \
        with the real git. The property-over-tests premise itself is \
        rationale, not an incident: README.ja.md (PGS, arXiv:2506.18315).",
};
