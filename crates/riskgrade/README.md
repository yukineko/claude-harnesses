# riskgrade

Graded risk / reversibility of a *planned* action (`classify`) and diff-level
semantic risk signals (`diffrisk`). A pure library (no I/O) linked into condukt;
not a distributed plugin (no `.claude-plugin/plugin.json`).

Moved out of blastguard on 2026-11-17 (user ruling: blastguard holds the
command-safety verdict only). The local-destruction tier still reuses
blastguard's detector (`blastguard::detect::detect`), so the classifier's
High/irreversible set cannot drift from the hook's deny set. Spec:
`docs/specs/riskgrade.md`.

## What it does

condukt's scheduler uses the graded
`classify::classify` risk/reversibility assessor to force outward, irreversible
actions (a deploy, `git push`, a release) through its GATED gate even when an
upstream LLM mislabelled the task.
A deploy word that appears only inside a file name in the task text (for example
`rollout` in `scripts/check-plugin-rollout.py`) does not force the gate.
A file-path token is a word containing `/`, or one ending in a known extension
such as `.py`/`.rs`/`.sh`/`.md`/`.toml`, with no `:` or `@`.
A path still counts when it comes first, follows a runner word (`run`, `bash`,
...) or shell separator, or starts with `./`.
A path quoted in backticks still counts.
Task prose is never parsed as a command: `classify` reads prose with the deploy
and history needles only, so prose that merely describes a command (for example
quotes `git config core.hooksPath` in backticks) is not force-gated.
The commands a task will actually run are passed separately and go through the
full `classify_command` analysis, so the same command passed as a command still
gates.
`classify_change` with no declared command list (`None`) is undetermined, never
Low.

A diff-level module is exported as well (pure, no I/O): `diffrisk`
(`classify_diff` / `changes_public_symbol`). The caller enumeration it is paired
with (`changed_symbol_names` / `enumerate_callers`) lives in
`harness_core::callgraph`. condukt consumes
both (`crates/condukt/src/diffrisk_record.rs`).
