Reusable **procedure includes**. A `UserPromptSubmit` hook
expands `!name` macros you type into the matching repo-committed procedure
(`.runbook/<name>.md`).

```
> follow !release-plugin and cut a new version
```
→ the hook prints the `release-plugin` procedure (Overview / Procedure /
Specifications / Forbidden Actions) on stdout.

The hook injects only the procedures a prompt asked for. Once the next procedure would exceed `max_chars`, it and the rest are omitted with a `残り N 件` note; the char budget is not hard: the first procedure is always included (up to `per_runbook_chars` plus heading and ellipsis), so the output can exceed `max_chars`.

If an expected procedure did not arrive, check `runbook list` / `runbook status`, which report an unreadable store
(`unknown — …`; `list`/`show` exit 1). A real injection is also recorded as a line in the shared harness inject-metrics ledger
(`~/.harness/state/inject-metrics.jsonl`).

`!name` can be dropped mid-sentence and stacked (`!build !test`).
A macro only fires when it resolves to an existing
runbook, so stray `!` in prose or code (`x != y`, `!!`, `foo!`) never injects
anything.

One markdown file per procedure under `.runbook/` (project, committed) or
`~/.runbook/runbooks/` (global).
The file stem is the macro name
(`deploy.md` → `!deploy`).
Optional TOML frontmatter adds a description and
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

`hooks/hooks.json` wires the UserPromptSubmit
hook (`bin/runbook inject`).
Add procedures under `.runbook/` in your repo and invoke
them with `!name`.

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
`--dry-run` and preserve foreign hook groups; installing twice leaves one runbook hook
group, and install backs up `settings.json`.

Project procedures shadow global ones with the same name. Macros typed in the prompt (names and aliases)
match case-insensitively. If a real runbook is named after `index_token`, `!runbooks`
injects that runbook instead of the index.

See [`runbook.example.toml`](runbook.example.toml): `enabled`, `project_dir`,
`global_dir`, `include_global`, `prefix`, `index_token`, `max_chars`,
`per_runbook_chars`. `RUNBOOK_DISABLE` set to any non-empty value other than `0`
turns the hook off.

The config is read from `./runbook.toml` (project) **or** `~/.runbook/config.toml`
(global) — the first file that exists wins and the two are **not** merged.
`max_chars` is floored at 200 and `per_runbook_chars` is clamped to
`[200, max_chars]`.

```sh
cargo build --release -p runbook
cargo test -p runbook
```

The platform-tagged `bin/runbook-<os>-<arch>` binaries are **not committed**
(they are git-ignored); only the `bin/runbook` launcher
is tracked. The
crate's `Makefile` (`make bins`) copies from a crate-local `target/release/`, which a
workspace build does not produce, so it is not a working build path here.
