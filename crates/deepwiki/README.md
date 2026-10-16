Never invent files, symbols or paths; cite a real `path:line`. [test: crates/deepwiki/tests/prose_claims.rs::claim_writer_forbids_invention]

```
/deepwiki
```

1. `deepwiki status` — is the wiki fresh vs the current commit? [test: crates/deepwiki/tests/prose_claims.rs::claim_status_fresh_stale]
2. `deepwiki scan` — the Rust binary maps the repo (languages, layout, entry
   points, key files). [test: crates/deepwiki/tests/prose_claims.rs::claim_scan_map]
3. `deepwiki stamp` — records the commit so the next run knows what changed. [test: crates/deepwiki/tests/prose_claims.rs::claim_stamp_records] [test: crates/deepwiki/tests/prose_claims.rs::claim_status_fresh_stale]

On later runs `status` reports `✅ fresh` or `⚠ stale` with the changed source
files. [test: crates/deepwiki/tests/prose_claims.rs::claim_status_fresh_stale]

Without a wiki, `status` reports `no wiki yet`. [test: crates/deepwiki/tests/integration.rs::status_on_dir_without_wiki_reports_no_wiki]

```sh
deepwiki scan
deepwiki status
deepwiki stamp PAGES…
deepwiki init
```

Every subcommand takes `--root <dir>` (default: the cwd). [test: crates/deepwiki/tests/prose_claims.rs::claim_root_flag]

- `commands/deepwiki.md` — the `/deepwiki` orchestration command. [test: crates/deepwiki/tests/prose_claims.rs::claim_command_file]
- `agents/deepwiki-writer.md` — the page-writing subagent. [test: crates/deepwiki/tests/prose_claims.rs::claim_writer_agent_file]
- `bin/deepwiki` (tracked launcher that execs the host binary) +
  `bin/deepwiki-<os>-<arch>` (git-ignored). [test: crates/deepwiki/tests/prose_claims.rs::claim_launcher_tracked_bins_ignored] [test: crates/deepwiki/tests/launcher_missing_binary.rs::control_launcher_still_execs_the_host_binary_when_it_is_present]
- `.deepwiki/_meta.toml` (in *your* repo) — the freshness stamp written by `stamp`. [test: crates/deepwiki/tests/prose_claims.rs::claim_stamp_records]

```sh
cargo build --release -p deepwiki
cargo test -p deepwiki
```

The platform-tagged `bin/deepwiki-<os>-<arch>` binaries are **not committed**
(`.gitignore` ignores `crates/*/bin/*-darwin-arm64`, `*-darwin-x86_64`,
`*-linux-arm64` and `*-linux-x86_64`). [test: crates/deepwiki/tests/prose_claims.rs::claim_launcher_tracked_bins_ignored] [test: crates/deepwiki/tests/prose_claims.rs::claim_platform_bins_untracked]
