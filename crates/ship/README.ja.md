コミット・マージ・プッシュは決して自動実行しない。明示的なユーザー承認を必ず先に得ること。

`ship check`・`ship check --run-safe`・`ship rollout` が実行する git サブコマンドは読み取り専用のものだけである [test: crates/ship/tests/prose_claims.rs::claim_binary_runs_only_readonly_git] [test: crates/ship/tests/integration.rs::binary_never_mutates_git]。
`scripts/rebuild-plugins.sh` と `scripts/rollout-plugins.sh` のどちらにも、状態を変更する git コマンドは含まれない [test: crates/ship/tests/prose_claims.rs::claim_scripts_no_mutating_git]。

```sh
ship check             # チェックリストを表示：未コミット変更、未マージの condukt/* ブランチ、残った worktree、未 push コミット、古いプラグインバイナリ [test: crates/ship/tests/prose_claims.rs::claim_check_lists_categories]
ship check --run-safe  # さらに repo root で scripts/rebuild-plugins.sh --stage-repo を実行し、GATED 項目を再表示 [test: crates/ship/tests/prose_claims.rs::claim_run_safe_runs_rebuild_stage_repo]
ship rollout           # repo root で scripts/rollout-plugins.sh を引数なしで実行 [test: crates/ship/tests/prose_claims.rs::claim_rollout_runs_script_no_args]
ship session-end       # SessionEnd hook のコマンド [test: crates/ship/tests/prose_claims.rs::claim_hooks_json_sessionend]
```

`ship check` は作業ツリーも stage 済みの内容も `HEAD` も変更しない [test: crates/ship/tests/prose_claims.rs::claim_check_does_not_modify]。
`ship check --run-safe` は `scripts/rollout-plugins.sh` を自分では実行せず、`ship rollout` は `scripts/rebuild-plugins.sh` を自分では実行しない [test: crates/ship/tests/prose_claims.rs::claim_run_safe_and_rollout_separate]。
`scripts/rollout-plugins.sh` は `scripts/rebuild-plugins.sh` を `--stage-repo` なしで実行し、`scripts/rebuild-plugins.sh` が `crates/*/bin` へコピーするのは `--stage-repo` のときだけである [test: crates/ship/tests/prose_claims.rs::claim_rollout_script_no_stage_repo]。

プラグインが登録する hook は 1 つ：`SessionEnd` が `${CLAUDE_PLUGIN_ROOT}/bin/ship session-end` を実行する [test: crates/ship/tests/prose_claims.rs::claim_hooks_json_sessionend]。
セッション後、`/ship` を実行して出荷リチュアルをステップバイステップで進めること [test: crates/ship/tests/prose_claims.rs::claim_skill_ship_exists]。

stale 判定は、各クレートの `src/` 配下の最新 mtime をローカルの `crates/<name>/bin/<name>-linux-x86_64`
と比べる [test: crates/ship/src/stale.rs::test_stale_crates_newer_src] [test: crates/ship/src/stale.rs::test_stale_crates_newer_bin] [test: crates/ship/tests/prose_claims.rs::claim_stale_newest_nested_src]。
このリポジトリではプラットフォーム別バイナリ（`crates/*/bin/*-linux-x86_64`）は git 管理外である [test: crates/ship/tests/prose_claims.rs::claim_platform_bins_gitignored]。

## コミット・マージ・プッシュを決して自動実行しない

- **コミット・マージ・プッシュ**: 決して自動実行しない。明示的なユーザー承認を必ず得ること。diff を表示したうえで「承認？」と尋ね、「了解」まで待つこと。

## プラグインをインストールすること

```
/plugin install ship@yukineko
```

## ソースからインストールすること

```sh
cargo install --path .
```

ライセンス: MIT [test: crates/ship/tests/prose_claims.rs::claim_license_mit]
