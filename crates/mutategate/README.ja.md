この crate の存在を coverage と読まないこと。

ホスト型 CI からこのゲートを起動しない。再結線は §7 に従い local（pre-push または明示スクリプト）で行うこと。

`.githooks/` 配下のどの git hook も `scripts/mutation-gate.sh` を参照せず、`.github/workflows/` ディレクトリも無いため、kill-rate ゲートは INERT であり、人間が手で叩いたときだけ動く。 [test: crates/mutategate/tests/prose_claims.rs::claim_inert_no_trigger]

内部ツールであり、配布される Claude Code プラグインではない（`plugin.json` を持たない）。 [test: crates/mutategate/tests/prose_claims.rs::claim_not_a_plugin_a] [test: crates/mutategate/tests/prose_claims.rs::claim_not_a_plugin_b]

背景として Meta の Automated Compliance Hardening (ACH) と PRIMG (arXiv:2505.05584) を参照すること。

このゲートは `cargo-mutants` の `outcomes.json` をパースし、kill-rate を計算し、閾値を下回ったら非ゼロで exit する。 [test: crates/mutategate/tests/overwatch_violation.rs::fail_outcome_appends_a_signed_nonempty_violation]

ここで用いる kill-rate の定義: [test: crates/mutategate/src/lib.rs::viable_and_killed_exclude_unviable] [test: crates/mutategate/src/lib.rs::kill_rate_is_killed_over_viable] [test: crates/mutategate/src/lib.rs::unknown_summary_does_not_inflate_kill_rate] [test: crates/mutategate/src/lib.rs::no_viable_mutants_fails_the_gate]

```
viable   = caught + missed + timeout + unknown   (unviable な mutant は除外 — シグナルなし)
killed   = caught + timeout                      (timeout はテストが露出させた誤動作)
kill_rate = killed / viable                      (未定義 -> ゲートは失敗)
```

`unknown` は、この crate が認識しない `summary` 状態の mutant、または `summary` を欠く record。 [test: crates/mutategate/src/lib.rs::unknown_summary_states_are_tracked_not_dropped]

`scenario` が `"Baseline"` でも `Mutant` オブジェクトでもない record（null・欠落を含む）も `unknown` に数える。 [test: crates/mutategate/src/lib.rs::ca_mutategate_03_null_scenario_dropped_flips_fail_to_pass]

`viable` には数えるが `killed` には数えないので、未知の状態は分母から消えて kill-rate を水増しするのではなく、生き残った mutant として扱われる。 [test: crates/mutategate/src/lib.rs::unknown_summary_does_not_inflate_kill_rate]

`unknown` が 1 件でもあれば理由文に警告が付く。 [test: crates/mutategate/tests/prose_claims.rs::claim_unknown_warning_in_reason_a] [test: crates/mutategate/tests/prose_claims.rs::claim_unknown_warning_in_reason_b] [test: crates/mutategate/tests/prose_claims.rs::claim_unknown_warning_in_reason_c_no_unknown_no_warning]

```sh
# 既存の outcomes.json に対するゲート [test: crates/mutategate/tests/overwatch_violation.rs::fail_outcome_appends_a_signed_nonempty_violation]
cargo run -p mutategate -- --outcomes mutants.out/outcomes.json --min-kill-rate 0.80

# エンドツーエンド: パイロットクレートでエンジンを走らせてからゲート [test: scripts/test_mutation_gate_status.sh] [test: crates/mutategate/tests/prose_claims.rs::claim_pilot_default_harness_core]
scripts/mutation-gate.sh
PILOT=difflog MIN_KILL_RATE=0.70 scripts/mutation-gate.sh
PILOT=specguard scripts/mutation-gate.sh
PILOT=condukt scripts/mutation-gate.sh
PILOT=blastguard scripts/mutation-gate.sh

# このゲートが何を・何から・何を根拠に守るかを、判定なしで表示 [test: crates/mutategate/tests/protects_flag.rs::protects_prints_three_labelled_nonempty_lines_and_exits_zero]
cargo run -p mutategate -- --protects
```

既定値: `--outcomes mutants.out/outcomes.json`、`--min-kill-rate 0.80`。 [test: crates/mutategate/tests/prose_claims.rs::claim_defaults_a_exactly_080_passes_with_default_path] [test: crates/mutategate/tests/prose_claims.rs::claim_defaults_b_075_fails_with_default_path] [test: crates/mutategate/tests/prose_claims.rs::claim_defaults_c_default_path_is_the_one_read]

`1e-9` 以下（0・負値）の `--min-kill-rate` は exit `2` で拒否される。 [test: crates/mutategate/src/lib.rs::negative_min_kill_rate_is_rejected] [test: crates/mutategate/src/lib.rs::sub_epsilon_min_kill_rate_is_rejected_it_still_disables_the_gate] [test: crates/mutategate/tests/overwatch_violation.rs::zero_min_kill_rate_is_rejected_by_the_cli_before_evaluating] [test: crates/mutategate/tests/overwatch_violation.rs::sub_epsilon_min_kill_rate_is_rejected_by_the_cli]

Exit コード: `0` pass、`1` kill-rate が閾値未満（または生存可能な mutant が無い）、`2` usage/IO/parse エラー。 [test: crates/mutategate/tests/prose_claims.rs::claim_exit_codes_a_pass_is_0] [test: crates/mutategate/tests/prose_claims.rs::claim_exit_codes_b_below_threshold_is_1] [test: crates/mutategate/tests/prose_claims.rs::claim_exit_codes_c_no_viable_mutants_is_1] [test: crates/mutategate/tests/prose_claims.rs::claim_exit_codes_d_missing_file_is_2] [test: crates/mutategate/tests/prose_claims.rs::claim_exit_codes_e_malformed_json_is_2] [test: crates/mutategate/tests/prose_claims.rs::claim_exit_codes_f_unknown_flag_is_2]

FAIL 時は overwatch の fleet violation ledger に違反イベントも追記する。 [test: crates/mutategate/tests/overwatch_violation.rs::fail_outcome_appends_a_signed_nonempty_violation]

この書き込みに失敗した場合は注記を出すが、FAIL の判定はそのまま維持される。 [test: crates/mutategate/tests/overwatch_violation.rs::emit_failure_does_not_gate_the_gate_but_is_announced]

`scripts/mutation-pilots.sh` は変更パスの一覧（stdin）を、走らせるべき較正済みパイロットクレートに写像する。 [test: scripts/test_mutation_pilots.sh]

どれにも当たらなければ `harness-core` に倒し、空の一覧は決して出さない。 [test: scripts/test_mutation_pilots.sh]

- パイロット1: `harness-core`（既定） [test: crates/mutategate/tests/prose_claims.rs::claim_pilot_default_harness_core]。
- パイロット2: `specguard` — 既定で `--file crates/specguard/src/similarity.rs` に絞り込み、`ack_blocks_when_no_new_commits_since_raised` という1テストを `--skip` する [test: crates/mutategate/tests/prose_claims.rs::claim_pilot_specguard_default_a_file] [test: crates/mutategate/tests/prose_claims.rs::claim_pilot_specguard_default_b_skip]。
- パイロット3: `condukt` — 既定で `--file crates/condukt/src/circuit.rs` に絞り込む [test: crates/mutategate/tests/prose_claims.rs::claim_pilot_condukt_default]。
- パイロット4: `blastguard` — 既定で `--file crates/blastguard/src/classify.rs` に絞り込む [test: crates/mutategate/tests/prose_claims.rs::claim_pilot_blastguard_default]。
- `PILOT=<crate>` で任意のクレートに切り替えられる [test: crates/mutategate/tests/prose_claims.rs::claim_mutants_extra_overrides_a_arbitrary_pilot]。`MUTANTS_EXTRA`（例: `MUTANTS_EXTRA="--file crates/harness-core/src/hash.rs"`）は cargo-mutants にそのまま渡され、スクリプトは呼び出し元のディレクトリに関係なく repo root から cargo-mutants を走らせる [test: crates/mutategate/tests/prose_claims.rs::claim_mutants_extra_overrides_b_path_passed_verbatim_cwd_is_repo_root] [test: crates/mutategate/tests/prose_claims.rs::claim_mutants_extra_overrides_b2_cwd_independent_of_caller_cwd]。空でない `MUTANTS_EXTRA` を指定すると、パイロット別の既定値は完全に上書きされる [test: crates/mutategate/tests/prose_claims.rs::claim_mutants_extra_overrides_c_explicit_extra_replaces_default]。

[record: 2026-09-11 rev: 89b31bb4] blastguard と同時に計測した GATE クレートのうち stuckguard / propguard はバーに届かず、overwatch はベースライン失敗で計測できなかったため、3 つともパイロットにしなかった。

クレートは 1 つずつ、それぞれが既に閾値をクリアしてから追加すること。

スイートが硬くなるにつれ `MIN_KILL_RATE` を引き上げる [test: crates/mutategate/tests/prose_claims.rs::claim_min_kill_rate_env_a_passed_to_mutategate] [test: crates/mutategate/tests/prose_claims.rs::claim_min_kill_rate_env_a2_default_is_080_and_fails_same_data]。

スクリプトは cargo-mutants に `--output target/mutants-<pilot>` を渡し、`target/mutants-<pilot>/mutants.out/outcomes.json` を採点する。 [test: crates/mutategate/tests/prose_claims.rs::claim_min_kill_rate_env_b_output_dir_per_pilot]

[record: 2026-09-11 rev: 89b31bb4] この時点では `scripts/mutation-gate.sh` を実行する箇所は無く、kill-rate ゲートは人間が手で叩いたときだけ動いた。
