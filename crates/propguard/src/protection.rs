//! propguard's protection statement (backlog 3a8e3b73): what this gate
//! protects, from what, and on what grounds. Printed verbatim by
//! `propguard --protects`, and read out of this file by
//! `scripts/check-gate-protection.py`, which requires every field non-empty.
//!
//! Keep each field one ordinary string literal (`\` line continuations are
//! fine): the checker parses the literal without building the crate and
//! reports anything else as undetermined. `grounds` must cite something that
//! can be checked — code or a measured incident — never a promise.

use harness_core::gate::Protection;

pub const PROTECTION: Protection = Protection {
    protects: "The truth of an agent's 'done' for the current task: the code \
        changed in the working tree (unstaged, staged and untracked files \
        matching include/exclude) held against 3-5 semantic invariants \
        (error-path, output-schema, determinism, idempotence, \
        bounds-monotonicity, no-partial-write) derived deterministically from \
        that task's done_criteria (src/derive.rs derive_properties).",
    against: "A Stop that would end the turn while fewer than `threshold` of \
        those properties hold (hooks/hooks.json runs `propguard check` on Stop \
        only; src/gate.rs below_threshold). In the default inject mode nothing \
        is judged by propguard: a new (diff, properties) pair blocks once to \
        inject the checklist for the same running agent to self-verify, and \
        the identical pair is then allowed (already_verified). In subprocess \
        mode an independent checker_cmd must report PROP <id>: PASS lines. It \
        also blocks when it cannot determine: an existing but unreadable or \
        unparseable config (config-unreadable), an unknown mode \
        (config-invalid), an existing but unreadable criteria_file \
        (criteria-unreadable), a failed git scan or diff read, a truncated \
        diff, a failed checker, and a panic in the gate (run_guarded, first \
        stop only). Limits: with no done_criteria source, no git repo or no \
        checkable change it allows; below-threshold, checker, git-scan, \
        diff-read and truncation blocks give up and allow after max_attempts \
        (default 2), the outage give-ups turning back into a block only when \
        overwatch's violation ledger shows them recurring; `propguard skip \
        --reason` and PROPGUARD_DISABLE=1 allow.",
    grounds: "Rationale, not a recorded incident of a shipped invariant \
        violation: README.md - '`tdd` runs specific test cases; that proves \
        examples but never *formalizes* the semantic invariants the code must \
        satisfy' (following PGS, arXiv:2506.18315). The block-when-undetermined \
        half is grounded in measured defects of propguard itself: commit \
        29d08f54 (Continuous-Audit 2026W39, CA-propguard-01/03) - an existing \
        but unreadable criteria_file was allowed as no-criteria, and an \
        unreadable or unparseable config silently ran on built-in defaults; \
        both now block (src/gate.rs evaluate, tags criteria-unreadable and \
        config-unreadable).",
};
