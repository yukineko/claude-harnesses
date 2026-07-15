# mutategate

A **mutation-testing kill-rate gate** for this workspace. Internal tooling — not a
distributed Claude Code plugin (no `plugin.json`).

## Why

Golden/regression tests prove the code *still does what it did*. They say nothing
about whether the tests would **catch a fault** if one were introduced. Mutation
testing closes that gap: it injects small faults ("mutants") and checks whether
the existing tests fail (mutant **caught/killed**) or still pass (mutant
**missed/survived**). The fraction of viable mutants killed is the **kill-rate**
(mutation score). A low score means the suite is weak no matter how green it looks.

Background: Meta's Automated Compliance Hardening (ACH) and PRIMG
(arXiv:2505.05584).

## What it is (and is not)

- It does **not** implement a mutation engine. It stands on the standard Rust tool
  [`cargo-mutants`](https://mutants.rs).
- It **is** the gate: parse `cargo-mutants`'s `outcomes.json` → compute kill-rate →
  exit non-zero when below a threshold. That parse→score→exit logic is pure and
  unit-tested (`cargo test -p mutategate`) on fixed sample JSON, so the pass/fail
  decision is deterministic and runs without the (slow) engine.

Kill-rate definition used here:

```
viable   = caught + missed + timeout      (unviable mutants excluded — no signal)
killed   = caught + timeout               (a timeout is a test-exposed misbehaviour)
kill_rate = killed / viable               (undefined -> gate fails)
```

## Usage

```sh
# Deterministic gate over an existing outcomes.json:
cargo run -p mutategate -- --outcomes mutants.out/outcomes.json --min-kill-rate 0.80

# End-to-end (runs the engine on the pilot crate, then gates):
scripts/mutation-gate.sh
PILOT=difflog MIN_KILL_RATE=0.70 scripts/mutation-gate.sh
PILOT=specguard scripts/mutation-gate.sh   # GATE crate: polarity gate (similarity.rs)
PILOT=condukt scripts/mutation-gate.sh     # non-GATE crate: circuit-breaker logic (circuit.rs)
```

Exit codes: `0` pass, `1` kill-rate below threshold (or no viable mutants), `2`
usage/IO/parse error.

## Scope (deliberately narrow)

Running `cargo-mutants` over the whole workspace is far too slow to gate on, so
the pilot is **one crate**:

- **Pilot 1: `harness-core`** (default) — shared build-time logic; `hash`/
  `pricing`/`spans` are pure and well-suited to mutation.
- **Pilot 2: `specguard`** — a GATE crate (one of blastguard/propguard/
  specguard/stuckguard/mutategate/overwatch; see `scripts/rollout-plugins.sh`)
  whose polarity check (`src/similarity.rs`) has been the source of real bugs
  found in review, so it is where kill-rate signal matters most. Narrowed by
  default to `--file crates/specguard/src/similarity.rs`.
- **Pilot 3: `condukt`** — the first NON-GATE crate added. `condukt` is the
  orchestrator binary and has the highest test density in the workspace (29/36
  source files carry tests), making it a good non-GATE candidate. It is far
  too large to mutate whole, so the default narrows to
  `--file crates/condukt/src/circuit.rs` — the pure circuit-breaker
  state-machine logic (no filesystem/process I/O), already covered by 20 unit
  tests.
- Override the pilot with `PILOT=<crate>`. Narrow further with
  `MUTANTS_EXTRA="--file src/hash.rs"` for a fast real run; an explicit
  `MUTANTS_EXTRA` overrides the per-pilot default entirely.

**Threshold: 0.80.** This mirrors the practical robustness bar of established
mutation tools (e.g. PIT) and the Meta ACH line of work; below it a suite is
demonstrably missing detectable faults. Kept conservative for the pilot so the
gate is signal, not flake.

## Expanding later

- Add crates one at a time only once each already clears the threshold, so a new
  crate cannot silently drag the gate down (the `condukt` pilot followed this:
  narrowed to `circuit.rs` and confirmed the threshold before being added to
  the `case "$PILOT"` block).
- Raise `MIN_KILL_RATE` as suites harden; inspect survivors under
  `target/mutants-<pilot>/` (`missed.txt`).
- CI: `.github/workflows/mutation.yml` runs the pilot on manual dispatch, a weekly
  schedule, and PRs touching the gate machinery — pilot-limited with a 30-minute
  job cap.
