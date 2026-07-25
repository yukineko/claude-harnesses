# fugu-router

**fugu-style per-model routing for Claude Code orchestration.**

[Sakana AI's fugu](https://sakana.ai/fugu-release/) hides a *trained coordinator*
that routes each request across a pool of specialized models by role
(Thinker / Worker / Verifier), then verifies and synthesizes their work. The
coordinator is learned (evolution strategy / RL).

We can't train Claude's weights. So fugu-router keeps the **shape** — a separate,
deterministic component that decides *which model runs which task* — but replaces
the *trained* judgement with **retrieval over outcomes**:

```
record outcomes ── episodes.jsonl ──▶ k-NN over similar past tasks
(model, pass?, cost)                    │
                                        ▼
                          cheapest tier that historically
                          clears the bar  →  suggested_model
```

This is the honest substitution discussed in `docs/AGENTIC-CODING-GUIDE.md`:
fugu *learns* the router; we *retrieve* a policy. Coarser (per-task, not
per-token), but it needs no API key and no embedding service.

> **`procedures` vs the `playbook` plugin.** fugu-router's `procedures search`
> subcommand retrieves *how similar verified tasks were solved* (k-NN over the
> outcome store) to seed condukt's interpreter. That is distinct from the
> standalone **`playbook` plugin**, which injects curated *knowledge notes* into
> a prompt. The subcommand used to be named `playbook` (hence the internal
> `playbooks.jsonl` store, kept for back-compat); it was renamed to `procedures`
> to remove that collision. `fugu-router playbook …` still works as a hidden alias.

## How it maps to fugu

| fugu | fugu-router |
|---|---|
| trained coordinator | deterministic policy over an episode store |
| agent pool / tiers | Claude tiers `haiku < sonnet < opus` |
| role assignment | per task: `worker_model` + independent `verifier_model` |
| "when to delegate / which model" | cheapest tier whose similar-task pass-rate clears the bar |
| learning (CMA-ES / RL) | retrieval + **online bandit** (Thompson sampling) over logged outcomes |

### Two kinds of learning, on purpose

- **Non-parametric (retrieval):** k-NN over recorded episodes. Instant adaptation
  (one example changes behaviour), fully interpretable, trivially correctable
  (delete a bad episode). Similarity is **semantic-ish** — suffix stemming + a
  domain concept lexicon (`semantic.rs`) bridge synonyms like login ↔ auth ↔
  session that pure lexical matching would miss.
- **Parametric/online (bandit):** Thompson sampling over a Beta(passes, fails)
  posterior per tier (`policy::decide_bandit`, toggled by `explore`). It draws a
  pass-probability for each tier and picks the cheapest that clears the bar, so
  an unproven cheaper tier gets *probed* under uncertainty and the policy updates
  from the result. This is genuine online learning of the reward — not just recall.

**Limit (stated plainly):** routing is per-task, not per-turn; there is no
hidden-state head routing and no neural weight update. The bandit learns the
*reward* (which tier pays off) online; it does not learn a deep representation.
Semantic bridging is lexicon-based, not embedding-based.

## Commands

```
fugu-router route --file decomp.json [--report route.json] [--mode fast|normal|high]  # set suggested_model per task; JSON to stdout
fugu-router record --title "..." --files a,b --class parallel \
                   --model sonnet --status verified --cost 0.12 [--mode fast|normal|high]  # feed an outcome
fugu-router suggest --files src/auth/login.ts "fix login validation" [--mode fast|normal|high]  # one-off
fugu-router confidence --files src/auth/login.ts "fix login validation"  # calibrated pass-probability in [0,1]
fugu-router procedures search --query "fix login validation" --files a,b --k 3
                                                             # k-NN over how similar verified tasks were solved
fugu-router stats [--json]                                   # per-model pass-rate / avg cost + duration coverage
fugu-router duration [--json]                                # alias of duration-outliers: duration coverage + per-class outliers
fugu-router import --episodes /path/episodes.jsonl [--playbooks /path/playbooks.jsonl] [--dry-run]
                                                             # merge another machine's stores (content-hash dedup)
fugu-router import --dedup                                   # dedup local stores in place
fugu-router sync [--pull-only | --push-only]                 # sync the record store with sync_repo (git)
fugu-router init                                             # write fugu-router.toml
fugu-router prompt                                           # UserPromptSubmit hook (injects a summary)
```

### `route` — the deterministic routing step

Reads a condukt decomposition, rewrites each task's `suggested_model` from
routing memory, and prints the same JSON. Pipe it straight into condukt:

```bash
condukt validate --file decomp.json
fugu-router route --file decomp.json --report /tmp/route.json > decomp.routed.json
condukt schedule --file decomp.routed.json
```

The `--report` file carries the extra advice condukt's schema has no field for —
notably `verifier_model` (an independent, usually different tier) — keyed by task id.

### `record` — the learning signal

After condukt verifies a task, log the outcome so the next run is smarter:

```bash
fugu-router record --title "<task title>" --files "<touched_files>" \
  --class parallel --model sonnet --status verified --cost 0.09
```

`--status` other than a pass-word (`verified|pass|passed|ok|true`) counts as a
non-pass. `--cost` is optional (read it from `gauge` if you want cost-aware routing).

Pass `--skill-fingerprint "$(fugu-router fingerprint)"` to stamp the episode with
the version of the SKILL.md corpus that produced it (stored as
`Episode.skill_fingerprint`, omitted when absent). Without it, a silent SKILL.md
edit that changes behaviour leaves the outcome unattributable to its cause; with
it, outcomes can be stratified by skill version and `evalkit canary` can diff two
versions' goldens.

`--duration <secs>` (optional) records the measured wall-clock duration for the
task — measurement only, never consulted by routing/scoring. `Episode.duration_secs`
is a **tri-state**: `None` = unmeasured (never conflated with a measured `0.0`),
`Some(x)` = a real measurement `x > 0`. **Omit `--duration` to record an episode as
unmeasured**; a non-positive value is **rejected** (non-zero exit) because `0` /
negative would be an unrepresentable/unmeasured measurement. On disk the field is
omitted entirely for unmeasured episodes; for back-compat, a legacy line that wrote
`duration_secs: 0.0` (the old encoding for "unmeasured") is read back as unmeasured
(`None`), never as a measured `0.0`. Unmeasured episodes are excluded from every
duration average (never folded in as `0.0`). `fugu-router stats` and `fugu-router
duration` (alias of `duration-outliers`) both print **duration coverage** as
`recorded/total` so the (historically low) recording rate is visible rather than
hidden behind averages.

`--delegation <fork|inline>` (optional, unset by default) tags the episode with
which subagent delegation strategy produced it, for a fork-vs-inline
cost/duration comparison (see `docs/design-delegation-strategy-measurement.md`
in the repo root). Free text, not validated — same looseness as `--class`.
Omit it for ordinary worker/verifier records unrelated to that comparison; it
is never consulted by `route`/`decide_bandit`.

### The mode axis (`fast` / `normal` / `high`)

`route` and `suggest` accept `--mode fast|normal|high`: a **deterministic
clamp applied after** the policy (`decide`/`decide_bandit`) has already
picked a worker/verifier pair. It never touches the learning logic itself.

- `normal` — identity (the backward-compatible default); the policy's pick
  stands unchanged.
- `high` — shift the worker one tier up, capped at `opus`.
- `fast` — shift the worker one tier down, capped at `sonnet` — `opus` can
  never be selected as worker or verifier under `fast`.

Under both `fast` and `high`, the verifier is **recomputed from the clamped
worker** via `policy::verifier_model` (never carried over unchanged); under
`fast` the recomputed verifier is additionally capped at `sonnet`.

Precedence: `--mode` flag > env `FUGU_ROUTER_MODE` > `config.toml`'s `mode` >
default `normal`. **An invalid value (env or config) is never silently
coerced to `normal`** — it is an explicit error with a non-zero exit.

A `gated` decision (`class=gated`, human-approved opus/opus) is a **no-op
under every mode**, mirroring `policy::downgrade_for_budget`'s own `gated`
no-op.

**Ordering: the mode clamp runs first, `downgrade_for_budget` (budget-pressure
downgrade) runs last.** Budget is a hard resource limit; mode is only a
preference, so budget wins. When a `mode=high` pick gets negated by budget
pressure, the rationale still shows both the upshift and the downgrade that
took it back — `downgrade_for_budget` appends to (never replaces) the
incoming rationale, so a high tier never silently disappears.

`record --mode <fast|normal|high>` (optional) tags the episode with the mode
it was routed under. Omitting it means "not recorded" — this is **never**
treated as `Some("normal")`; those are distinct facts.

### `confidence` — calibrated pass-probability

```bash
fugu-router confidence --files src/auth/login.ts "fix login validation"
```

Prints a single number in `[0,1]`: the k-NN neighbourhood's similarity-weighted
pass-rate (via `Episode::effective_pass`, so human `label`s count), the same
retrieval `route`/`suggest` use. Falls back to the neutral prior `0.5` when
there are fewer than `min_samples` neighbours — insufficient history is never
an error, just a maximally-uncertain answer. Pure and deterministic: same
store state + same inputs → byte-identical output, no LLM involved.

### `fingerprint` — version stamp for the SKILL.md corpus

```bash
fugu-router fingerprint                 # hash SKILL.md files under the cwd
fugu-router fingerprint --dir crates    # hash a specific subtree
```

Walks `SKILL.md` files under `--dir` (default cwd), hashes their sorted
relative-path + content with a std hasher, and prints a short stable hex.
Deterministic: the same corpus → the same hex, a changed/added SKILL.md → a
different hex. Feed it to `record --skill-fingerprint` (above).

### `label` — human teacher signal

`record`'s `pass` comes from the verifier judging *its own* sibling's work, so a
biased verifier can reinforce a bad routing choice. `label` lets a human correct
a recorded episode; the human verdict overrides `pass` in policy aggregation
(`Episode::effective_pass`), de-biasing the self-reinforcing loop (cf. Langfuse
Annotation Queues):

```bash
fugu-router label "add login" --verdict bad  --by human   # most recent title match
fugu-router label --latest     --verdict good             # the last episode recorded
```

`--verdict good|bad` sets the episode's `human_label`; the most recent match wins
(`--latest` ignores the title selector). A labeled-bad episode no longer counts
as a pass for its model in k-NN voting; a labeled-good one rescues a verifier
failure. Pass a title selector **or** `--latest`.

## Integration with condukt

condukt is the orchestration spine; fugu-router is its routing brain. The
`/condukt` skill calls `route` between `validate` and `schedule`, and `record`
after each verify. The coupling is **soft**: if the `fugu-router` binary is
absent, condukt falls back to the interpreter's own `suggested_model`, so nothing
breaks. See `crates/condukt/skills/condukt/SKILL.md`.

## Install

### Plugin
Bundles the binary + the UserPromptSubmit hook. The hook injects a one-block
routing-memory summary when your prompt looks like coding work.

### Manual
```
cargo build --release
cp target/release/fugu-router ~/.cargo/bin/
fugu-router init                       # optional config
fugu-router install --dry-run          # preview settings.json change
fugu-router install                    # merge the UserPromptSubmit hook
```
Remove with `fugu-router uninstall`. Set `FUGU_ROUTER_DISABLED=1` to no-op.

## Configuration

`~/.fugu-router/config.toml` — see `fugu-router.example.toml`. Key knobs:
`pass_threshold` (how sure before trusting a cheaper tier), `min_samples` (how
much history before leaving the cold-start prior), `sim_threshold` (how similar a
past task must be to count).

| Key | Default | Description |
|---|---|---|
| `store_file` | `~/.fugu-router/episodes.jsonl` | Episode store path (redirect to a git-tracked path to share across machines) |
| `playbook_file` | `~/.fugu-router/playbooks.jsonl` | Playbook store path (same; both stores can be git-tracked independently) |
| `sync_repo` | *(unset)* | Remote git repo URL for `fugu-router sync`. When set, `store_file`/`playbook_file` default to `<sync_dir>/{episodes,playbooks}.jsonl` |
| `sync_dir` | `~/.fugu-router/record-repo` | Local clone path for `sync_repo` |

### Sharing stores across machines

Two ways to share the stores across machines:

**`sync` (managed git remote).** Set `sync_repo` to a git repo URL; the stores
then default to `<sync_dir>/{episodes,playbooks}.jsonl`. `fugu-router sync` pulls
from the remote first, then commits & pushes local changes (`--pull-only` /
`--push-only` to do one side only).

**Manual (`import`).** Point `store_file` and `playbook_file` at files inside a
git repo you manage yourself. On the receiving machine, after pulling, run
`fugu-router import --episodes /path/to/synced/episodes.jsonl` to merge. The
import deduplicates by content hash so pulling the same episode twice is safe.

`fugu-router import --dedup` rewrites the local stores in place, dropping any
exact duplicates (content-hash comparison; first-seen order is preserved).

### Path normalisation in `record`

`fugu-router record --files ...` normalises absolute file paths to repo-relative
paths at record time. For example, `/Users/yuki/src/harness/crates/x.rs` becomes
`crates/x.rs` when the current working directory is inside the harness repo. This
eliminates machine-specific path segments from the episode store so paths transfer
cleanly across machines and produce better k-NN file-token similarity.

## Cold start

With an empty store, routing uses a keyword prior biased **cheap** — start at the
floor and let the verifier's cascade escalation (haiku→sonnet→opus on a failed
check) buy up only the tasks that need it:

- design/refactor/migrate/security keywords → `opus` (high stakes win outright);
- a very wide blast radius (>10 touched files) → `opus`; a medium spread
  (6–10 files) → `sonnet`;
- rename/format/docs/typo → `haiku` (a wide trivial sweep of >5 files → `sonnet`);
- everything else (an ordinary small change) → `haiku`.

The independent verifier is likewise cheap for low-stakes work: an `opus` worker
is checked by `sonnet`, a low-stakes `sonnet` worker by `haiku`, and a `haiku`
worker by `sonnet` (one tier up, to keep the check independent). Serial/design
tasks still get an `opus` verifier. `gated` tasks are never auto-routed (human
approval).

Defaults reinforce the bias: `pass_threshold = 0.6` and `min_samples = 1`, so a
cheaper tier is trusted after a single mostly-reliable similar success, and the
Thompson-sampling explorer adds a small cheap-tier bonus so unproven cheap tiers
get tried first.

## License

MIT
