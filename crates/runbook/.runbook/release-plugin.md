+++
description = "バンドルバイナリを更新してプラグインをリリースする手順"
aliases = ["release", "ship"]
+++

# release-plugin

## Overview
この yukineko プラグイン（Rust 小バイナリ + フック）をリリースするときの手順。
動作を変えたら必ずバンドルバイナリを再ビルドする必要がある。プロンプトで
`!release-plugin`（または `!release`）と書くとこの手順が注入される。

## Procedure
1. `cargo test` と `cargo clippy --all-targets` が通ることを確認する
2. `make bins` で `bin/<name>-darwin-arm64` と `bin/<name>-linux-x86_64` を再生成する
3. `.claude-plugin/plugin.json` の `version` を上げる
4. `bin/` を実行ビット付きで commit する（`git update-index --chmod=+x`）
5. リポ root の `.claude-plugin/marketplace.json`（このリポの marketplace カタログ）を更新する

## Specifications
- 完了条件: 新しいバイナリがコミットされ、plugin.json の version が上がっていること
- バイナリは `opt-level="z"` / `lto` / `strip` でビルドされた小サイズであること

## Forbidden Actions
- 動作変更時にバイナリ再ビルドを忘れて古いコードのまま出さない
- API キーや秘密情報を手順・設定ファイルに直接書かない
