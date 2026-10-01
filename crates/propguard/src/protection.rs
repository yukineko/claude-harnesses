//! propguard's protection statement (backlog 3a8e3b73): what this gate
//! protects, from what, and on what grounds. Printed verbatim by
//! `propguard --protects`, and read out of this file by
//! `scripts/check-gate-protection.py`, which requires every field non-empty.
//!
//! Keep each field one ordinary string literal (`\` line continuations are
//! fine): the checker parses the literal without building the crate and
//! reports anything else as undetermined. `grounds` must cite something that
//! can be checked — code or a recorded incident — never a promise.
//!
//! Every claim below was checked against the code it cites; when the code
//! changes, change this text in the same commit (CLAUDE.md 第4節).

use harness_core::gate::Protection;

pub const PROTECTION: Protection = Protection {
    protects: "The meaning of a stopped turn as 'done': that the changes in \
        this work tree (tracked changes vs HEAD, staged changes and untracked \
        files, filtered by include/exclude; src/git.rs changed_files, \
        src/gate.rs checkable_files) were held against the semantic \
        properties derived deterministically from the task's done_criteria \
        (PROPGUARD_CRITERIA, else criteria_file, else inline done_criteria; \
        src/derive.rs source_criteria) - at most 5 of error-path, \
        output-schema, determinism, idempotence, bounds-monotonicity, \
        no-partial-write, 3-5 by default (src/derive.rs derive_properties, \
        src/config.rs sanitize) - before the agent is allowed to stop.",
    against: "A Stop that would end the turn while fewer than `threshold` of \
        those properties hold (hooks/hooks.json runs `propguard check` on Stop \
        only; src/gate.rs below_threshold). In the default inject mode \
        nothing is judged by propguard: a new (diff, properties) pair blocks \
        once to inject the checklist for the same running agent to \
        self-verify, and the identical pair is then allowed \
        (already_verified), so a property the agent leaves broken is not \
        detected. In subprocess mode an independent checker_cmd must report \
        `PROP <id>: PASS` lines. It also blocks when it cannot determine: a \
        checker that crashes, times out, exits non-zero (even after printing \
        PASS lines) or names none of the derived properties; a failed git \
        scan or diff read; a truncated diff; an existing but unreadable or \
        unparseable config (config-unreadable); an unknown mode \
        (config-invalid); an existing but unreadable criteria_file with no \
        non-empty inline done_criteria to fall back to (criteria-unreadable); \
        and a panic in the gate (run_guarded: it blocks once and allows on \
        the consecutive retry, stop_hook_active). Limits: with no \
        done_criteria source, no git repo, no checkable change, \
        PROPGUARD_DISABLE set (non-empty, not 0), `enabled = false` in \
        config, or a consumed session-scoped `propguard skip --reason` it \
        allows. Below-threshold, checker, git-scan, diff-read and truncation \
        blocks give up and allow after max_attempts (default 2); the \
        checker, git-scan and diff-read give-ups turn back into a block only \
        when overwatch's violation ledger shows the outage recurring \
        (checker-outage-systemic) or cannot be read back \
        (checker-outage-undetermined). config-unreadable, config-invalid and \
        criteria-unreadable have no max_attempts give-up. In addition, every \
        hook-mode block goes through harness_core::repeat::emit_stop_block \
        (operator ruling 2026-09-18): the second time propguard gives a \
        byte-identical reason in the same CLAUDE_CODE_SESSION_ID, it prints \
        only a systemMessage naming the unresolved finding and the stop \
        proceeds. That waiver applies to every tag above, the config and \
        criteria ones included; the block stays when the repeat ledger \
        cannot answer or the session id is unset, and the run_guarded panic \
        block is not waived.",
    grounds: "The block-when-undetermined half is grounded in propguard's own \
        recorded fail-opens, since fixed: 1a09f3ff (CA-propguard-01/02) - a \
        failing subprocess check cached its diff hash so the next identical \
        unfixed stop was auto-allowed, and an unanchored substring match let \
        another property's PASS text override a real FAIL; 275a4a20 - a \
        checker that exited non-zero after writing a 'PROP <id>: PASS' line \
        had that partial stdout trusted as a verdict; a231146a - a \
        checker-error give-up always allowed, silently shipping unverified \
        code; 9a2eb9f0 (backlog 87dbfbb8, v0.1.38) - a tracked file whose \
        content was not valid UTF-8 gave an empty diff_text and the gate \
        returned ALLOW tag=empty-diff, measured with the real git; 29d08f54 \
        (Continuous-Audit 2026W39, CA-propguard-01/03) - an existing but \
        unreadable criteria_file was allowed as no-criteria, and an \
        unreadable or unparseable config silently ran on built-in defaults. \
        The property-over-tests premise itself is rationale, not an incident \
        of a shipped invariant violation: README.md - '`tdd` runs specific \
        test cases; that proves examples but never *formalizes* the semantic \
        invariants the code must satisfy' (following PGS, arXiv:2506.18315).",
};
