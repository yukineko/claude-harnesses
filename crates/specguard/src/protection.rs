//! specguard's protection statement (backlog 3a8e3b73): what this gate
//! protects, from what, and on what grounds. Printed verbatim by
//! `specguard --protects`, and read out of this file by
//! `scripts/check-gate-protection.py`, which requires every field non-empty.
//!
//! Keep each field one ordinary string literal (`\` line continuations are
//! fine): the checker parses the literal without building the crate and
//! reports anything else as undetermined. `grounds` must cite something that
//! can be checked — code or a measured incident — never a promise.
//!
//! Nothing turns a specguard exit code into a block today. The statement says
//! so first; wiring one (e.g. `map gate-check` into a push hook) must update it.
//! This statement covers the `specguard` binary only, not `specforge`.

use harness_core::gate::Protection;

pub const PROTECTION: Protection = Protection {
    protects: "NOTHING IS BLOCKED AUTOMATICALLY: no hook, git hook or script \
        turns a specguard exit code into a block. Its one plugin hook, \
        SessionStart `specguard pending 2>/dev/null || true` \
        (hooks/hooks.json), discards stderr and the exit code, and the \
        launcher says of its entrypoints 'None of these block' \
        (bin/specguard). What it protects, by reporting to a human: agreement \
        between a project's canonical spec docs (the canon globs in \
        specguard.toml) and its implementation, so drift is surfaced instead \
        of accumulating unseen.",
    against: "Spec drift found by an audit someone runs (`/specguard:run`, \
        `specguard run`, or `prompt` + `ingest`; condukt's SKILL.md runs \
        prompt/ingest after a gate PASS as prose, with `|| true` and 'spec-drift \
        findings は condukt 完了を阻害しない'). A needs_user finding raises a \
        sentinel, and the next session's SessionStart prints a fix offer \
        (src/main.rs render_pending). A config that is present but \
        unloadable, or a sentinel whose state cannot be read, prints an \
        UNKNOWN notice instead of nothing (src/main.rs pending, \
        render_pending). Exactly two cases are silent: no config file at all \
        (src/main.rs pending, NotFound) and a loaded config with no sentinel \
        raised (render_pending, Known(false)). Exit codes a \
        direct caller may branch on, none wired to a block: run/ingest 3 (no \
        marker), 4 (agent failed), 5 (prompt unratified); ack 6 (no fix \
        commit since the sentinel) and 9; testaudit 7 and 8; brief 10 \
        (undetermined; /flow's preflight prose in \
        crates/flow/skills/flow/SKILL.md treats not-covered as a divert \
        candidate put to `condukt policy answer` and undetermined as no \
        divert); `map gate-check` 1 (a changed gate-crate entry with neither \
        spec_doc nor ack) and 2, documented as a 'Push-time spec-doc gate' \
        (src/main.rs GateCheck) but invoked by neither .githooks/pre-push nor \
        any script. It never stops a tool call, a turn, a stop, a commit or a \
        push, and emits no Stop-hook decision.",
    grounds: "Rationale: README.md 'A CLI that has an LLM agent audit, \
        **read-only**, whether an implementation has drifted from its \
        *canonical spec*'. Measured for the SessionStart surface: before \
        d8036188 (backlog f49e4a72) a present-but-unloadable config made \
        `specguard pending` print nothing, the same stdout as 'no drift \
        pending'; tests/pending_undetermined.rs (independent RED writer, \
        f35fe72e) observed it RED. The unreadable-sentinel case is pinned by \
        tests/faultinject_sentinel.rs \
        unsearchable_sentinel_dir_silences_the_pending_offer.",
};
