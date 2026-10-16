Claude Code の tool 呼び出しの同時実行数を、散文ではなく **binary が数える**上限 [test: crates/parallelguard/tests/concurrency.rs::eight_simultaneous_shells_admit_exactly_three]。

parallelguard は `acquire` を通過してまだ `release` されていない呼び出しを数え、上限を超える呼び出しを deny する [test: crates/parallelguard/src/model.rs::the_fourth_concurrent_shell_is_denied] [test: crates/parallelguard/tests/concurrency.rs::releasing_returns_the_slot]。

独立した 2 つのプールで、上限はそれぞれ **3**: [test: crates/parallelguard/src/model.rs::a_full_shell_pool_does_not_block_subagents] [test: crates/parallelguard/tests/concurrency.rs::eight_simultaneous_shells_admit_exactly_three] [test: crates/parallelguard/tests/concurrency.rs::eight_simultaneous_subagents_admit_exactly_three]

| プール | ツール | 上限 [test: crates/parallelguard/tests/concurrency.rs::eight_simultaneous_shells_admit_exactly_three] |
|---|---|---|
| shell | `Bash` | 同時 3 [test: crates/parallelguard/tests/concurrency.rs::eight_simultaneous_shells_admit_exactly_three] |
| subagent | `Task` / `Agent` | 同時 3 [test: crates/parallelguard/tests/concurrency.rs::eight_simultaneous_subagents_admit_exactly_three] [test: crates/parallelguard/src/model.rs::bash_and_task_draw_from_different_pools] |

2 つのプールを共有プール 1 本にまとめない: 2 プールなら shell が満杯でも subagent は止まらない [test: crates/parallelguard/tests/concurrency.rs::a_full_shell_pool_never_blocks_a_subagent]。

上限は上げられず、3 は **天井**であり、`HARNESS_MAX_PARALLEL` の
大きすぎる値・パース不能な値は天井に解決する [test: crates/parallelguard/tests/prose_claims.rs::claim_ceiling] [test: crates/parallelguard/tests/concurrency.rs::the_cap_can_be_lowered_but_not_raised]。
`HARNESS_MAX_PARALLEL` で**下げる**ことはできる [test: crates/parallelguard/tests/prose_claims.rs::claim_lower_cap]。
`0` は天井ではなく下限の 1 に引き上げられる [test: crates/parallelguard/tests/prose_claims.rs::claim_ceiling_zero_actual_behaviour]。

```
PreToolUse(Bash|Task|Agent)          -> parallelguard acquire   slot を取る / deny する [test: crates/parallelguard/tests/prose_claims.rs::claim_hooks_wiring] [test: crates/parallelguard/src/model.rs::the_fourth_concurrent_shell_is_denied]
PostToolUse(Bash|Task|Agent)         -> parallelguard release   slot を返す [test: crates/parallelguard/tests/prose_claims.rs::claim_hooks_wiring] [test: crates/parallelguard/tests/concurrency.rs::releasing_returns_the_slot]
SessionStart|UserPromptSubmit|Stop   -> parallelguard reset      台帳を消す [test: crates/parallelguard/tests/prose_claims.rs::claim_hooks_wiring] [test: crates/parallelguard/tests/concurrency.rs::reset_clears_a_leaked_ledger]
```

台帳はセッションごとに 1 つの JSON ファイル（`$HOME/.parallelguard/state/sessions/`、
絶対パスの `PARALLELGUARD_STATE_DIR` で変更可） [test: crates/parallelguard/tests/prose_claims.rs::claim_ledger_path] [test: crates/parallelguard/src/store.rs::a_relative_state_dir_override_is_ignored]。
並列 tool バッチの hook プロセスは同時に走るので、advisory lockfile で
read-modify-write を直列化し、上限を超えて通すことはない [test: crates/parallelguard/tests/prose_claims.rs::claim_lock_serializes] [test: crates/parallelguard/tests/concurrency.rs::eight_simultaneous_shells_admit_exactly_three]。

**沈黙は degrade として使えない**: 下の経路はどれも出力なしの exit 0 で終わらない [test: crates/parallelguard/tests/prose_claims.rs::claim_never_silent] [test: crates/parallelguard/tests/concurrency.rs::the_launcher_denies_when_no_binary_is_bundled]。
payload が壊れている / 台帳が読めない / lock が取れない / 書き込みが失敗する /
そのプラットフォームのビルドが無い — どれも **in-flight 数が不明**であり、不明は「空き」ではないので、
すべて deny に倒す（CLAUDE.md 第3節） [test: crates/parallelguard/tests/concurrency.rs::an_unparseable_payload_denies] [test: crates/parallelguard/tests/concurrency.rs::an_unreadable_ledger_denies_rather_than_reading_as_empty] [test: crates/parallelguard/tests/prose_claims.rs::claim_lock_held_denies] [test: crates/parallelguard/tests/prose_claims.rs::claim_write_failure_denies] [test: crates/parallelguard/tests/concurrency.rs::the_launcher_denies_when_no_binary_is_bundled]。

* 台帳は毎ターン境界の `reset` で消える [test: crates/parallelguard/tests/prose_claims.rs::claim_hooks_wiring] [test: crates/parallelguard/tests/concurrency.rs::reset_clears_a_leaked_ledger]
* 殺された hook が lock を塞ぎ続けることはない: lock は永続 lockfile 上の kernel advisory lock（`flock`）で、保持者が死ねば kernel が解放し、鮮度タイムアウトも steal も無い [test: crates/parallelguard/tests/prose_claims.rs::claim_killed_holder]

* slot は**時間で失効しない** [test: crates/parallelguard/src/model.rs::a_slot_never_expires_by_age]。
* `run_in_background: true` の `Bash` は特別扱いされない: 他の `Bash` と同じく `acquire` で slot を取り `release` で返すので、`release` 後も走り続けるバックグラウンドプロセスは数の外 [test: crates/parallelguard/tests/prose_claims.rs::claim_background_not_special]。

```sh
parallelguard status       # 実効の上限・セッションごとの count・直近の deny [test: crates/parallelguard/tests/prose_claims.rs::claim_status_contents]
parallelguard --protects   # PROTECTS / AGAINST / GROUNDS の保護宣言を表示 [test: crates/parallelguard/tests/protects_flag.rs::protects_prints_three_labelled_nonempty_lines_and_exits_zero]
```

`status` は、セッションの台帳がある間は（何も走っていなくても）プールごとの count を表示し、
台帳が無いときは「the gate is NOT running」の行を表示する [test: crates/parallelguard/tests/prose_claims.rs::claim_status_distinguishes]。
ターン境界の `reset` は台帳を消すので、`reset` 直後の `status` は、ゲートが動いていても同じ「NOT running」の行を表示する [test: crates/parallelguard/tests/prose_claims.rs::claim_status_after_reset]。
