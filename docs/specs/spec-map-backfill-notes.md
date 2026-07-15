# spec-map spec_doc backfill notes (backlog f55981f2)

## What this covers

Backlog `f55981f2` asked to backfill `spec_doc` for entries missing it in
`.specguard/spec-map.toml`, citing a breakdown of "45 missing (hooks 4 / server
script 20 / client core 1 / test file 8 / junk 11)".

## Discrepancy with the cited "45" figure

At the time this task was picked up, `specguard map list` against the
current `.specguard/spec-map.toml` showed only **9** entries with a missing
`spec_doc`, not 45. All 9 were already `status = "tracked"` (not `"changed"`),
and all 9 are `*.rs` integration test files:

- `crates/blastguard/tests/overwatch_violation.rs`
- `crates/condukt/tests/diffrisk_record_e2e.rs`
- `crates/mutategate/tests/overwatch_violation.rs`
- `crates/overwatch/tests/audit_round.rs`
- `crates/overwatch/tests/canary.rs`
- `crates/overwatch/tests/continuous_audit_script.rs`
- `crates/overwatch/tests/review_queue.rs`
- `crates/overwatch/tests/rollout_gate_crates.rs`
- `crates/overwatch/tests/violation.rs`

No entries matched a "hooks" (`crates/*/src/hooks/**`), "server script"
(`scripts/**` — actually excluded from the map entirely via `[map].exclude`),
or "client core" pattern with a missing `spec_doc`; the 45/4/20/1/8/11
breakdown does not reconcile against the live store. This gap between the
backlog's cited counts and the measured state is recorded here per the task's
`done_criteria` (exclusion/attribution rationale must live in a commit message
or docs) rather than silently reconciled — a follow-up should confirm whether
the "45" figure referred to a different/stale snapshot of the map, or to a
`map build`/`sync` run that hadn't landed yet.

## Resolution applied

All 9 missing entries are `*.rs` test files whose sibling test files (and
their crate's own `src/**` entries) already carried a `spec_doc` pointing at
the crate's canonical spec (`docs/specs/<crate>.md`). None were judged to be
junk (stale/removed/duplicate) — each corresponds to a real, currently-passing
integration test exercising a live feature (overwatch violation emission,
canary rollout gating, continuous-audit wiring, diffrisk recording). Per the
task's "test file 帰属方針" (test-file attribution policy), each was attributed
to its own crate's existing spec doc via `specguard map set-spec`:

| entry | spec_doc |
| --- | --- |
| `crates/blastguard/tests/overwatch_violation.rs` | `docs/specs/blastguard.md` |
| `crates/condukt/tests/diffrisk_record_e2e.rs` | `docs/specs/condukt.md` |
| `crates/mutategate/tests/overwatch_violation.rs` | `docs/specs/mutategate.md` |
| `crates/overwatch/tests/audit_round.rs` | `docs/specs/overwatch.md` |
| `crates/overwatch/tests/canary.rs` | `docs/specs/overwatch.md` |
| `crates/overwatch/tests/continuous_audit_script.rs` | `docs/specs/overwatch.md` |
| `crates/overwatch/tests/review_queue.rs` | `docs/specs/overwatch.md` |
| `crates/overwatch/tests/rollout_gate_crates.rs` | `docs/specs/overwatch.md` |
| `crates/overwatch/tests/violation.rs` | `docs/specs/overwatch.md` |

No entries were excluded as junk in this pass (0 of the measured 9 warranted
exclusion). `specguard map list` now shows 0 entries with a missing
`spec_doc`.
