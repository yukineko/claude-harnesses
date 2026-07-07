# reviewgate

A **code-review gate** for Claude Code. On every `Stop`, reviewgate reviews the
diff before the agent is allowed to declare a turn done — the "is this *good*
code?" complement to [donegate](https://github.com/yukineko/donegate)'s "does it
actually *run*?".

It is subscription-native: one Stop hook plus a bundled Rust binary, **no API
key**. The binary is a deterministic orchestrator; the LLM judgement is done
either by the agent already running on your subscription (inject mode) or by a
headless `claude -p` it spawns (subprocess mode).

## Two modes

| mode | what it does | independence | cost |
|------|--------------|--------------|------|
| `inject` (default) | Blocks the stop once per new diff state and injects a review **rubric**; the running agent reviews its own changes and fixes issues before finishing. | self-review | free (no extra process) |
| `subprocess` | Runs `reviewer_cmd` (default `claude -p`) as an **independent** reviewer over the diff and blocks only when it reports issues, injecting just those findings. | independent reviewer | one headless review per round |

## How it converges

reviewgate hashes the reviewable diff. A stop whose diff matches the one it last
forced a review of is allowed through — the agent already reviewed exactly that.
A *changed* diff costs one more round, capped by `max_attempts` (default 2), so
the agent is never trapped. A genuine *harness* error (no git, bad config, our
own bug) always **allows** the stop — reviewgate must never trap a turn because
it itself broke.

Safe by default: no git repo, or no reviewable file changed, → the stop is
allowed. Lockfiles, `node_modules`, `target`, generated files, etc. are excluded.

## Fail closed, but bounded

A *reviewer* that itself fails is **not** the same as a clean review, so it does
not silently allow — that would turn a broken reviewer into a bypass:

- A reviewer subprocess that crashes / times out / emits unusable output →
  **block** (bounded by `max_attempts`), then give up loudly.
- A diff too large to review whole (truncated to `max_diff_bytes`) has an
  unreviewed tail → **block** (bounded by `max_attempts`), then give up loudly.

In both cases the block reason names every escape hatch (`.reviewgate-skip`,
`REVIEWGATE_DISABLE=1`, raising `max_diff_bytes`), so a broken reviewer or an
oversized diff can never permanently trap the turn.

## Install

### As a plugin (subscription, no build)

```
/plugin marketplace add yukineko/claude-harnesses
/plugin install reviewgate@yukineko
```

### From source

```
cargo install --path .
reviewgate init          # write a starter ./reviewgate.toml
reviewgate install       # wire the Stop hook into ~/.claude/settings.json
```

## Subcommands

- `reviewgate review` — the Stop hook (reads the hook JSON on stdin).
- `reviewgate install [--dry-run]` / `uninstall [--dry-run]` — manage the hook.
- `reviewgate init [--force]` — write a starter `reviewgate.toml`.
- `reviewgate status` — show the resolved config and what would be reviewed now.
- `reviewgate trust` — trust the current project so its `./reviewgate.toml`
  (incl. `reviewer_cmd`) is honored; until trusted, a repo-shipped config is ignored.

Run `reviewgate review` by hand (no stdin) for a human-readable dry check.

## Config

See [`reviewgate.example.toml`](reviewgate.example.toml). Project
`./reviewgate.toml` wins over `~/.reviewgate/config.toml` over built-in defaults —
but only once the project root is **trusted** (`reviewgate trust`), since
`reviewer_cmd` runs as a subprocess.

Key fields: `mode`, `max_attempts`, `min_changed_files`, `include`/`exclude`
globs, `rubric`, and (subprocess) `reviewer_cmd` / `reviewer_timeout_secs`.

## Escape hatches

- One-shot: create `.reviewgate-skip` in the project root (a one-line reason);
  consumed once, the next stop is allowed.
- Off entirely: `REVIEWGATE_DISABLE=1`, or `enabled = false` in config.

## Logs

Each decision appends a JSONL line to `<state_dir>/log.jsonl`
(`~/.reviewgate/state/log.jsonl` by default).

## License

MIT
