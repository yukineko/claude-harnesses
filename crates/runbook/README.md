# runbook

Reusable **procedure includes** for Claude Code. A `UserPromptSubmit` hook
expands `!name` macros you type into the matching repo-committed procedure
(`.runbook/<name>.md`), so a recurring workflow — deploy, cut a release, triage
an incident — runs the same way every time. Inspired by **Devin Playbooks**
(`!macro`), rebuilt as a local, no-API-key hook.

```
> follow !release-plugin and cut a new version
```
→ the hook injects the `release-plugin` procedure (Overview / Procedure /
Specifications / Forbidden Actions) as context before Claude starts.

Subscription-native: one bundled Rust binary, **no API key**. The hook only ever
*injects* the procedures a prompt asked for under a hard char budget, and always
exits 0 — it can never block a turn.

That also means it **degrades silently**: if `.runbook/` or the global dir exists
but cannot be read (permission/IO error — a *missing* dir is just "no runbooks"),
the hook injects **nothing at all** for that prompt, even for macros that would have
resolved, and prints no warning. An individual `.md` that cannot be read, or whose
body is empty, is skipped the same way. If an expected procedure did not arrive,
check `runbook list` / `runbook status`, which do report the unreadable store
(`unknown — …`; `list`/`show` exit 1). Each real injection is also appended
(best-effort) to the shared harness inject-metrics ledger
(`~/.harness/state/inject-metrics.jsonl`).

## How it differs

| | **playbook** (sibling plugin) | **runbook** (this) | Claude **skills** |
|---|---|---|---|
| content | atomic *facts* / conventions | multi-step *procedures* | procedures |
| trigger | automatic, relevance-scored | explicit `!name`, inline & stackable | explicit `/name`, standalone |
| storage | curated note store | plain `.md` committed in the repo | `.claude/skills/` |

runbook deliberately keeps procedures as ordinary markdown files versioned with
your code, invoked by a lightweight inline `!name` you can drop mid-sentence and
stack (`!build !test`). A macro only fires when it resolves to an existing
runbook, so stray `!` in prose or code (`x != y`, `!!`, `foo!`) never injects
anything.

## Procedures

One markdown file per procedure under `.runbook/` (project, committed) or
`~/.runbook/runbooks/` (global). The file stem is the macro name
(`deploy.md` → `!deploy`). Optional TOML frontmatter adds a description and
aliases:

```markdown
+++
description = "本番デプロイ手順"
aliases = ["ship"]
+++

# deploy
## Overview …
## Procedure …
## Forbidden Actions …
```

Type `!runbooks` in a prompt to inject the list of what's available.

## Install (plugin)

Installed from the marketplace, `hooks/hooks.json` wires the UserPromptSubmit
hook automatically. Add procedures under `.runbook/` in your repo and invoke
them with `!name`.

## Standalone (cargo)

```sh
cargo install --path .
runbook init                 # create .runbook/ + a sample procedure
runbook new deploy --description "本番デプロイ手順"
runbook list                 # show available macros
runbook show deploy          # print one procedure
runbook install              # merge the UserPromptSubmit hook into ~/.claude/settings.json
runbook status               # resolved config + dirs + count
runbook uninstall
```

`runbook new` also takes `--global` (write into the global dir instead of the
project) and `--force` (overwrite an existing file); the name is normalized to
lowercase `[a-z0-9_-]` (max 48 chars). `runbook install`/`uninstall` take
`--dry-run`, are idempotent, back up `settings.json`, and preserve foreign hook
groups.

Project procedures shadow global ones with the same name. Macro names and aliases
match case-insensitively. If a real runbook is named after `index_token`, `!runbooks`
injects that runbook instead of the index.

## Config

See [`runbook.example.toml`](runbook.example.toml): `enabled`, `project_dir`,
`global_dir`, `include_global`, `prefix`, `index_token`, `max_chars`,
`per_runbook_chars`. `RUNBOOK_DISABLE` set to any non-empty value other than `0`
turns it off.

The config is read from `./runbook.toml` (project) **or** `~/.runbook/config.toml`
(global) — the first file that exists wins and the two are **not** merged. If that
file cannot be read or parsed, all settings silently fall back to the built-in
defaults (including `enabled = true`); the other layer is not consulted.
`max_chars` is floored at 200 and `per_runbook_chars` is clamped to
`[200, max_chars]`.

## Build

```sh
cargo build --release -p runbook   # lands in the workspace target dir, not crates/runbook/target
cargo test -p runbook
```

The platform-tagged `bin/runbook-<os>-<arch>` binaries are **not committed**
(`.gitignore` ignores `crates/*/bin/*-<os>-<arch>`); only the `bin/runbook` launcher
is tracked. Local deploy builds from source via `scripts/rollout-plugins.sh`. The
crate's `Makefile` (`make bins`) copies from a crate-local `target/release/`, which a
workspace build does not produce, so it is not a working build path here.
