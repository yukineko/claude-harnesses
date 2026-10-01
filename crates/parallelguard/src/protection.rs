//! parallelguard's protection statement (backlog 3a8e3b73): what this gate
//! protects, from what, and on what grounds. Printed verbatim by
//! `parallelguard --protects`, and read out of this file by
//! `scripts/check-gate-protection.py`, which requires every field non-empty.
//!
//! Keep each field one ordinary string literal (`\` line continuations are
//! fine): the checker parses the literal without building the crate and
//! reports anything else as undetermined. `grounds` must cite something that
//! can be checked — code or a measured incident — never a promise.
//!
//! parallelguard's enablement is parked by user ruling
//! (`scripts/parked-plugins.json`). The statement must say so: describing the
//! cap without that would read as if it were being enforced.

use harness_core::gate::Protection;

pub const PROTECTION: Protection = Protection {
    protects: "NOT ACTIVE: enablement parked by user ruling 2026-08-28 \
        (scripts/parked-plugins.json). While parked it is not \
        enabled in ~/.claude/settings.json, so none of its hooks fire and \
        nothing is capped; unparking must update this statement. When enabled, it would protect the host machine's \
        responsiveness within one Claude Code session: the number of Bash \
        shells and of Task/Agent subagents actually in flight at once (two \
        separate pools, 3 each; HARNESS_MAX_PARALLEL may lower the cap, never \
        raise it).",
    against: "A fan-out wide enough to overload the host - a batch of \
        parallel Bash calls, or a skill spawning one subagent per shard. When \
        enabled, its PreToolUse hook on Bash|Task|Agent (hooks/hooks.json, \
        `parallelguard acquire`) denies the call that would exceed the cap \
        (src/model.rs Inflight::acquire, live >= cap), and also denies when \
        the count cannot be established: an unparseable payload, a ledger \
        lock or read failure, a failed ledger write, a panic (src/main.rs \
        decide, cmd_acquire), or a missing platform binary (bin/parallelguard \
        launcher). PostToolUse releases the slot; SessionStart, \
        UserPromptSubmit and Stop clear the ledger. It does not bound Bash run \
        with run_in_background: true, whose slot is released as soon as the \
        call returns.",
    grounds: "Rationale, not a recorded incident: src/main.rs module doc - \
        'Freezing this machine takes one thing: too many processes at once', \
        and before this gate the only bound was a sentence in a SKILL.md \
        asking the model to send at most N. Commit b4b13b6f records the gap it \
        closed: the earlier cap (483a0d6e) was a planning-side cap that never \
        counted the Bash calls and subagents actually in flight. \
        Deny-when-undetermined is grounded in src/main.rs cmd_acquire: a \
        PreToolUse hook that exits 0 with no output, or panics (exit 101, a \
        non-blocking hook error), is an allow.",
};
