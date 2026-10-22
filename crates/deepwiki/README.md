Never invent files, symbols or paths; cite a real `path:line`.

```
/deepwiki
```

1. `deepwiki status` — is the wiki fresh vs the current commit?
2. `deepwiki scan` — the Rust binary maps the repo (languages, layout, entry
   points, key files).
3. `deepwiki stamp` — records the commit so the next run knows what changed.

On later runs `status` reports `✅ fresh` or `⚠ stale` with the changed source
files.

Without a wiki, `status` reports `no wiki yet`.

```sh
deepwiki scan
deepwiki status
deepwiki stamp PAGES…
deepwiki init
```

Every subcommand takes `--root <dir>` (default: the cwd).

- `commands/deepwiki.md` — the `/deepwiki` orchestration command.
- `agents/deepwiki-writer.md` — the page-writing subagent.
- `bin/deepwiki` (tracked launcher that execs the host binary) +
  `bin/deepwiki-<os>-<arch>` (git-ignored).
- `.deepwiki/_meta.toml` (in *your* repo) — the freshness stamp written by `stamp`.

```sh
cargo build --release -p deepwiki
cargo test -p deepwiki
```

The platform-tagged `bin/deepwiki-<os>-<arch>` binaries are **not committed**
(`.gitignore` ignores `crates/*/bin/*-darwin-arm64`, `*-darwin-x86_64`,
`*-linux-arm64` and `*-linux-x86_64`).
