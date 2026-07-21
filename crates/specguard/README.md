# specguard

> 🌐 **English** ・ [日本語](README.ja.md)

**Spec ↔ implementation drift audit harness (project-agnostic)**

A CLI that has an LLM agent audit, **read-only**, whether an implementation has
drifted from its *canonical spec*, and whether the canon docs themselves contain
silence, contradiction, or duplication. The judgment lives in the LLM (which
quotes the canon verbatim); specguard is the **deterministic harness** around it
— scope resolution → prompt rendering → agent launch → marker parsing →
report / sentinel. Everything project-specific is externalized to a TOML config.

```
specguard.toml ──┐
   git diff ──────┼──▶ scope (changed areas ∪ invariants)
                  │         │
templates/ ───────┼──▶ render prompt ──▶ agent (read-only) ──▶ parse markers ──▶ report + sentinel
```

There are two ways to run it, both sharing **the same `specguard` binary**:

| | standalone binary | Claude Code plugin |
|---|---|---|
| Audit engine | spawns `claude --print` per shard | in-session read-only subagent (no nested claude) |
| Billing | depends on the claude CLI login | **the host session's subscription** |
| read-only enforcement | `claude --print` **Bash-arg allowlist** (strong) | subagent **tool-name** restriction (weaker) |
| Entry point | `specguard run` (cron, etc.) | `/specguard:run` (interactive / HOTL) |

→ For specguard's own verification-gate design see
**[DESIGN-VERIFY.md](DESIGN-VERIFY.md)** (Japanese); the canon for the audit
policy (classification, verdict vocabulary, discipline) is
`templates/audit-prompt.md`. **[DESIGN.md](DESIGN.md)** (+
[DESIGN-INTAKE.md](DESIGN-INTAKE.md)) documents `specforge`, the
generation-side sibling harness that lives in this same crate
(`src/forge/`, binary `specforge`) and calls specguard back as its
read-only accept-gate; it ships no `/specforge:*` slash commands yet, so it
is not part of this plugin's surface below.

---

## Getting started

### Prerequisites

- A Rust toolchain (`cargo`). Get it from https://rustup.rs.
- The audit target must be a **git repository**.
- Either way the target repo needs a `specguard.toml` (scaffolded below).

### 1. Install the binary (prerequisite for both modes)

```sh
./install.sh                                  # release build → ~/.local/bin
SPECGUARD_BIN_DIR=/usr/local/bin ./install.sh # to change the install dir
```

Manually, `cargo build --release` produces `target/release/specguard`. Make sure
`~/.local/bin` is on your PATH. Details and troubleshooting are in
**[INSTALL.md](INSTALL.md)**.

### 2. Scaffold into the target repo

```sh
cd /path/to/your/repo
specguard init        # generates specguard.toml + a SessionStart hook (idempotent)
```

`init` will not overwrite an existing `specguard.toml` without `--force`, and it
appends only the SessionStart hook (which surfaces unhandled drift) without
disturbing other settings in `.claude/settings.json`. **In plugin mode the hook
is bundled**, so you only need the config (`cp specguard.example.toml specguard.toml`
also works).

### 3a. Use it standalone

```sh
cd /path/to/your/repo
# edit specguard.toml's [[area]] / [[invariant]] / canon for your repo
specguard run                                 # run the audit
```

Run `specguard run` from cron / a task scheduler, and let the SessionStart hook
pick up the sentinel raised when `needs_user=yes`, prompting a human — a
Human-on-the-loop loop.

### 3b. Use it as a Claude Code plugin (subscription-native)

This repository *is* the plugin. Instead of spawning `claude --print`, it
delegates each shard to an in-session read-only subagent (`specguard-auditor`),
auditing on the host session's subscription. The deterministic harness is still
delegated to the same `specguard` binary (no duplicated judgment logic).

```sh
cd /path/to/your/repo
claude --plugin-dir /path/to/specguard        # load it for this session
# after edits: /reload-plugins; inspect with /plugin
```

Or install via the marketplace (persists across sessions):

```text
/plugin marketplace add yukineko/specguard    # register the marketplace (GitHub)
/plugin install specguard@specguard           # install the plugin
```

In **standalone mode** the `specguard` binary must be on PATH (`./install.sh`).
In **plugin mode** the pre-compiled binary is bundled under `bin/` and the hook
uses `${CLAUDE_PLUGIN_ROOT}/bin/specguard` — no PATH setup required.
**macOS / Linux / WSL2** are supported (the binary is Rust, the hook/commands are bash).

```
/specguard:run
  └─ specguard prompt --json    (harness: resolve scope + render shards)
  └─ Task(specguard-auditor) × shard   (judgment: read-only subagent / subscription)
  └─ specguard ingest --from …  (harness: parse → verify → report → sentinel/baseline)
```

---

## Usage

Once a human handles a `needs_user=yes` finding, clear the sentinel (otherwise
the SessionStart hook keeps nagging about the same issue).

**`ack` fix-commit gate**: `specguard ack` records the git HEAD when the sentinel
is raised and refuses to clear it until at least one new commit has been made —
ensuring a fix is actually committed before the drift is acknowledged. Use
`specguard ack --force` to bypass this check when the fix was applied without a
new commit (e.g. a rebase or cherry-pick that landed before the sentinel was raised).

**`testaudit` — detect tests that are implemented but not run**: `specguard testaudit`
scans all `.rs` files and reports: (a) `#[ignore]`-annotated tests, (b) tests inside
`#[cfg(…)]` blocks that are never compiled, (c) `.rs` files containing `#[test]`
functions that are not `mod`-declared by any parent (so `cargo test` never picks them
up), and (d) integration-test files under `tests/` that are not included by the
workspace. Exit 0 means clean; exit 7 means findings were found. Add `--json` for
machine-readable output.

### Slash commands (plugin)

| Command | Backing binary | Description |
|---|---|---|
| `/specguard:run [--baseline <ref>]` | `prompt --json` + subagent + `ingest` | subscription-native audit |
| `/specguard:brief <task>` | `brief --prompt` + subagent | read-only pre-task spec briefing (prevent drift before coding) |
| `/specguard:scope` | `scope` | show the resolved scope (no agent) |
| `/specguard:ack` | `ack` | clear a handled sentinel |
| `/specguard:accept-prompt <reason>` | `accept-prompt` | ratify & pin the prompt (meta-canon) |
| `/specguard:decide <title>` | `decide` | scaffold a decision record (ADR) pinned to the canon commit |
| `/specguard:drift-map [target] [--baseline <ref>]` | `map sync`/`map list --filter` + subagent | **Write side.** Maintain the spec↔impl mapping, author missing specs, and reconcile drift (HOTL when unclear). `target` (command/crate/API/e2e/NL) narrows scope; after an adjustment it runs the tests and hands off to backlog/condukt/flow if they fail |
| `/specguard:spec-audit [target] [--baseline <ref>]` | `audit --json --filter` + subagent + `ingest` | **Read-only.** Audit the **correctness** of impl+spec (and coverage) using the spec-map as scope. `target` narrows to a command/crate/API/e2e; remediation (adding tests, fixes) is handed off to backlog/condukt/flow |

### Subcommands (binary)

```sh
specguard run                      # run the audit (spawns claude --print per shard)
specguard scope                    # print the resolved scope only (no agent)
specguard prompt                   # print each shard's prompt (no agent)
specguard prompt --json            # emit shards as machine-readable JSON (used by the plugin)
specguard ingest [--from <file>]   # feed pre-collected shard outputs (JSON/stdin) into
                                   #   parse→report→sentinel (does NOT spawn an agent)
specguard brief "<task>"           # read-only pre-task spec briefing (runs the agent)
specguard brief "<task>" --prompt  # render the briefing prompt only (used by the plugin)
specguard pending                  # print the active fix-offer if a sentinel is pending (SessionStart hook)
specguard ack                      # clear a handled sentinel (requires a fix commit since the sentinel was raised)
specguard ack --force              # clear the sentinel unconditionally (skips the fix-commit check)
specguard testaudit                # scan for tests implemented but not being run (exit 7 if findings)
specguard testaudit --json         # same, but emit machine-readable JSON ({findings:[{kind,file,name,reason}]})
specguard decide "<title>"         # scaffold a decision record (ADR)
specguard accept-prompt -m "reason"  # ratify the prompt (meta-canon)
specguard map build                # create the spec-map store (if absent) + seed from the full history window
specguard map sync                 # reflect only the git delta since the baseline (A/M/R/D)
specguard map list [--json]        # print the current spec↔impl mapping
specguard map set-spec <key|glob> <doc>  # attach a spec-doc to matching entries + mark them tracked
specguard map resolve <key|glob>   # mark matching entries tracked (reviewed; no spec needed)
specguard map prune                # drop entries matching [map].exclude (non-spec-bearing paths)
specguard --baseline HEAD~5 run    # override the baseline
specguard --config examples/aegis.toml run
```

### The spec-map store + `/specguard:drift-map` (the write-side complement)

Where `run` / `brief` are **read-only audits**, the `map` subcommand and the
`/specguard:drift-map` command are the **write side** that grows the mapping and the specs:

- **`specguard map`** maintains an **independent, reusable mapping store** (`.specguard/spec-map.toml`)
  relating spec/feature ↔ implementation files ↔ tests ↔ API/URL. It is a deterministic skeleton
  layer that reflects the `git log --name-status` delta (Added→new entry, Modified→`changed`,
  Renamed→move, Deleted→detach/`missing`) and carries no drift-workflow logic — it is designed as a
  **shared layer that a future `spec-audit` will also consume** (the command never reimplements the
  map; it always delegates to `specguard map`). Each entry has `kind` (Feature|Endpoint), `spec_doc`,
  `impl_files`, `test_files`, `client_refs`, and `api` ({method, route}). A full-history `map build`
  no longer resurrects deleted sources: because `git log --name-status` emits reverse-chronological
  (newest commit first), changes are folded oldest→newest (last-writer-wins), so a path deleted in a
  newer commit correctly leaves no dangling entry.
- **`/specguard:drift-map`** is the LLM orchestration that **consumes** that store: (1) keep the map
  fresh via `map sync`, (2) reference each entry's spec under `docs/specs/`, (3) for entries with no
  spec, read the impl + tests and author a spec body (overview / invariants / behavior), marked
  `REVIEW-NEEDED` (optionally using the sibling generator `specforge`), and (4) for `changed` entries
  whose spec and code diverge, **fix whichever side is wrong (the spec doc or the implementation).
  When the correct direction is unclear or confidence is low, it does not silently pick — it asks a
  human via `AskUserQuestion` (Human-on-the-loop).** Semantic attribution (which file belongs to which
  feature) is derived from the cheapest sufficient source per entry — test code, API/route impl, client
  HTTP calls, or spec-doc descriptions — and anything not cheaply resolvable is either asked or left
  `missing`, keeping search cost bounded.

#### Keeping the spec-map fresh: periodic `specguard map sync` (opt-in)

`specguard map build` is a one-shot seed (full history window). If nothing ever
runs `specguard map sync` afterward, the map silently drifts out of date as the
repo evolves (new files unmapped, changed files not marked `changed`, deleted
files left dangling) — and `/specguard:drift-map`/`/specguard:spec-audit` scoping
gets less precise the staler the map is. `/specguard:drift-map` does call
`map sync` on demand, but relying on it alone means the map is only ever as
fresh as the last time someone happened to run that command.

To keep it fresh continuously, copy the opt-in template
[`scripts/specguard-map-sync.cron.example`](../../scripts/specguard-map-sync.cron.example)
(repo root) into your own crontab or a git hook (`post-merge`/`pre-push`). It is
**not auto-installed** — nothing runs until you copy a fragment yourself — and
every fragment is advisory/fail-soft (a failed sync just leaves the map stale
until the next run; it never blocks a push/merge or condukt/rollout).

Verify the command before wiring it up:

```sh
specguard map sync --help
cd crates/specguard && specguard map sync   # or: specguard map sync -c crates/specguard/specguard.toml
```

### `/specguard:spec-audit` (correctness audit, read-only)

Where spec-drift (`run` / `drift-map`) checks whether spec and implementation **agree** (consistency),
`/specguard:spec-audit` checks their **correctness** — is the implementation actually right and the spec
itself sound — catching cases where the two are mutually consistent yet **both wrong**. It uses the
spec-map store as scope (per feature). A read-only subagent judges each entry on three dimensions:
(a) **spec soundness** (contradiction / silence / ambiguity / unfalsifiable or wrong requirements),
(b) **implementation correctness** (real bugs, edge cases, security, invariants not actually upheld),
and (c) **test adequacy / coverage** (the deterministic `Untested` signal plus whether existing tests
actually exercise the spec's behaviors).

- **Targeted**: `/specguard:spec-audit <target>` accepts a concrete command, crate, API route, or natural
  language, resolved to `specguard audit --filter <query>` (e.g. `drift-map`, `crates/specguard`, `/health`,
  or **`e2e`** → entries whose `test_files` are under `tests/e2e/...`). Empty = whole map. If the map is
  stale/unmapped for the query, it is rebuilt (`specguard map sync`/`build`) before auditing.
- **Read-only + handoff**: the audit writes nothing (findings → report/sentinel, HOTL). Remediation such as
  **adding tests is NOT done by spec-audit itself** — it enqueues a `backlog add` task or hands off to
  `/condukt` / `/flow` for the executor to implement (source→executor separation).

### Output

| Path | Contents |
|---|---|
| `<report_dir>/<date>.md` | the report |
| `<report_dir>/.last-ref` | the last audited HEAD (next run's change-triggered baseline) |
| `<sentinel>` | only when `needs_user=yes` (date / report / summary) |

The baseline **advances in lockstep with ack**: `.last-ref` moves to HEAD only on
a clean run, and is held while findings remain (so unfixed drift can't fall out
of the next run's diff and go undetected).

---

## Configuration (TOML)

`specguard.example.toml` has a fully commented sample of every field. Key points:

- `[project]` … `name`, `root` (repository root)
- `[agent]` … `command` + `args`. Defaults to `claude --print` (with a read-only
  allowlist). Swappable for any agent CLI (reads a prompt on stdin, writes the
  report to stdout)
- `[scope]` … `baseline_ref` / `fallback_ref` (if neither resolves, all tracked
  files are audited)
- `[output]` … `report_dir` / `sentinel`
- `[prompt]` … `template` (embedded default if omitted) / `require_ratification`
  (the ratification gate) / `graded` + `graded_threshold` (graded triage: default
  OFF keeps the classic binary gate; when ON, a changed template whose
  token-shingle Jaccard similarity to an already-ratified precedent is `>=
  graded_threshold` auto-ratifies, reserving human `accept-prompt` for large
  deviations. A **polarity guard** rides alongside the similarity score: it
  applies synonym expansion and canonical-bucket folding (e.g. approve/deny,
  permit/forbid, human/auto all collapse to the same bucket as their synonyms)
  to detect meaning inversion, so only a genuine cross-bucket polarity flip
  forces `Novel` even when the lexical similarity stays high — that inversion
  always escalates to a human regardless of the threshold)
- `[[area]]` (repeatable) … `name` / `globs` / `canon`. **In-scope when a change
  matches `globs`**
- `[[invariant]]` (repeatable) … `name` / `description` / `canon`. **Checked
  every run**
- `[verify]` … verification gates (default OFF). `enabled` = refutation (drop
  false positives) / `completeness` = completeness critique (surface false
  negatives). **Enabling both is recommended.** See [DESIGN-VERIFY.md](DESIGN-VERIFY.md)
- `[decisions]` … enable the decision-record (ADR) freshness/staleness check (D3)

`examples/aegis.toml` is a config reproducing the original AEGIS implementation.

### The three audit dimensions (overview)

The canon is the audit prompt (`templates/audit-prompt.md` /
`decisions-prompt.md`). In brief:

- **D1 implementation↔canon drift**: has the implementation drifted from the
  canon (contradictions classified as misread / code-violation / stale-canon).
- **D2 spec quality**: silence / contradiction / duplication in the canon docs
  themselves.
- **D3 decision-log freshness/staleness**: pin the *reason* for a spec change to
  a canon commit and check whether the decision still holds (enable via
  `[decisions]`).

---

## Exit codes

| code | meaning |
|---|---|
| 0 | success |
| 2 | config / usage error |
| 3 | a shard's output lacked the marker (report saved; baseline not advanced, no sentinel) |
| 4 | a shard's agent exited non-zero (the real code goes to stderr) |
| 5 | the prompt (meta-canon) is unratified/changed (when `require_ratification` is on); needs `accept-prompt` |
| 6 | `ack` found no fix commit since the sentinel was raised; pass `--force` to override |
| 7 | `testaudit` found tests that are implemented but not being run |
| 8 | `testaudit` could not determine — an unreadable dir/`.rs` made the scan incomplete; fails closed rather than report GREEN (unknown → RED) |

The source of truth is the `EXIT_*` constants in `src/main.rs` (this is the only
doc copy of the table). Agent exit codes are never propagated raw — they always
collapse to `4`, with each shard's real code on stderr.

---

## About the read-only guarantee

- **standalone**: the default agent launches with an allowlist (Read/Grep/Glob +
  `git diff/log/show/status`) and denies writes, network, and arbitrary shell. In
  `--print` mode any tool outside the allowlist is auto-denied, so even a prompt
  injection from the audited repo's content cannot run a destructive command. It
  is guaranteed by **permissions**, not by a polite request in the prompt.
- **plugin**: the subagent guarantee is at the **tool-name** level (Edit/Write/
  NotebookEdit/WebFetch/WebSearch revoked + a read-only-git prompt discipline). A
  Claude Code subagent definition cannot express a Bash *argument* allowlist
  (`Bash(git diff *)`), so it is weaker against prompt injection than standalone.
  For targets where enforcement strength matters most, prefer standalone
  `specguard run`.
- When the verification gates (`[verify]`) are on, the refutation/completeness
  steps inside `ingest` still spawn an agent via the binary (so a nested claude
  runs even through the plugin). Full native-ization is future work.

---

## Tests

```sh
cargo test          # unit (parse/scope/prompt/report) + integration (fake agent)
```

The integration tests use a `bash -c` fake agent, so no real LLM is required.

## License

MIT
