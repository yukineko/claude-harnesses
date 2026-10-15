`download` は `cargo test` 中にネットワークへ到達しない。 [test: crates/benchkit/tests/prose_claims.rs::claim_download_not_in_cargo_test] [test: crates/benchkit/tests/prose_claims.rs::claim_download_not_in_cargo_test_binary_runs].
benchkit はデータセットを再配布しない: `tests/fixtures/` 外にデータセットファイルは追跡されておらず、`tests/fixtures/instances.jsonl` は 2 行である。 [test: crates/benchkit/tests/prose_claims.rs::claim_no_redistribution].
`download` と `run-instance --real` 以外では benchkit 自身のコードはプロセスを起動せずソケットも開かず、`load`、`--real` 無しの `run-instance`、`audit-sample` (`--audits` / `--from-violations`) は外部ツールを起動しない。 [test: crates/benchkit/tests/prose_claims.rs::claim_shell_io_only_download_and_real].
`audit-sample` の批准キューは自動適用しない。 [test: crates/benchkit/src/auditsample.rs::ratify_queue_never_auto_applies] [test: crates/benchkit/tests/prose_claims.rs::claim_ratify_queue_cli_never_applies].

benchkit のサブコマンドは `load`、`download`、`run-instance`、`audit-sample` である。 [test: crates/benchkit/tests/prose_claims.rs::claim_subcommands].
`dashboard` はサブコマンドではなくライブラリ・モジュールで、1 run を `runs.jsonl` に 1 行追記し、既存行を書き換えない。 [test: crates/benchkit/tests/prose_claims.rs::claim_subcommands] [test: crates/benchkit/tests/prose_claims.rs::claim_dashboard_append_only].
`run-instance` の背後のパッチ生成器は空の候補を返すスタブである。 [test: crates/benchkit/tests/prose_claims.rs::claim_stub_generator_empty].

ローダーはプロセスを起動せず、時刻も環境変数も読まず、同じファイルを 2 回読むと同じ `Vec<Instance>` になる。 [test: crates/benchkit/tests/prose_claims.rs::claim_loader_pure_deterministic].
ローダーはインスタンスをファイル順に返す。 [test: crates/benchkit/src/loader.rs::skips_blank_lines_and_preserves_order].
不正行は `path:line` 付きでエラーになる。 [test: crates/benchkit/src/loader.rs::malformed_line_reports_line_number] [test: crates/benchkit/tests/prose_claims.rs::claim_malformed_row_path_line].

`instance_id`、`repo`、`base_commit`、`patch`、`test_patch`、`problem_statement` は必須で、どれかを欠く行は読み込みに失敗する。 [test: crates/benchkit/tests/prose_claims.rs::claim_instance_required_fields].
`hints_text`、`created_at`、`version`、`environment_setup_commit` は欠けていれば空文字列になり、`created_at` は原文のまま保持される。 [test: crates/benchkit/tests/prose_claims.rs::claim_instance_required_fields].
`fail_to_pass` (JSONL キー `FAIL_TO_PASS`) は通るべきテスト、`pass_to_pass` (JSONL キー `PASS_TO_PASS`) は通り続けるべきテストの一覧で、どちらかの ID が失敗すると instance は unresolved になる。 [test: crates/benchkit/tests/prose_claims.rs::claim_f2p_p2p_semantics].
モデルは serde の `rename` で大文字キー `FAIL_TO_PASS` / `PASS_TO_PASS` を snake_case Rust フィールドへ写す。 [test: crates/benchkit/tests/loader.rs::loads_vendored_fixture_into_instances].
fixture はこれらを素の文字列 JSON リストで持つ。 [test: crates/benchkit/tests/loader.rs::loads_vendored_fixture_into_instances].
`download` は huggingface.co の上流 `.parquet` ファイルをそのままのバイト列でキャッシュパスへ書き、JSONL へは変換しない。 [test: crates/benchkit/tests/prose_claims.rs::claim_download_shape].

```sh
benchkit load crates/benchkit/tests/fixtures/instances.jsonl

benchkit download                 # .benchkit-cache/swe-bench-verified.jsonl へ書く [test: crates/benchkit/tests/prose_claims.rs::claim_download_default_dest]
benchkit download --dest data/verified.jsonl   # --dest でキャッシュファイルのパスを指定する [test: crates/benchkit/tests/prose_claims.rs::claim_download_dest_flag]
benchkit download --force         # キャッシュがあっても再取得 [test: crates/benchkit/tests/prose_claims.rs::claim_download_force_refetch]

# --real 無しは dry run: git も pytest も走らず、テスト結果は生成されない。 [test: crates/benchkit/tests/prose_claims.rs::claim_dry_run]
benchkit run-instance <instances.jsonl> <instance_id>
benchkit run-instance <instances.jsonl> <instance_id> --real   # git clone/checkout/apply + python -m pytest [test: crates/benchkit/tests/prose_claims.rs::claim_real_path]

benchkit audit-sample <changes.jsonl> [--audits <verdicts.jsonl> | --from-violations] \
    [--fraction 0.1] [--seed 0] [--json]
```

`run-instance --real` は `git clone`、`git checkout <base_commit>`、`test_patch` の `git apply`、`FAIL_TO_PASS` と `PASS_TO_PASS` の ID を対象にした `python -m pytest` を順に実行し、pytest 出力の解析結果を採点する。 [test: crates/benchkit/tests/prose_claims.rs::claim_real_path].
結果マップに無いテスト ID は「通っていない」と数える。 [test: crates/benchkit/src/scorer.rs::missing_test_id_is_fail_closed].

`audit-sample` は seed 固定の決定論的サンプルを引く。 [test: crates/benchkit/src/auditsample.rs::sample_is_deterministic_for_a_seed].
判定 (`--audits`) または実際の overwatch violation ストリーム (`--from-violations`) を与えると、見逃しを新 invariant 候補と閾値調整の批准キューへ振り分ける。 [test: crates/benchkit/src/auditsample.rs::a_miss_emits_both_feedback_paths] [test: crates/benchkit/src/auditsample.rs::real_audit_cross_reference_match_is_miss_nonmatch_ignored] [test: crates/benchkit/tests/prose_claims.rs::claim_audit_sample_cli] [test: crates/benchkit/tests/prose_claims.rs::claim_from_violations_flag_routes].
`--audits` と `--from-violations` は同時に指定できない。 [test: crates/benchkit/tests/prose_claims.rs::claim_audit_sample_cli].

`download` はキャッシュ存在時は `--force` が無い限り no-op。 [test: crates/benchkit/src/download.rs::existing_cache_is_a_noop] [test: crates/benchkit/tests/prose_claims.rs::claim_download_force_refetch].
`[dependencies]` に HTTP クライアント crate を持たず、`curl` にハードタイムアウト (`--max-time 120`) 付きで shell out する。 [test: crates/benchkit/tests/prose_claims.rs::claim_download_curl_timeout].
