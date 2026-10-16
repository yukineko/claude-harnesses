存在しないファイル・シンボル・パスを捏造しない。実在する `path:line` を引用すること。[test: crates/deepwiki/tests/prose_claims.rs::claim_writer_forbids_invention]

```
/deepwiki
```

1. `deepwiki status` — wiki が現在のコミットに対して新鮮かどうかを判定する。[test: crates/deepwiki/tests/prose_claims.rs::claim_status_fresh_stale]
2. `deepwiki scan` — Rust バイナリがリポジトリを地図化する（`--json` で機械可読な形式も出せる）。[test: crates/deepwiki/tests/prose_claims.rs::claim_scan_map]
3. `deepwiki stamp` — 書いたページとともに HEAD を記録し、次回の実行が何が変わったかを判別できるようにする。[test: crates/deepwiki/tests/prose_claims.rs::claim_stamp_records] [test: crates/deepwiki/tests/prose_claims.rs::claim_status_fresh_stale]

`status` は `✅ fresh` か `⚠ stale` を、変更されたソースファイルとともに報告する。[test: crates/deepwiki/tests/prose_claims.rs::claim_status_fresh_stale]

wiki が無い場合、`status` は `no wiki yet` と報告する。[test: crates/deepwiki/tests/integration.rs::status_on_dir_without_wiki_reports_no_wiki]

```sh
deepwiki scan
deepwiki status
deepwiki stamp PAGES…
deepwiki init
```

全サブコマンドが `--root <dir>`（既定は cwd）を受け付ける。[test: crates/deepwiki/tests/prose_claims.rs::claim_root_flag]

- `commands/deepwiki.md` — `/deepwiki` のオーケストレーションコマンド。[test: crates/deepwiki/tests/prose_claims.rs::claim_command_file]
- `agents/deepwiki-writer.md` — ページを書くサブエージェント。[test: crates/deepwiki/tests/prose_claims.rs::claim_writer_agent_file]
- `bin/deepwiki`（ホストのバイナリを exec する、追跡されるランチャー）+ `bin/deepwiki-<os>-<arch>`（git 管理外）。[test: crates/deepwiki/tests/prose_claims.rs::claim_launcher_tracked_bins_ignored] [test: crates/deepwiki/tests/launcher_missing_binary.rs::control_launcher_still_execs_the_host_binary_when_it_is_present]
- `.deepwiki/_meta.toml`（あなたのリポジトリ側）— `stamp` が書く鮮度スタンプ。[test: crates/deepwiki/tests/prose_claims.rs::claim_stamp_records]

プラットフォーム別の `bin/deepwiki-<os>-<arch>` は**コミットされない**（`.gitignore` が `crates/*/bin/*-darwin-arm64`・`*-darwin-x86_64`・`*-linux-arm64`・`*-linux-x86_64` を無視する）。[test: crates/deepwiki/tests/prose_claims.rs::claim_launcher_tracked_bins_ignored] [test: crates/deepwiki/tests/prose_claims.rs::claim_platform_bins_untracked]

```sh
cargo build --release -p deepwiki
cargo test -p deepwiki
```
