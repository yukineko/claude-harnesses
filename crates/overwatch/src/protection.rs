//! overwatch's protection statement (backlog 3a8e3b73): what this gate
//! protects, from what, and on what grounds. Printed verbatim by
//! `overwatch --protects`, and read out of this file by
//! `scripts/check-gate-protection.py`, which requires every field non-empty.
//!
//! Keep each field one ordinary string literal (`\` line continuations are
//! fine): the checker parses the literal without building the crate and
//! reports anything else as undetermined. `grounds` must cite something that
//! can be checked — code or a measured incident — never a promise.
//!
//! overwatch is not an in-turn gate: its hooks only print a status view. It
//! blocks through the consumers of its exit codes and output, and the one
//! refusal it issues directly to agents (`begin` exit 1) is not enforced by
//! anything. The statement says so rather than inheriting the word "gate".

use harness_core::gate::Protection;

pub const PROTECTION: Protection = Protection {
    protects: "NOT AN IN-TURN GATE: it never blocks a tool call, a turn or a \
        stop. Both of its hooks (SessionStart and Stop, hooks/hooks.json) run \
        `overwatch status`, which prints a view and returns Ok (src/render.rs \
        status); it emits no hook decision. What it protects, only through \
        consumers that act on its exit codes and output: (1) a plugin rollout, \
        when a human runs scripts/rollout-plugins.sh with a canary (required \
        for the GATE crates) - the stage is rolled back and the rollout \
        halted; (2) the shared main working tree, through condukt's main-tree \
        guard (crates/condukt/src/maintree.rs observe_peers, run by the opt-in \
        .githooks/pre-commit as `condukt guard main-tree`), which reads \
        `overwatch status --json` as one of its two liveness inputs; (3) \
        duplicate work across sessions, advisory only (see AGAINST).",
    against: "(1) A canary stage that makes gates misfire: `overwatch \
        canary-gate` exits 3 (src/main.rs CanaryGate arm) when violations \
        recorded since the stage's deploy exceed --threshold (rollout default \
        2; src/canary.rs decide_from_count uses '>'), when any fleet-recurring \
        signature appears (--systemic-threshold, rollout default 0), or when \
        the violation ledger cannot be read (src/canary_cli.rs \
        gate_from_registry, ViolationScan::Undetermined rolls back); \
        rollout-plugins.sh also halts (exit 5) on any canary-gate exit other \
        than 0 or 3. Limits: the check runs right after each stage's registry \
        repoint with no soak time (stage_deploy_ts is taken just before the \
        copy), so it counts only violations recorded in those seconds and does \
        not observe the new binaries in use; ROLLOUT_GATE_EVAL_FAILSOFT=1 and \
        --no-canary are explicit bypasses. (2) A commit from the shared main \
        tree while another session is live: overwatch supplies the lease \
        roster and reports an unreadable lease ledger as an `undetermined` \
        entry with source `sessions` instead of an empty roster, which the \
        guard resolves to a block (maintree.rs parse_overwatch_sessions); the \
        guard, not overwatch, blocks. (3) Two sessions working one key: \
        `overwatch begin` exits 1 with skip JSON when another live session \
        holds the key or the lease lock is contended (src/lease.rs begin). \
        Nothing enforces that exit: no code, hook or script in this repo \
        calls `begin` (a grep finds only a comment in crates/backlog/src/main.rs, \
        a blastguard test string and stuckguard's src/anchor.rs advice text); \
        its callers are prose read by an agent, and they disagree - \
        crates/overwatch/skills/overwatch/SKILL.md says 'exit 1 なら決して先に進んではならない', \
        while crates/flow/skills/flow/SKILL.md step 7 says only that a missing \
        binary or failed call is skipped ('呼び出し失敗時は skip して続行する') \
        and does not mention exit 1 (crates/flow/README.md also lists the \
        step). flow's own dedup is `condukt state claim-task`. It does not detect prompt injection, spec drift or weak \
        tests (crates/harness-core/src/fleet.rs: 'it is not itself a \
        prompt-injection/spec/mutation defense gate').",
    grounds: "Recorded incidents: (1) scripts/rollout-plugins.sh health-gate \
        comments - a gate-eval error used to fall through to PROCEED, 'a \
        GATE-crate rollout advancing every stage precisely because the health \
        check was broken and verified nothing', and 'a single-stage canary ran \
        it ZERO times'. (2) CLAUDE.md section 8, measured 2026-07-23: two \
        sessions shared main's working tree and index \
        ('触っているファイルは非衝突なのに、index が共有されているため分離できない'); \
        README.md: '`(none)` there is the claim \"no other session is live\"', \
        which is why an unreadable ledger is reported as unknown, not as \
        (none). (3) Rationale only, no recorded incident: README.md 'A \
        distributed multi-session system requires coordination.'",
};
