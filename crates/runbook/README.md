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

`runbook install`/`uninstall` are idempotent, back up `settings.json`, and
preserve foreign hook groups.

## Config

See [`runbook.example.toml`](runbook.example.toml): `enabled`, `project_dir`,
`global_dir`, `include_global`, `prefix`, `index_token`, `max_chars`,
`per_runbook_chars`. `RUNBOOK_DISABLE=1` turns it off.

## Build

```sh
make bins     # refresh bin/runbook-darwin-<arch> and bin/runbook-linux-x86_64
cargo test
```

The committed `bin/runbook-*` binaries are what the plugin ships, so end users
need neither cargo nor an API key. Rebuild and recommit them when behavior
changes.
