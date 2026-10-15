`replaykit promote` は tracekit の実行トレース（`spans.jsonl`）を evalkit の golden ケースへ昇格させる [test: crates/replaykit/tests/prose_claims.rs::claim_intro_roundtrip]。その golden の `cmd` は昇格した fixture に対して `replaykit verify` を再実行し、昇格したままの fixture では 0、fixture のステップの status をエラーに変えると 1 で終了する [test: crates/replaykit/tests/prose_claims.rs::claim_intro_roundtrip]。

```sh
replaykit promote --run my-run-2026-06-28 --root . --dataset replayed
#    evals/replay/fixtures/<id>.json   （移植可能な要約）と
#    evals/replay/replayed.jsonl       （golden、同じ run を再昇格しても行は増えない）を書き出す [test: crates/replaykit/tests/prose_claims.rs::claim_promote_writes_fixture_and_dedup]
```

`--spans` を指定しない場合、`replaykit extract --run <RID>` は `~/.tracekit/<sanitize(RID)>/spans.jsonl` から span を読む [test: crates/replaykit/tests/prose_claims.rs::claim_extract_default_spans_path]。
`--spans <path>` を指定するとその path から span を読む [test: crates/replaykit/tests/prose_claims.rs::claim_extract_spans_override]。
`--out <path|->` の既定は `-` で、整形 JSON のトラジェクトリ要約を標準出力へ出力する [test: crates/replaykit/tests/prose_claims.rs::claim_extract_out_default_stdout]。
壊れた span 行はスキップされる [test: crates/replaykit/src/trace.rs::load_skips_malformed_and_counts_them]。

`replaykit verify <fixture.json>` は管理対象の fixture の phase [test: crates/replaykit/src/verify.rs::phase_mismatch_fails]・エラー件数 [test: crates/replaykit/src/verify.rs::error_step_over_ceiling_fails]・総コスト [test: crates/replaykit/src/verify.rs::cost_over_cap_fails] をステップから再計算し、`expect` ブロックと照合する。
総コストは `expect` がコスト上限を固定している場合にだけ照合される [test: crates/replaykit/src/verify.rs::no_cap_means_cost_is_not_checked]。
違反は標準エラーへ出力される [test: crates/replaykit/tests/prose_claims.rs::claim_verify_violations_to_stderr]。

`replaykit promote --run <RID> [--spans <path>] [--root <dir>] [--evals-dir <name>] [--dataset <name>] [--draft]` は要約を組み立て、`<root>/<evals_dir>/replay/fixtures/<id>.json` に書き出し、golden 行を `<root>/<evals_dir>/replay/<sanitize(dataset)>.jsonl` に追記する（同じ run を再昇格しても行は増えない）[test: crates/replaykit/tests/prose_claims.rs::claim_promote_paths_and_sanitized_dataset] [test: crates/replaykit/tests/prose_claims.rs::claim_promote_appends_distinct_runs]。
`--evals-dir` が既定のとき、golden の `cmd` は `["replaykit","verify","evals/replay/fixtures/<id>.json"]` で、fixture パスは root からの相対になる [test: crates/replaykit/tests/prose_claims.rs::claim_golden_cmd_relative_to_root]。
既定値は `--root` が CWD、`--evals-dir` が `evals`、`--dataset` が `replayed` [test: crates/replaykit/tests/prose_claims.rs::claim_promote_defaults]。
`--draft` は受け付けるが現状は**何もしない** [test: crates/replaykit/tests/prose_claims.rs::claim_draft_is_noop]。

- `0`: 再生が固定済みの不変条件と一致した（pass）[test: crates/replaykit/tests/integration.rs::verify_healthy_fixture_passes]
- `1`: 本物の回帰 / 不変条件違反 [test: crates/replaykit/tests/prose_claims.rs::claim_exit_1_regression]
- `2`: ハーネスエラー（fixture または spans ファイルの欠落・読み取り不能、あるいは要約 JSON として不正な fixture）[test: crates/replaykit/tests/prose_claims.rs::claim_exit_2_harness_error]

素の CLI の**ゲート**であり、ライフサイクル hook ではない [test: crates/replaykit/tests/prose_claims.rs::claim_not_a_lifecycle_hook]。

`[dependencies]` テーブルにあるのは `harness-core`・`serde`・`serde_json`・`clap` だけで、自身の `src/` には `TcpStream`・`UdpSocket`・`reqwest`・`ureq`・`hyper`・`API_KEY` への参照が無い [test: crates/replaykit/tests/prose_claims.rs::claim_deps_no_network_no_api_key]。

このリポジトリに `.github/` の CI は存在せず、`.githooks/` 直下のどのファイルも `evalkit` も `replaykit` も含まない [test: crates/replaykit/tests/prose_claims.rs::claim_no_ci_wiring]。
