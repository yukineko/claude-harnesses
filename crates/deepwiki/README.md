# deepwiki

A repository **architecture wiki** for Claude Code, generated and kept fresh by a
`/deepwiki` command. Inspired by **Devin Wiki**: instead of re-reading the whole
codebase every time you need orientation, deepwiki writes a concise, source-linked
map of the repo to `.deepwiki/*.md` — committed with the code — and refreshes only
the parts that changed.

Subscription-native: a bundled **Rust scanner** does the deterministic mapping,
a **subagent** writes the pages (so heavy repo reading stays out of your main
conversation), and freshness is tracked against **git**. No API key.

## How it works

```
/deepwiki
```

1. `deepwiki status` — is the wiki fresh vs the current commit?
2. `deepwiki scan` — the Rust binary maps the repo (languages, layout, entry
   points, key files) with no LLM.
3. The **`deepwiki-writer` subagent** turns that map into `.deepwiki/overview.md`
   plus per-module pages, citing real `path:line` locations.
4. `deepwiki stamp` — records the commit so the next run knows what changed.

On later runs `status` reports `✅ fresh` or `⚠ stale` with the changed source
files, and the refresh focuses only on the subsystems that moved.

## The binary

```sh
deepwiki scan            # markdown repo map (--json for machine form)
deepwiki status          # fresh / stale vs the wiki's build commit
deepwiki stamp PAGES…    # record HEAD + the pages written (called by /deepwiki)
deepwiki init            # create .deepwiki/
```

Every subcommand takes `--root <dir>` (default: the cwd).

## Layout

- `commands/deepwiki.md` — the `/deepwiki` orchestration command.
- `agents/deepwiki-writer.md` — the page-writing subagent.
- `bin/deepwiki` (tracked launcher) + `bin/deepwiki-<os>-<arch>` (locally built,
  git-ignored) — the scanner.
- `.deepwiki/` (in *your* repo) — the generated wiki + `_meta.toml` freshness stamp.

## Build

```sh
cargo build --release -p deepwiki
cargo test -p deepwiki
```

The platform-tagged `bin/deepwiki-<os>-<arch>` binaries are **not committed**
(`.gitignore` ignores `crates/*/bin/*-<os>-<arch>`); local deploy builds from
source via `scripts/rollout-plugins.sh`. The crate's `Makefile` (`make bins`)
copies from a crate-local `target/release/`, which a workspace build does not
produce.
