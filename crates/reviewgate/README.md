A **code-review gate**.

It registers one Stop hook, which runs the bundled `reviewgate review` binary.

- `off` is the default `mode`: with no config file the stop is allowed with no review, no block and no injected rubric; reviewing is opt-in via `mode = "inject"` or `mode = "subprocess"`.
- In `inject` mode the first stop with a reviewable change in a session is blocked.
- The injected reason carries the review **rubric** verbatim, lists the files and asks for a review.
- The injected reason describes the files as the uncommitted changes of the working tree, says the session attribution is inferred from the transcript, and states that their authorship was not strictly verified.
- `subprocess` mode runs `reviewer_cmd` (default `claude -p`) as the reviewer.
- In `subprocess` mode a reviewer that answers LGTM lets the stop through, and reported findings are rendered verbatim in the block reason.

A stop whose diff matches the one it last forced a review of is allowed through (log tag `already-reviewed`).
A diff that changed after an `already-reviewed` allow is blocked again, never waived as a repeat of the earlier block.
The number of blocking rounds is capped by `max_attempts`, so the agent is never trapped; the inject-mode give-up is announced on stderr and as a `systemMessage`, since it lets the latest diff through unreviewed.
The default of `max_attempts` is 2.

When there is nothing to review (not a git repo, no reviewable file changed) the stop is **allowed**.
When reviewgate *cannot determine* the change it does **not** allow: a git command that errors inside a real repo leaves the changed set undetermined and **blocks** (bounded by `max_attempts`).
Likewise, a reviewable change whose diff text comes back empty (such as under a `.gitattributes` diff driver that prints nothing) is undetermined, not an empty diff, and **blocks** (bounded by `max_attempts`).
A config file that cannot be read/parsed does not fall back to the `off` default — it prints a WARNING and blocks `config-invalid`.
The `review` hook runs under `harness_core::gate::run::run_guarded`: a panic on the first stop blocks (fail **closed**), and only a panic on re-entry (`stop_hook_active`) falls through to allow, so a broken reviewgate cannot trap a turn forever.
Lockfiles are excluded by default.
`node_modules`, `target` and generated files are excluded by default.

**Whose changes.** Only the files **this session wrote** are reviewed (`harness_core::attribution::attribute_own_writes`, read from the hook's `transcript_path`, shared with tdd): a changed file is kept when this session's transcript or a subagent sidechain records an `Edit`/`Write`/`MultiEdit`/`NotebookEdit` of it, or a `Bash` command naming its path; every other changed file is excluded as not attributed to this session.
Known gap: a file written by a command that never names it (`cargo fmt`, a shell glob, a script) is excluded too.
The exclusion is appended to the block reason.
The exclusion is printed to stderr.
When attribution is undetermined (no/unreadable transcript) every changed file is reviewed.

A *reviewer* that itself fails is **not** the same as a clean review, so it does not silently allow — that would turn a broken reviewer into a bypass:

- A reviewer subprocess that cannot be spawned / times out / emits unusable output (empty stdout, non-UTF-8 stdout) → **block** (bounded by `max_attempts`).
- A reviewer whose prompt could not be delivered → **block**.
- A diff too large to review whole (truncated to `max_diff_bytes`) has an unreviewed tail → **block** (bounded by `max_attempts`).
- A diff whose content fetch partly failed (a `git diff` / `git ls-files` call errored, or an untracked file could not be read) is incomplete → **block** (bounded, same path as a failed change scan).
- When the independent reviewer still reports findings after `max_attempts`, the stop is allowed with a stderr WARNING, the distinct log tag `review-giveup`, and an overwatch violation event.
- A malformed `include` glob does not narrow the review, and stderr names the pattern.
- A malformed `exclude` glob excludes nothing, with a stderr WARNING.
- A config file that cannot be read/parsed is reported (stderr WARNING, and `status` shows `FAILED to load`).

The truncation block reason names `reviewgate skip --reason "<why>"`, `REVIEWGATE_DISABLE=1` and raising `max_diff_bytes`; the reviewer-findings block reason names `reviewgate skip --reason` and `REVIEWGATE_DISABLE=1`.

## Install

```
/plugin marketplace add yukineko/claude-harnesses
/plugin install reviewgate@yukineko
```

```
cargo install --path .
reviewgate init          # write a starter ./reviewgate.toml
reviewgate install       # wire the Stop hook into ~/.claude/settings.json
```

- `reviewgate review` — the Stop hook (reads the hook JSON, including `session_id`, on stdin).
- `reviewgate install [--dry-run]` / `uninstall [--dry-run]` — manage the hook.
- `reviewgate init [--force]` — write a starter `reviewgate.toml`.
- `reviewgate status` — show the resolved config and what would be reviewed now.
- `reviewgate trust` — trust the current project so its `./reviewgate.toml` (including `reviewer_cmd`) is honored.
- Until trusted, a repo-shipped config is ignored.
- `reviewgate skip --reason "<why>"` — one-shot, session-scoped skip (see below).

Run `reviewgate review` by hand (no stdin) for a dry check: it exits 1 on a block and prints no hook JSON.

See [`reviewgate.example.toml`](reviewgate.example.toml).
Project `./reviewgate.toml` wins over `~/.reviewgate/config.toml` over built-in defaults — but only once the project root is **trusted** (`reviewgate trust`), since `reviewer_cmd` runs as a subprocess.

Key fields: `mode`, `max_attempts`, `reset_after_secs` (default 600; the per-session attempt counter resets after this idle gap), `min_changed_files`, `max_diff_bytes` (default 200000), `include`/`exclude` globs, `rubric`, `state_dir`, and (subprocess) `reviewer_cmd` / `reviewer_timeout_secs`.
A value of `0` for `max_attempts` / `min_changed_files` is clamped to 1, and `0` for `max_diff_bytes` falls back to the default.

`mode` accepts `"off"`, `"inject"` or `"subprocess"` (aliases `reviewer` / `independent`).
Any other value is **config-invalid**: it is not mapped to either mode, and the stop is blocked (`config-invalid`, naming the value) — even when nothing is reviewable.
`reviewgate status` shows the state; `reviewgate skip --reason ...` or `REVIEWGATE_DISABLE=1` are the escape hatches.

The plugin also ships `agents/reviewgate-typing.md`, a subagent definition whose tools include Read and no Edit or Write.
The `review` hook does not invoke it; call it explicitly.

- One-shot: `reviewgate skip --reason "<why>"`.
- A reason is required.
- The skip applies only to the session that issued it.
- The skip is consumed once.
- The skip is recorded in the gate log at both issue and consumption.
- The old shared `.reviewgate-skip` file in the project root was removed: it no longer waves a stop through.
- Off entirely: `enabled = false` in config, or `REVIEWGATE_DISABLE=1`.

Each decision appends a JSONL line to `<state_dir>/log.jsonl` (`~/.reviewgate/state/log.jsonl` by default).

License: MIT
