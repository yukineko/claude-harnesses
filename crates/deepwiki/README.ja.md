存在しないファイル・シンボル・パスを捏造しない。実在する `path:line` を引用すること。

```
/deepwiki
```

1. `deepwiki status` — wiki が現在のコミットに対して新鮮かどうかを判定する。
2. `deepwiki scan` — Rust バイナリがリポジトリを地図化する（`--json` で機械可読な形式も出せる）。
3. `deepwiki stamp` — 書いたページとともに HEAD を記録し、次回の実行が何が変わったかを判別できるようにする。

`status` は `✅ fresh` か `⚠ stale` を、変更されたソースファイルとともに報告する。

wiki が無い場合、`status` は `no wiki yet` と報告する。

```sh
deepwiki scan
deepwiki status
deepwiki stamp PAGES…
deepwiki init
```

全サブコマンドが `--root <dir>`（既定は cwd）を受け付ける。

- `commands/deepwiki.md` — `/deepwiki` のオーケストレーションコマンド。
- `agents/deepwiki-writer.md` — ページを書くサブエージェント。
- `bin/deepwiki`（ホストのバイナリを exec する、追跡されるランチャー）+ `bin/deepwiki-<os>-<arch>`（git 管理外）。
- `.deepwiki/_meta.toml`（あなたのリポジトリ側）— `stamp` が書く鮮度スタンプ。

プラットフォーム別の `bin/deepwiki-<os>-<arch>` は**コミットされない**（`.gitignore` が `crates/*/bin/*-darwin-arm64`・`*-darwin-x86_64`・`*-linux-arm64`・`*-linux-x86_64` を無視する）。

```sh
cargo build --release -p deepwiki
cargo test -p deepwiki
```
