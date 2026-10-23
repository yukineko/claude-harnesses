Do not read this crate's existence as coverage. Treat it as scoring machinery, not as coverage.

Never trigger the gate from hosted CI: any replacement trigger must be local per §7 (pre-push or an explicit script).

No git hook under `.githooks/` references `scripts/mutation-gate.sh` and there is no `.github/workflows/` directory, so the kill-rate gate is INERT: it runs only when a human invokes it.

Internal tooling — not a distributed Claude Code plugin (no `plugin.json`).

See Meta's Automated Compliance Hardening (ACH) and PRIMG (arXiv:2505.05584) for background.

The gate parses `cargo-mutants`'s `outcomes.json`, computes the kill-rate, and exits non-zero when it is below the threshold.

Kill-rate definition used here:

```
viable   = caught + missed + timeout + unknown   (unviable mutants excluded — no signal)
killed   = caught + timeout                      (a timeout is a test-exposed misbehaviour)
kill_rate = killed / viable                      (undefined -> gate fails)
```

`unknown` is a mutant whose `summary` state this crate does not recognise, or a record with no `summary`.

A record whose `scenario` is neither `"Baseline"` nor a `Mutant` object (null or absent included) is also counted as `unknown`.

It counts toward `viable` but not toward `killed`, so an unrecognised state is scored as a survivor instead of vanishing from the denominator and inflating the rate.

A run with any `unknown` also carries a warning in its reason.

```sh
# Gate an existing outcomes.json
cargo run -p mutategate -- --outcomes mutants.out/outcomes.json --min-kill-rate 0.80

# End-to-end: run the engine on the pilot crate, then gate
scripts/mutation-gate.sh
PILOT=difflog MIN_KILL_RATE=0.70 scripts/mutation-gate.sh
PILOT=specguard scripts/mutation-gate.sh
PILOT=condukt scripts/mutation-gate.sh
PILOT=blastguard scripts/mutation-gate.sh

# Show what this gate protects, from what, and on what grounds, without a verdict
cargo run -p mutategate -- --protects
```

Defaults: `--outcomes mutants.out/outcomes.json`, `--min-kill-rate 0.80`.

A `--min-kill-rate` at or below `1e-9` (zero, negative) is rejected with exit `2`.

Exit codes: `0` pass, `1` kill-rate below threshold (or no viable mutants), `2` usage/IO/parse error.

On a FAIL the gate also appends a violation event to overwatch's fleet violation ledger.

If that write fails, a note is printed and the FAIL verdict still stands.

`scripts/mutation-pilots.sh` maps a list of changed paths (stdin) to the calibrated pilot crate(s) to run, falling back to `harness-core` so it never prints an empty list.

- Pilot 1: `harness-core` (default).
- Pilot 2: `specguard` — narrowed by default to `--file crates/specguard/src/similarity.rs`, and by default skips the one test `ack_blocks_when_no_new_commits_since_raised`.
- Pilot 3: `condukt` — narrowed by default to `--file crates/condukt/src/circuit.rs`.
- Pilot 4: `blastguard` — narrowed by default to `--file crates/blastguard/src/classify.rs`.
- Override the pilot with `PILOT=<crate>`. `MUTANTS_EXTRA` (for example `MUTANTS_EXTRA="--file crates/harness-core/src/hash.rs"`) is passed to cargo-mutants verbatim, and the script runs cargo-mutants from the repo root whatever the caller's directory. A non-empty `MUTANTS_EXTRA` overrides the per-pilot default entirely.

 Of the GATE crates measured alongside blastguard, stuckguard and propguard scored below the bar and overwatch could not be measured (baseline failure); none of the three was made a pilot.

Add crates one at a time, only once each already clears the threshold.

Raise `MIN_KILL_RATE` as suites harden.

The script passes `--output target/mutants-<pilot>` to cargo-mutants and scores `target/mutants-<pilot>/mutants.out/outcomes.json`.

 At that revision nothing executed `scripts/mutation-gate.sh`; the kill-rate gate ran only when a human invoked it by hand.
