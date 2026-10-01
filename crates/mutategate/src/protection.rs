//! mutategate's protection statement (backlog 3a8e3b73): what this gate
//! protects, from what, and on what grounds. Printed verbatim by
//! `mutategate --protects`, and read out of this file by
//! `scripts/check-gate-protection.py`, which requires every field non-empty.
//!
//! Keep each field one ordinary string literal (`\` line continuations are
//! fine): the checker parses the literal without building the crate and
//! reports anything else as undetermined. `grounds` must cite something that
//! can be checked — code or a measured incident — never a promise.
//!
//! mutategate has no automatic trigger. The statement must say so first:
//! describing the kill-rate floor without that would read as if it were being
//! enforced. Wiring a trigger must update this statement.

use harness_core::gate::Protection;

pub const PROTECTION: Protection = Protection {
    protects: "INERT: nothing runs this gate automatically against a real \
        pilot crate, so today it protects nothing until a human runs it by \
        hand. (scripts/test_mutation_gate_status.sh does run \
        scripts/mutation-gate.sh, but under a fake `cargo` PATH shim, to test \
        the script's own exit-status handling; no mutant is generated.) It ships no plugin.json \
        or hooks (Cargo.toml: 'NOT a distributed Claude Code plugin'); its only \
        trigger, .github/workflows/mutation.yml, was deleted in a572f5ad \
        (GitHub Actions ban) and nothing replaced it (README.md 'TRIGGER: \
        none'; scripts/mutation-gate.sh 'NO AUTOMATIC TRIGGER'). When run by \
        hand it protects the fault-detection power of ONE pilot crate's test \
        suite: whether those tests would fail if a small fault were injected.",
    against: "When run by hand (scripts/mutation-gate.sh, or `mutategate \
        --outcomes <outcomes.json>`): a test suite that is green but too weak \
        to catch injected faults. It exits 1 when kill-rate = (caught + \
        timeout) / (caught + missed + timeout + unknown) is below \
        --min-kill-rate (default 0.80), or when there are no viable mutants \
        (src/lib.rs evaluate, MutationSummary::viable and killed; src/main.rs \
        main). It exits 2 on an unreadable or unparseable outcomes.json, or a \
        --min-kill-rate <= 1e-9 (src/lib.rs validate_min_kill_rate). \
        scripts/mutation-gate.sh exits 2 when cargo-mutants exits anything \
        other than 0 or 2, or writes no outcomes.json. One pilot per run \
        (harness-core by default; specguard, condukt and blastguard narrowed \
        to one file each by the script's `case \"$PILOT\"` block), so a run \
        says nothing about any other crate or file. A FAIL is also appended \
        to the overwatch violation ledger, fail-soft: a failed write is \
        printed but never changes the exit code (src/main.rs emit_violation). \
        No exit code of it reaches a Claude Code turn, a commit or a push.",
    grounds: "Rationale: README.md 'Golden/regression tests prove the code \
        *still does what it did*. They say nothing about whether the tests \
        would **catch a fault**'. Measured, 2026-09-11 at 89b31bb4 \
        (scripts/mutation-pilots.sh header, cargo-mutants, one file each, \
        threshold 0.80): stuckguard src/detect.rs 78.8% and propguard \
        src/config.rs 48.7% are below the bar; overwatch \
        src/canary.rs was unmeasurable (baseline failure). Counting unknown \
        states as survivors is grounded in CA-mutategate-01 (src/lib.rs \
        MutationSummary::unknown doc): an unrecognised state dropped from the \
        denominator would inflate the rate.",
};
