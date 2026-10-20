---
name: flow
description: **ユーザーが明示的に起動したときだけ走る**（SessionStart の自動提案を廃止したため、こちらから提案することはしない） [test: scripts/test_prose_flow_a.py::T::test_claim_no_propose_no_hook_runs_flow] [test: scripts/test_prose_flow_a.py::T::test_claim_no_propose_autoflow_has_no_sessionstart_and_no_proposal_strings] [test: scripts/test_prose_flow_a.py::T::test_claim_no_propose_autoflow_stop_output_never_proposes_flow] [test: scripts/test_prose_flow_a.py::T::test_claim_no_propose_docs_say_explicit_invocation_only]。課題の供給（compass の次の一手 / backlog のキュー）から解決手段の実行までを1本のループで回したいときに使うこと。
argument-hint: "[任意: 直接の課題文。省略時は compass→backlog から自動でピック]"
allowed-tools: Task, AskUserQuestion, Bash(backlog:*), Bash(compass:*), Bash(condukt:*), Bash(fugu-router:*), Bash(hypothesis:*), Bash(overwatch:*), Bash(budgetguard:*), Bash(specguard:*), Bash(specforge:*), Bash(jq:*), Bash(printf:*), Bash(git:*), Read
---

```
SOURCE（課題の供給。上から順に引く）   EXECUTOR（解決手段の実行）
  backlog    … 確定済みキュー【主経路】─┐
  hypothesis … 計測待ちの PDO 仮説      ├─▶  condukt（fugu-router がモデル選択）─▶ verify
  compass    … 次の右サイズの一手       │    ※backlog が空のときだけ引く（自動 carve）
  prompt     … ユーザー直の課題文      ─┘
                                     │
  specforge  … spec-gap の戻り経路   ◀┘  正典が無い課題は実装フェーズへ通さず
       └──▶ ratify（exit 9 なら人間へ返す）
```

## 自分から提案しない

**自分から `/flow` を提案してはならない [test: scripts/test_prose_flow_a.py::T::test_claim_no_propose_no_hook_runs_flow] [test: scripts/test_prose_flow_a.py::T::test_claim_no_propose_autoflow_has_no_sessionstart_and_no_proposal_strings] [test: scripts/test_prose_flow_a.py::T::test_claim_no_propose_autoflow_stop_output_never_proposes_flow]。** [record: 2026-08-20 rev: 5289312f] 0.2.6 までは SessionStart hook が
「開いている仕事があれば AskUserQuestion で `/flow` を確認せよ」というディレクティブを
注入していたが（L2: propose-then-confirm）、ユーザーの指示で廃止した
（autoflow 側の同種の 2 経路も同時に撤去）。backlog に pending が積まれていることだけを理由に起動してはならない。
廃止したディレクティブを推測で再現してはならない。

### Step 0 — 引数があれば課題文へ直行すること

乗り換えはしない。

`$ARGUMENTS` に課題文があれば Step 3（その課題文で condukt 実行）へ直行せよ [test: scripts/test_prose_flow_a.py::T::test_claim_frontmatter_declares_args_and_tools]。ループはせず1件だけ実行して終了せよ。
引数が空なら Step 1 へ進むこと。

どちらの経路でも、着手した課題が終わるまでそれだけをやること。
途中で見つけた別の問題は再現テスト（RED）を書いて `backlog add --repro-test "<cmd>"` で起票し、**その課題に戻ること** [test: crates/backlog/tests/repro_evidence.rs::reproduced_lands_pending_and_records_outcome]。
テスト無しの `backlog add` は拒否され store に何も書かれない [test: crates/backlog/tests/no_confirm_no_unproven.rs::add_without_repro_writes_nothing_anywhere]。
下の「常時起票」を参照すること。

### Step 0.5 — 自律モードでは人間への問い合わせをゲート単位で判定に通すこと

自律モードでも、budgetguard の予算超過による早期脱出（Step 4）を省いてはならない [test: scripts/test_prose_flow_b.py::TestBudgetCheck::test_claim_budget_over_is_nonzero] [test: scripts/test_prose_flow_b.py::TestBudgetCheck::test_claim_budget_corrupt_ledger_is_nonzero] [test: scripts/test_prose_flow_b.py::TestBudgetCheck::test_claim_budget_corrupt_config_is_nonzero] [test: scripts/test_prose_flow_b.py::TestBudgetCheck::test_claim_budget_missing_binary_is_nonzero] [test: scripts/test_prose_flow_b.py::TestBudgetCheck::test_claim_budget_prose_nonzero_goes_to_step_4]。

ループ中に人間へ問い合わせる（`AskUserQuestion`）箇所は、**自律モードでは各ゲート固有の
risk×reversibility×confidence を `condukt policy answer` に渡し、決定論的 verdict に従って
自答／エスカレート／拒否する** [test: crates/condukt/tests/policy_cli.rs::auto_verdict_self_answers_recommended_option_and_journals] [test: crates/condukt/tests/policy_cli.rs::escalate_verdict_falls_through_but_is_still_journaled] [test: crates/condukt/tests/policy_cli.rs::block_verdict_is_a_hard_stop_exit_3]。

まず `condukt state autonomy-check` で、そもそも縮退してよいかを確認せよ [test: crates/condukt/tests/autonomy_invariant.rs::autonomy_check_true_config_exits_zero_and_prints_true] [test: crates/condukt/tests/autonomy_invariant.rs::autonomy_check_missing_config_defaults_to_not_autonomous]。

```bash
condukt state autonomy-check   # exit 0 + {"autonomous":true} → 自律 / exit 1 + {"autonomous":false} → 非自律 [test: crates/condukt/tests/autonomy_invariant.rs::autonomy_check_true_config_exits_zero_and_prints_true] [test: crates/condukt/tests/autonomy_invariant.rs::autonomy_check_false_config_exits_one_and_prints_false]
```

- **exit 1（非自律・既定）** → 従来どおり全ゲートで `AskUserQuestion` を出せ [test: crates/condukt/tests/autonomy_invariant.rs::autonomy_check_false_config_exits_one_and_prints_false] [test: crates/condukt/tests/autonomy_invariant.rs::autonomy_check_missing_config_defaults_to_not_autonomous]。
- `autonomy-check` が存在しない版や、exit 0 以外で終わる場合（存在しない版は unrecognized subcommand で exit 2 になる）→ **非自律とみなす**（安全側フォールバック＝全 Ask） [test: scripts/test_prose_flow_a.py::T::test_claim_autonomy_unknown_subcommand_never_exits_zero]。
- **exit 0（autonomous）** なら各ゲートを次の `condukt policy answer` に通せ [test: crates/condukt/tests/autonomy_invariant.rs::autonomy_check_true_config_exits_zero_and_prints_true]。

```bash
# 各 human gate はまずこの shim を通すこと。exit code で 自答 / 従来 Ask / 拒否 を分岐する [test: crates/condukt/tests/policy_cli.rs::auto_verdict_self_answers_recommended_option_and_journals] [test: crates/condukt/tests/policy_cli.rs::escalate_verdict_falls_through_but_is_still_journaled] [test: crates/condukt/tests/policy_cli.rs::block_verdict_is_a_hard_stop_exit_3]
OUT=$(condukt policy answer \
        --risk <low|medium|high> --reversible <low|medium|high> --confidence <low|medium|high> \
        --question "<質問文>" --option "<A>" --option "<B>" --recommend <既定 index> 2>/dev/null)
case $? in
  0) CHOSEN=$(printf '%s' "$OUT" | jq -r '.chosen') ;;  # auto なら CHOSEN を採用し Ask しない。自答は監査ログに追記される [test: crates/condukt/tests/policy_cli.rs::auto_verdict_self_answers_recommended_option_and_journals]
  2) : ;;  # escalate なら従来どおり AskUserQuestion を出せ [test: crates/condukt/tests/policy_cli.rs::escalate_verdict_falls_through_but_is_still_journaled]
  3) : ;;  # block なら実行を拒否して停止し、人にも聞かない [test: crates/condukt/tests/policy_cli.rs::block_verdict_is_a_hard_stop_exit_3]
  *) : ;;  # 上記以外（1=不正入力 / 127 など）は安全側 = AskUserQuestion を出せ [test: crates/condukt/tests/policy_cli.rs::unparseable_level_exits_1_without_panic]
esac
```

- **exit 0（auto）** → stdout（`{"answered":true,"policy":"auto","chosen":"..."}`）の `chosen` を採用し、**Ask しない** [test: crates/condukt/tests/gate_decision_journaling.rs::control_auto_still_journals_the_chosen_option_and_prints_its_json]。
  この自答は `gate-decisions.jsonl` に追記され、**`condukt policy answers` で後から監査できる** [test: crates/condukt/tests/policy_cli.rs::answers_reader_replays_the_audit_trail]。
- **exit 2（escalate）** → 従来どおり `AskUserQuestion` を出せ [test: crates/condukt/tests/policy_cli.rs::escalate_verdict_falls_through_but_is_still_journaled] [test: crates/condukt/tests/autonomy_invariant.rs::policy_answer_baseline_escalates_the_ambiguous_middle]。
- **exit 3（block）** → 実行を拒否して停止する [test: crates/condukt/tests/policy_cli.rs::block_verdict_is_a_hard_stop_exit_3]。
- **その他（exit 1 不正入力 / exit 127 など）** → 安全側にフォールバックして `AskUserQuestion` を出せ [test: crates/condukt/tests/policy_cli.rs::unparseable_level_exits_1_without_panic] [test: crates/condukt/tests/policy_cli.rs::recommend_index_out_of_range_exits_1]。

#### 権限認可と判断要求（Ask）を分けること

自律走行中の「進めてよいか」という YES・NO の権限認可は、二度と人間に聞いてはならない。
判断要求（人間に選択・設計判断を求める Ask）のゲートに `--approval` を付けてはならない [test: scripts/test_prose_flow_a.py::T::test_claim_judgment_approval_flag_is_exactly_what_erases_the_question] [test: scripts/test_prose_flow_a.py::T::test_claim_judgment_no_approval_on_judgment_gates_in_skill_text]。迷ったときも付けてはならない。
verdict をこの skill の中で hardcode してはならない。

[record: 2026-08-07 rev: 056c51dc] ユーザー常設許諾: 自律走行中の「進めてよいか」という YES/NO の権限認可は事前に許諾済み。

人間に聞くのは、選択・設計判断を求める Ask だけにすること。
権限認可のゲート（はい/いいえで答えられ、答えが「はい」だと分かっているもの）には `--approval` を付けること [test: crates/condukt/tests/autonomy_invariant.rs::policy_answer_approval_self_answers_when_autonomous]。

```bash
condukt policy answer --approval \
  --risk <...> --reversible <...> --confidence <...> \
  --question "<進めてよいか>" --option "<進む>" --option "<やめる>" --recommend 0
```

1. **`block` は絶対に緩まない** — risk=high かつ reversible=low の hard stop は `--approval` を付けても block のまま [test: crates/condukt/tests/autonomy_invariant.rs::policy_answer_approval_never_relaxes_the_irreversible_block] [test: crates/condukt/src/policy.rs::approval_clamps_escalate_to_auto_and_never_relaxes_block]。
2. **非自律モードでは完全に不活性** — 自律判定はバイナリ側が行うので、`--approval` を付けても非自律なら escalate のまま [test: crates/condukt/tests/autonomy_invariant.rs::policy_answer_approval_is_inert_when_not_autonomous]。
3. **callsite ごとの opt-in** — `--approval` を付けなかったゲートは自律でも escalate する [test: crates/condukt/tests/autonomy_invariant.rs::policy_answer_baseline_escalates_the_ambiguous_middle]。
4. **`--untestable` / `--conflict` が常に優先** — `--approval` と併記しても escalate する [test: crates/condukt/tests/autonomy_invariant.rs::policy_answer_untestable_beats_approval] [test: crates/condukt/tests/autonomy_invariant.rs::policy_answer_conflict_beats_approval]。

| human gate | 種別 | risk | reversible | confidence | `--approval` | 典型 verdict | 自答時の既定（recommend） | [test: scripts/test_prose_flow_a.py::T::test_claim_policy_table_header_and_rows_present] [test: scripts/test_prose_flow_a.py::T::test_claim_policy_table_rows_verdicts]
|---|---|---|---|---|---|---|---|
| **排他ロック競合**（Step 2・他セッションが明示的に `lock acquire` 済み。driver の並走では発生しない） | 権限認可 | low | high | high | 付ける | auto | **stand down**（報告して clean exit。`--force` 自動奪取はしない） | [test: scripts/test_prose_flow_a.py::T::test_claim_policy_table_header_and_rows_present] [test: scripts/test_prose_flow_a.py::T::test_claim_policy_table_rows_verdicts] [test: scripts/test_prose_flow_a.py::T::test_claim_policy_table_auto_picks_the_recommended_option]
| **resume 選択**（複数候補） | 権限認可 | low | high | high | 付ける | auto | 3-1 の優先度 pick 規則の先頭 | [test: scripts/test_prose_flow_a.py::T::test_claim_policy_table_header_and_rows_present] [test: scripts/test_prose_flow_a.py::T::test_claim_policy_table_rows_verdicts]
| **deploy/push の GATED 承認**（3-3 sink・Step 4） | 権限認可 | medium | medium | medium | **付ける** | auto（常設許諾） | **承認して進む**（block を返した場合のみ停止） | [test: scripts/test_prose_flow_a.py::T::test_claim_policy_table_header_and_rows_present] [test: scripts/test_prose_flow_a.py::T::test_claim_policy_table_rows_verdicts] [test: scripts/test_prose_flow_a.py::T::test_claim_policy_table_block_is_never_relaxed_by_approval]
| **condukt Phase 3 の合意**（「この schedule で進む?」） | 権限認可 | schedule 由来 | high | schedule 由来 | **付ける** | auto | 提示した schedule のまま進む | [test: scripts/test_prose_flow_a.py::T::test_claim_policy_table_header_and_rows_present] [test: scripts/test_prose_flow_a.py::T::test_claim_policy_table_rows_verdicts] [test: scripts/test_prose_flow_a.py::T::test_claim_policy_table_phase3_every_combination_auto_with_approval]
| **pivot-check**（Step 4・`pivot`） | **判断要求** | medium | high | low | 付けない | **escalate** | —（genuine な戦略判断なので人に聞く。既定案＝継続/persevere） | [test: scripts/test_prose_flow_a.py::T::test_claim_policy_table_header_and_rows_present] [test: scripts/test_prose_flow_a.py::T::test_claim_policy_table_rows_verdicts] [test: scripts/test_prose_flow_a.py::T::test_claim_judgment_pivot_gate_no_approval_escalates_and_recommends_continue]
| **worker が blocked**（condukt Phase 5） | **判断要求** | medium | medium | low | 付けない | **escalate** | —（実装が詰まった＝人間の判断が要る） | [test: scripts/test_prose_flow_a.py::T::test_claim_policy_table_header_and_rows_present] [test: scripts/test_prose_flow_a.py::T::test_claim_policy_table_rows_verdicts]
| **spec-gap divert**（3-1.5・open_questions / confidence:low / brief verdict=not-covered） | **判断要求** | medium | high | low | 付けない | **escalate** | —（「正典が無いまま実装させるか」は設計判断。3値は R4 の acceptance が固定） | [test: scripts/test_prose_flow_a.py::T::test_claim_policy_table_header_and_rows_present] [test: scripts/test_prose_flow_a.py::T::test_claim_policy_table_rows_verdicts] [test: scripts/test_prose_flow_a.py::T::test_claim_judgment_divert_command_as_written_escalates_and_is_journaled]
| **merge conflict の pick-a-side** | **判断要求** | — | — | — | `--conflict` | **escalate** | —（自動 pick は last-writer-wins） | [test: scripts/test_prose_flow_a.py::T::test_claim_policy_table_header_and_rows_present] [test: scripts/test_prose_flow_a.py::T::test_claim_policy_table_rows_verdicts] [test: scripts/test_prose_flow_a.py::T::test_claim_policy_table_conflict_and_untestable_escalate_even_with_approval]
| **測れない決定**（CLAUDE.md §2） | **判断要求** | — | — | — | `--untestable` | **escalate** | —（測れないという事実こそ人間が知るべき情報） | [test: scripts/test_prose_flow_a.py::T::test_claim_policy_table_header_and_rows_present] [test: scripts/test_prose_flow_a.py::T::test_claim_policy_table_rows_verdicts] [test: scripts/test_prose_flow_a.py::T::test_claim_policy_table_conflict_and_untestable_escalate_even_with_approval]
| **charter 彫り直し**（3-1 の 3・backlog が一手も出さず charter からも一手が引けない） | **判断要求** | — | — | — | 付けない | **人に聞く（自答しない）** | —（ゴールの中身を決めるのは人間） [record: 2026-10-01 rev: 9e3c987d] [test: scripts/test_prose_flow_a.py::T::test_claim_policy_table_charter_recarve_is_never_self_answered] |
| **循環ブレーカー trip**（早期脱出・`condukt circuit check`） | 決定論 stop | — | — | — | — | **人にも policy にも聞かない clean stop** | —（ループを止め Step 4 へ） | [test: scripts/test_prose_flow_a.py::T::test_claim_policy_table_header_and_rows_present] [test: scripts/test_prose_flow_a.py::T::test_claim_policy_table_rows_verdicts] [test: scripts/test_prose_flow_a.py::T::test_claim_policy_table_circuit_trip_exits_nonzero]

### Step 1 — ここで compass / charter を引いてはならない

引数なしの `/flow` では、compass / charter を先に引いてはいけない [test: scripts/test_prose_flow_a.py::T::test_claim_step1_order_backlog_before_compass]。この Step で `compass gap` を実行してはならない [test: scripts/test_prose_flow_a.py::T::test_claim_step1_order_backlog_before_compass]。
確定キューである backlog を先に処理し、backlog が一手も出さなかったときだけ charter 側へ降りること。
Step 2（driver 登録）→ Step 3-1 へ進むこと。

### Step 2 — driver を非排他に登録すること

キュー全体をロックしない [test: crates/integration-tests/tests/flow_skill_queue_contract.rs::the_loop_does_not_acquire_the_exclusive_project_lock]。
登録と解除を飛ばしてはならない。
同じ project のキューは複数セッションが同時に回してよい [test: crates/backlog/tests/concurrent_drivers.rs::two_concurrent_driver_processes_get_disjoint_tasks]。
task 単位の排他は `backlog next --claim` が選択と予約を同一クリティカルセクションで行うことで保証される [test: crates/backlog/tests/concurrent_drivers.rs::two_concurrent_driver_processes_get_disjoint_tasks]。

自分が driver であることを非排他に登録せよ（他の driver が居ても失敗しない） [test: crates/backlog/tests/concurrent_drivers.rs::lock_status_answers_liveness_for_zero_one_and_many_drivers]。

```bash
backlog driver register --session-id "$CLAUDE_CODE_SESSION_ID" --project "$PWD"
backlog lock status --project "$PWD"   # 参考: いま誰が driver か（drivers[] に全員が並ぶ） [test: crates/backlog/tests/concurrent_drivers.rs::lock_status_answers_liveness_for_zero_one_and_many_drivers]
```

> 登録は project ごとにスコープされる（`~/.backlog/drivers/<project のハッシュ>/<session>.driver`） [test: crates/backlog/tests/concurrent_drivers.rs::another_projects_driver_does_not_make_my_project_look_busy] [test: crates/backlog/tests/concurrent_drivers.rs::a_stale_driver_registration_is_reaped_like_a_stale_lock]。
> `--project` を付けずに `backlog lock status` を呼ぶと全 project 横断で driver の有無を返す [test: crates/backlog/tests/concurrent_drivers.rs::another_projects_driver_does_not_make_my_project_look_busy]。
> `/flow` は必ず `--project "$PWD"` を付けること [test: crates/backlog/tests/concurrent_drivers.rs::another_projects_driver_does_not_make_my_project_look_busy]。

- 他セッションが driver として登録済みでも、見送らない [test: crates/integration-tests/tests/flow_skill_queue_contract.rs::a_peer_driver_is_not_a_reason_to_stand_down]。並走して構わない [test: crates/backlog/tests/concurrent_drivers.rs::two_concurrent_driver_processes_get_disjoint_tasks]。待つのは解ではない [test: crates/integration-tests/tests/flow_skill_queue_contract.rs::a_peer_driver_is_not_a_reason_to_stand_down]。
- **例外 — 誰かが明示的に排他ロックを取っている場合**（`backlog lock status` の `kind` が `exclusive-lock` で `stale` でない）は尊重し、Step 0.5 の policy-answer routing に通せ [test: crates/backlog/src/liveness.rs::an_active_exclusive_lock_still_wins_and_keeps_its_shape] [test: crates/backlog/tests/lock_heartbeat_ttl.rs::heartbeat_refreshes_a_ttl_exceeded_lock_and_keeps_it_from_being_stolen]。
  権限認可なので `--approval` を付け、`--risk low --reversible high --confidence high` で渡せ（既定 verdict は auto） [test: crates/condukt/src/policy.rs::anchor_auto_trivially_safe_and_reversible] [test: crates/condukt/src/policy.rs::approval_clamps_escalate_to_auto_and_never_relaxes_block]。
  - auto（exit 0）でも自動奪取はしない。`chosen`（＝stand down）を採用し、「排他ロック保持中のため見送り」と報告して clean exit せよ [test: crates/condukt/tests/gate_decision_journaling.rs::control_auto_still_journals_the_chosen_option_and_prints_its_json]。自答は監査ログに残る [test: crates/condukt/tests/policy_cli.rs::auto_verdict_self_answers_recommended_option_and_journals]。
  - escalate（exit 2）なら従来どおり
    `AskUserQuestion`（待機 / 強制奪取 `--force` / 中止）を出せ [test: crates/condukt/tests/policy_cli.rs::escalate_verdict_falls_through_but_is_still_journaled]。
    `--force` は進捗に関わらず排他ロックを奪取する（`backlog lock acquire --force ...`） [test: crates/backlog/tests/lock_heartbeat_ttl.rs::ttl_exceeded_lock_with_unconfirmable_progress_is_not_reaped_but_force_wins]。
- `backlog lock status` が `undetermined`（registry を読めなかった）を返したら「driver 不在」とは読まない [test: crates/backlog/tests/concurrent_drivers.rs::an_unreadable_registry_does_not_report_none] [test: crates/integration-tests/tests/flow_skill_queue_contract.rs::undetermined_liveness_is_not_read_as_free]。安全側に倒し、理由を報告して見送ること。
- stale な driver 登録は次の `register` が自動で reap する（TTL 1800 秒・heartbeat 基準） [test: crates/backlog/tests/concurrent_drivers.rs::a_stale_driver_registration_is_reaped_like_a_stale_lock]。TTL 超過の排他ロックは進捗の停止が確認できたときだけ reap され、確認できなければ `--force` だけが奪取する [test: crates/backlog/tests/lock_heartbeat_ttl.rs::ttl_exceeded_lock_with_unconfirmable_progress_is_not_reaped_but_force_wins] [test: crates/backlog/tests/lock_heartbeat_ttl.rs::frozen_signals_across_the_window_are_confirmed_stalled_and_reaped]。

### Step 3 — 実行ループを回すこと

「source が尽きる / 予算超過 / ユーザー中断」まで以下を繰り返すこと。

> cwd は常にこのセッションの **linked worktree** にすること。
> `backlog`（add / done / fail / edit / cancel / `sync --apply`）と `compass`（charter --write / gap --write / outcome / opportunity add / evaluate）の **store 書き込みは、main（primary checkout）の作業ツリー内では拒否される**（非 0 終了・何も書かない） [test: crates/backlog/tests/store_worktree_boundary.rs::add_from_primary_checkout_is_refused_and_writes_nothing] [test: crates/backlog/tests/store_worktree_boundary.rs::done_from_primary_checkout_is_refused_and_file_unchanged] [test: crates/backlog/tests/store_worktree_boundary_env.rs::fail_from_primary_checkout_is_refused_and_file_unchanged] [test: crates/backlog/tests/store_worktree_boundary.rs::edit_from_primary_checkout_is_refused_and_file_unchanged] [test: crates/backlog/tests/store_worktree_boundary_env.rs::cancel_from_primary_checkout_is_refused_and_file_unchanged] [test: crates/backlog/tests/store_worktree_boundary.rs::sync_apply_from_primary_checkout_is_refused_and_creates_no_issue] [test: crates/compass/tests/store_worktree_boundary.rs::charter_write_from_primary_checkout_is_refused_and_writes_nothing] [test: crates/compass/tests/store_worktree_boundary.rs::gap_write_from_primary_checkout_is_refused_and_charter_unchanged] [test: crates/compass/tests/store_worktree_boundary.rs::outcome_from_primary_checkout_is_refused_and_writes_nothing] [test: crates/compass/tests/store_worktree_boundary.rs::opportunity_add_from_primary_checkout_is_refused_and_writes_nothing] [test: crates/compass/tests/store_worktree_boundary.rs::evaluate_from_primary_checkout_is_refused_and_writes_nothing]。
> 判定不能（git が答えない）も拒否に倒れる [test: crates/backlog/tests/store_worktree_boundary_env.rs::add_where_dot_git_points_at_a_missing_gitdir_is_refused]。
> 以下の `backlog` / `compass` はすべて、セッションの worktree を cwd にして実行せよ（`git worktree add …` で作った linked worktree） [test: crates/backlog/tests/store_worktree_boundary.rs::add_from_linked_worktree_succeeds_and_lands_in_that_worktree] [test: crates/compass/tests/store_worktree_boundary.rs::charter_write_from_linked_worktree_succeeds_there_only]。
> 読み取り（list / pivot-check / `compass charter`（--write なし））は main からでも動く [test: crates/backlog/tests/store_worktree_boundary.rs::list_from_primary_checkout_still_works] [test: crates/compass/tests/store_worktree_boundary.rs::pivot_check_from_primary_checkout_still_works_and_creates_nothing] [test: crates/compass/tests/store_worktree_boundary.rs::charter_show_from_primary_checkout_still_works]。
> `next`（--claim なし）と `compass gap`（--write なし）も main からでも動く [test: scripts/test_prose_flow_b.py::TestMainReads::test_claim_main_reads_backlog_next] [test: scripts/test_prose_flow_b.py::TestMainReads::test_claim_main_reads_compass_gap]。

#### 3-1. 次のタスクを優先度順にピックすること

予算を判定できないまま、または予算超過のままピックしてはならない [test: scripts/test_prose_flow_b.py::TestBudgetCheck::test_claim_budget_over_is_nonzero] [test: scripts/test_prose_flow_b.py::TestBudgetCheck::test_claim_budget_corrupt_ledger_is_nonzero] [test: scripts/test_prose_flow_b.py::TestBudgetCheck::test_claim_budget_corrupt_config_is_nonzero] [test: scripts/test_prose_flow_b.py::TestBudgetCheck::test_claim_budget_missing_binary_is_nonzero] [test: scripts/test_prose_flow_b.py::TestBudgetCheck::test_claim_budget_prose_nonzero_goes_to_step_4]。各サイクルのピックの前に次を実行すること [test: scripts/test_prose_flow_b.py::TestBudgetCheck::test_claim_budget_3_1_block_contains_check_command] [test: scripts/test_prose_flow_b.py::TestBudgetCheck::test_claim_budget_frontmatter_allows_budgetguard]。
```bash
budgetguard check   # stdout は JSON 1 行 [test: crates/budgetguard/tests/check_contract.rs::check_within_budget_exits_0_with_verdict_within] [test: crates/budgetguard/tests/check_contract.rs::check_corrupt_ledger_is_undetermined_exit_2]。exit 0 = 予算内 / exit 1 = 予算超過 / exit 2 = 判定不能 [test: crates/budgetguard/tests/check_contract.rs::check_within_budget_exits_0_with_verdict_within] [test: crates/budgetguard/tests/check_contract.rs::check_at_or_over_block_threshold_exits_1_with_verdict_over] [test: crates/budgetguard/tests/check_contract.rs::check_corrupt_ledger_is_undetermined_exit_2] [test: crates/budgetguard/tests/check_contract.rs::check_corrupt_config_is_undetermined_exit_2]
```
exit 0 以外（exit 1・exit 2・`budgetguard` 不在・起動失敗）なら予算超過とみなし、ピックせず Step 4 へ進むこと（早期脱出条件） [test: scripts/test_prose_flow_b.py::TestBudgetCheck::test_claim_budget_over_is_nonzero] [test: scripts/test_prose_flow_b.py::TestBudgetCheck::test_claim_budget_corrupt_ledger_is_nonzero] [test: scripts/test_prose_flow_b.py::TestBudgetCheck::test_claim_budget_corrupt_config_is_nonzero] [test: scripts/test_prose_flow_b.py::TestBudgetCheck::test_claim_budget_missing_binary_is_nonzero] [test: scripts/test_prose_flow_b.py::TestBudgetCheck::test_claim_budget_prose_nonzero_goes_to_step_4]。

compass / charter を先頭で引いてはならない。1 と 2 がどちらも一手を出さなかったときのフォールバックとしてだけ引くこと。
出荷しただけで仮説を validate してはならない（build ≠ validate） [test: crates/hypothesis/src/store.rs::test_validate_requires_evidence]。

1. measure step として、新規 build より先に、出荷済み・未計測の仮説を回収せよ。
   ```bash
   hypothesis list --status awaiting-measurement   # 出荷済み・未計測（awaiting-measurement）の仮説を列挙する [test: crates/hypothesis/tests/integration.rs::list_json_uses_hyphenated_status_vocabulary] [test: crates/hypothesis/src/store.rs::test_list_status_filter]
   ```
   - 各 awaiting-measurement 仮説について、計測信号が今観測可能かを判定せよ。
     - 観測可能な場合、condukt を起動しない。実験で観測した成果を集め、3-3 の sink で `hypothesis validate/reject --evidence` して仮説を閉じよ（3-2 は飛ばす） [test: crates/hypothesis/src/store.rs::test_awaiting_measurement_can_still_validate]。
     - まだ観測不能（データ蓄積待ち等）な場合、ここで無限ループしない。awaiting-measurement のまま残し、「計測待ち（まだ観測不能）」として報告して次の候補へ進むこと。
   - awaiting-measurement の仮説が 0 件なら次の候補へ進むこと。
2. measure 対象（今観測可能なもの）が無ければ backlog（確定キュー）を引くこと。
   backlog に複数の ready 課題がある場合は、順列（1件ずつ）ではなく 1 回の condukt run に束ねて並列処理せよ。
   並列・直列の判定を flow 側でしてはならない。独立候補を束ねて渡すだけにすること。
   ファイル競合・shared-glob・依存層は condukt が判定し、非衝突タスクだけを並列バッチに、衝突したものは直列に落とす [test: crates/condukt/src/schedule.rs::shared_file_demotes_both_off_the_parallel_track] [test: crates/condukt/src/schedule.rs::shared_glob_demotes_to_serial] [test: crates/condukt/src/schedule.rs::deps_create_ordered_layers]。
   Gated クラスのタスクは単独で隔離され、バッチに入らない [test: crates/condukt/tests/autonomy_invariant.rs::gated_task_is_isolated_and_never_batched]。

   （a）`backlog list` で覗いて上位 N 件を自分のものと決めてはいけない [test: crates/integration-tests/tests/flow_skill_queue_contract.rs::the_pick_reserves_the_task_instead_of_only_reading_it]。バッチは 1 件ずつ `--claim` で予約しながら最大 N 件取り出し、cwd はこのセッションの linked worktree にすること [test: crates/backlog/tests/store_worktree_boundary.rs::next_claim_from_linked_worktree_succeeds_and_records_a_lease] [test: crates/backlog/tests/concurrent_drivers.rs::two_concurrent_driver_processes_get_disjoint_tasks]。
      `--claim` の lease は repo 外の `~/.backlog/claims` に書かれ、main からでも拒否されない [test: crates/backlog/tests/store_worktree_boundary.rs::next_claim_from_primary_checkout_is_allowed_and_never_dirties_main]。
      ```bash
      backlog next --claim --project "$PWD"   # 選択と予約を同一クリティカルセクションで行う [test: crates/backlog/tests/concurrent_drivers.rs::two_concurrent_driver_processes_get_disjoint_tasks]。N 回繰り返すこと。
      ```
      `next --claim` は選んだ task を untracked な claim ledger に lease として記録するため、どの checkout からでも 2 つの driver が同じ task を受け取ることは無い [test: crates/backlog/tests/cross_checkout_claim.rs::two_checkouts_of_the_same_project_never_claim_the_same_task]。
      トラックされた `.backlog/tasks.toml` は書かれない [test: crates/backlog/tests/claim_lease_untracked.rs::claim_does_not_dirty_the_tracked_store_and_is_visible_across_worktrees]。
      `list` は lease 中の task を `claimed` と表示する [test: crates/backlog/tests/claim_lease_untracked.rs::claim_does_not_dirty_the_tracked_store_and_is_visible_across_worktrees]。素の `next` はそれを返さない [test: crates/backlog/tests/claim_lease_untracked.rs::plain_next_skips_a_leased_task_in_every_checkout]。
      - 出力が `no pending tasks` になるまで、または N 件（既定 N=condukt の `max_parallel` で、無指定なら 4）に達するまで繰り返すこと [test: scripts/test_prose_flow_a.py::T::test_claim_max_parallel_default_is_four_when_unset]。
      - 各件の `id` / `title` / `notes` / `hashkey` を控えること（`hashkey` は `next --claim` の出力に含まれる） [test: crates/backlog/tests/concurrent_drivers.rs::two_concurrent_driver_processes_get_disjoint_tasks]。N=1 の場合は従来どおり単一課題として扱うこと。
      - `no pending tasks` が返っても即座に「キューが空」と断定してはならない [test: crates/backlog/tests/cross_checkout_claim.rs::an_unreadable_ledger_refuses_the_claim_instead_of_answering_empty]。1 度は取り直すこと。`next --claim` は台帳を読めなかったとき非 0 で終了し、stdout には何も出さない [test: crates/backlog/tests/cross_checkout_claim.rs::an_unreadable_ledger_refuses_the_claim_instead_of_answering_empty]。
      「判定不能」を「claim なし」として続行してはならない。予約した各 item について `condukt state is-claimed --hashkey <hashkey>` を実行せよ（claim-skip ゲート） [test: crates/condukt/tests/is_claimed_exit_contract.rs::claimed_hashkey_exits_0_with_claimed_true]。
      判定は 3 値: exit 0 は claim 済みなのでその item をスキップする [test: crates/condukt/tests/is_claimed_exit_contract.rs::claimed_hashkey_exits_0_with_claimed_true]。exit 1 かつ stdout が `"claimed": false` なら claim なしなのでピックを続行する [test: crates/condukt/tests/is_claimed_exit_contract.rs::clean_empty_registry_exits_1_with_claimed_false]。それ以外（`condukt` が無い・exit 3＝claim registry を読めない・起動失敗・タイムアウト）は判定不能なので、claim 済みとみなしてその item をスキップし、理由（exit code / stderr）を報告すること [test: crates/condukt/tests/is_claimed_exit_contract.rs::corrupt_registry_exits_3_not_1]。
      lease は保持者の heartbeat が続く間は 1 時間（`CLAIM_STALE_SECS`）を過ぎても除外し続け [test: crates/backlog/tests/verifier_486e83ca.rs::verifier_live_holder_keeps_lease_past_window]、heartbeat が止まって `CLAIM_STALE_SECS` を過ぎると除外をやめる [test: crates/backlog/tests/verifier_486e83ca.rs::verifier_dead_holder_is_reclaimable] [test: crates/backlog/tests/claim_lease_untracked.rs::stale_lease_releases_the_task]。
   （b）予算逼迫、相互依存、同一領域・同一ファイルへの接触、deploy・push（Gated 相当）のいずれかが読み取れる候補は、バッチから外して 1 件ずつ直列に回すこと。
      - 3-1 冒頭の `budgetguard check` が exit 0 でも JSON の `pressure` が `true`（当日支出が `daily.warn_usd` 以上）なら予算逼迫なので、バッチ幅を N=1 に縮退して従来の直列にすること [test: crates/budgetguard/tests/check_contract.rs::check_between_warn_and_block_is_within_with_pressure_true] [test: scripts/test_prose_flow_b.py::TestBudgetCheck::test_claim_budget_pressure_line_mentions_pressure_and_n1]。
      判断に迷うものはバッチに入れてよい（condukt が衝突を検出して直列化する） [test: crates/condukt/src/schedule.rs::shared_file_demotes_both_off_the_parallel_track]。
   （c）このループが `backlog add` する場合、cwd はこのセッションの linked worktree にすること（main では拒否される [test: crates/backlog/tests/store_worktree_boundary.rs::add_from_primary_checkout_is_refused_and_writes_nothing]）。compass opportunity 由来ならその weight を供給すること。
      ```bash
      W=$(compass gap | jq -r '.opportunities[0].weight // empty')
      backlog add --title "<課題>" --project "$PWD" --priority p1 --weight "${W:-0}" \
        --repro-test "<その課題を再現するコミット済みテスト>"   # 再現しなければ拒否され store に何も書かれない [test: crates/backlog/tests/no_confirm_no_unproven.rs::add_with_non_reproducing_repro_writes_nothing_anywhere]
      ```
      weight は同じ priority 内の順序だけを変え、priority は上書きしない [test: crates/backlog/src/store.rs::priority_dominates_weight] [test: crates/backlog/src/store.rs::next_orders_by_weight_desc_within_priority]。weight が同じなら従来の created_at 順になる [test: crates/backlog/src/store.rs::equal_weight_falls_back_to_created_at]。
      weight 無指定は既定 0.0 である [test: scripts/test_prose_flow_b.py::TestWeightDefault::test_claim_weight_default]。
      クロスプロジェクトで繰り返し検出される作業種別は `docs/backlog-tag-taxonomy.md` の規約タグ（例:
      `worktree-hygiene` / `deploy-verify` / `network-infra`）も併せて付ける [test: scripts/test_prose_flow_b.py::TestTagTaxonomy::test_claim_tag_taxonomy]。
3. backlog が（1 度取り直したうえで）一手も出さず、measure 対象も無いときだけ compass / charter を引くこと。
   ```bash
   compass gap     # dod / measuring_stick / last_outcome / opportunities などを JSON で出す [test: scripts/test_prose_flow_b.py::TestGapRoute::test_claim_gap_route_gap_keys]
   ```
   - **charter が鮮明で右サイズの一手が引ける**なら、`compass gap` の `opportunities` と `dod` からその一手を選び、6 へ進むこと [test: scripts/test_prose_flow_b.py::TestGapRoute::test_claim_gap_route_gap_keys]。
     以降は backlog 由来と同じ扱いとし、sink には 3-3 の compass 由来（`compass outcome`）を使うこと [test: scripts/test_prose_flow_b.py::TestCompassOutcome::test_claim_compass_outcome]。
   - charter が陳腐・矛盾・抽象すぎて一手が引けない場合、`/compass` を案内して停止してはならない [test: scripts/test_prose_flow_b.py::TestCarveSentinel::test_claim_carve_sentinel] [test: scripts/test_prose_flow_b.py::TestMainReads::test_claim_main_reads_compass_gap]。
     その場で charter を彫り直すこと。
     `compass` skill の Step 1〜5（`compass evaluate` → C3–C5 の自己判定 → 未解決の問いを
     `AskUserQuestion` で 1 問ずつ詰める → `compass charter --write` → `compass gap --write "<gap>"`）を回し、
     彫り直した charter から一手を引いて 6 へ進むこと [test: scripts/test_prose_flow_b.py::TestCarveSentinel::test_claim_carve_sentinel] [test: scripts/test_prose_flow_b.py::TestCarveSentinel::test_claim_carve_sentinel_bare_gap_write_is_usage_error]。
     - この carve 中の `AskUserQuestion` を省いてはならない [test: scripts/test_prose_flow_a.py::T::test_claim_frontmatter_declares_args_and_tools]。charter を彫る問いは「どちらを選ぶべきか」を
       人間に尋ねる判断要求であって権限認可ではないので、`--approval` を付けてはならない [test: scripts/test_prose_flow_a.py::T::test_claim_judgment_approval_flag_is_exactly_what_erases_the_question] [test: scripts/test_prose_flow_a.py::T::test_claim_judgment_no_approval_on_judgment_gates_in_skill_text]。
     - 彫れないまま実装へ進んではならない。人間が離席して `sentinel` に落ちた場合は、分かっている範囲で charter を保存し、
       残りを保留に流してループを終えること（Step 4 へ） [test: scripts/test_prose_flow_b.py::TestCarveSentinel::test_claim_carve_sentinel] [test: scripts/test_prose_flow_b.py::TestCarveSentinel::test_claim_carve_sentinel_no_defer_stays_open]。
   - `compass` バイナリが無い / 呼び出しが失敗した場合、ここで condukt へ流してはならない [test: scripts/test_prose_flow_b.py::TestCompassFailDetectable::test_claim_compass_fail_detectable_no_charter] [test: scripts/test_prose_flow_b.py::TestCompassFailDetectable::test_claim_compass_fail_detectable_unreadable_charter]。
     判定不能を「仕事なし」にも「実行してよい」にも丸めてはならない。charter の状態を観測できなかった事実を報告して Step 4 へ抜けること。

4. backlog が空になったことを理由に自動で open 仮説（新規 discovery）へ降りてはいけない。
   自動ループでは open 仮説は残課題として報告するだけにして 5 へ進むこと。
   open 仮説は、ユーザーが明示的にそれを回せと言ったときだけ引くこと。明示指示がある場合のみ以下を使うこと。
   ```bash
   hypothesis list --status open    # confidence 降順（同点 created_at 昇順）でソート済み [test: crates/hypothesis/src/store.rs::list_sorts_by_confidence_descending] [test: crates/hypothesis/src/store.rs::list_confidence_tiebreak_by_created_at]。空なら次へ進むこと。
   ```
   `list --status open` の先頭（最高 confidence）から順にピックすること [test: crates/hypothesis/src/store.rs::list_sorts_by_confidence_descending]。
   open な仮説があれば、full build に直行する前に RAT ゲート（riskiest-assumption test）を通すこと。
   ```bash
   RAT=$(hypothesis rat <hid>)      # 未テストの最重要×弱証拠 assumption（leap of faith）を返す [test: crates/hypothesis/src/store.rs::test_assumptions_persist_and_rat_selection]
   ```
   - `RAT` が非空（高リスク・未テストの leap of faith がある）なら、課題文は full build ではなく、
     その assumption だけを検証する最小 de-risk 実験にすること（"<assumption text> が成り立つかを最小コストで測る実験"） [test: scripts/test_prose_flow_b.py::TestRat::test_claim_rat]。
     `RAT` 行頭の index を控え、3-3 の sink で `hypothesis tested <hid> <index>` を呼んで計測ループを閉じること [test: scripts/test_prose_flow_b.py::TestRat::test_claim_rat] [test: scripts/test_prose_flow_b.py::TestRat::test_claim_rat_tested_out_of_range_fails]。
   - `RAT` が空（高リスクの未テスト assumption が無い＝既に de-risk 済み）なら、従来どおり
     その仮説を検証する実験（full build）を課題文にすること [test: crates/hypothesis/src/hypothesis.rs::riskiest_assumption_none_when_all_derisked]。
   いずれも仮説 ID を控えること。
5. 停止判定では、仕事が無いのに繰り返してはならない。下記の 3 つのどれかが一手を出す限り止めてはならない（pending が残っているのに早期に止めてはならない）。
   open 仮説だけを根拠にループを継続してはいけない。
   継続してよいのは、(i) backlog の pending（3-1 の 2）、(ii) measure step（3-1 の 1 で今観測可能な awaiting-measurement 仮説）、(iii) compass 主筋（3-1 の 3 のフォールバックで実際に引けた compass の一手）のどれかが実際に一手を出したときだけにすること。
   この 3 つがどれも空ならループを抜けて Step 4 へ進むこと。
   残っている open 仮説と、まだ観測不能な awaiting-measurement は、残課題として報告するだけにしてループを終えること。
6. ピックしたタスクを課題文に組み立てること。
   - flow 側で並列上限の待ち合わせ制御をしてはならない。
   - 単一課題（compass 主筋 / measure / hypothesis / 単発 backlog）は、タイトル＋ notes（仕様・制約・参照ファイル）で 1 課題文にすること。
   - backlog バッチ（3 の a・b で複数残った場合）は、1 つの課題文に、各 backlog item を「独立した top-level タスク」として列挙すること。
     各項目に id・タイトル・notes・（分かるなら）触るファイルや領域を明記し、「これらは互いに独立であり、非衝突なものは並列に、
     衝突・共有リソースを触るものは直列に scheduleしてよい」と condukt に明示すること。
     item 単位で done_criteria を切ってもらい、item id ↔ condukt タスクの対応を控えること。
7. 課題文を組み立てた直後、選んだ source 種別によらず必ず overwatch に anchor を登録せよ。
   ```bash
   overwatch begin --key "<pdo-unit-id>" --title "<task title>" \
     --scope "<touched_files をカンマ区切り、不明なら省略>" \
     --done-criteria "<done_criteria>"
   ```
   - `<pdo-unit-id>` は backlog なら `hashkey`、compass 主筋 / measure / hypothesis なら move / 仮説 ID など
     その PDO 単位を一意に指す文字列にすること [test: scripts/test_prose_flow_b.py::TestOverwatchBegin::test_claim_overwatch_begin]。この key は Step 4 の `overwatch end` で解放するため控えておくこと [test: crates/overwatch/tests/backlog_s04_audit.rs::s3b056c02_end_of_missing_key_is_distinguishable_from_release]。
   - `--scope` は分かるなら condukt の `touched_files` と同じ語彙（カンマ区切り）で渡すこと [test: scripts/test_prose_flow_b.py::TestOverwatchBegin::test_claim_overwatch_begin] [test: scripts/test_prose_flow_b.py::TestOverwatchBegin::test_claim_overwatch_begin_overlap_is_reported_for_shared_scope]。調査・carve ループ中など
     scope が未確定なら省略すること。
     空 scope は「重なり無し」ではなく `scope_undeclared` として報告される [test: scripts/test_prose_flow_b.py::TestOverwatchBegin::test_claim_overwatch_begin]。
   - バッチ（複数 backlog item）なら item ごとに `overwatch begin` を呼ぶこと（key = 各 item の `hashkey`） [test: scripts/test_prose_flow_b.py::TestOverwatchBegin::test_claim_overwatch_begin]。

8. 選択を shared discovery store に記録せよ（バッチなら選んだ各 item を記録すること）。`select` は既に記録された行だけを selected にするので、先に `record` すること [test: crates/compass/tests/discovery.rs::dod3_select_flips_only_matching_row_to_selected]。
   ```bash
   compass discovery record --session-id "<SESSION_ID>" --title "<選んだタスクのタイトル>"
   compass discovery select --session-id "<SESSION_ID>" --title "<選んだタスクのタイトル>"
   ```

9. 着手前に、選んだタスク（バッチなら各 item）を claim せよ（TOCTOU の最終ガード） [test: crates/condukt/tests/claim_registry_e2e.rs::running_a_task_claims_and_a_conflicting_run_is_hard_skipped_then_released]。
   ```bash
   condukt state claim-task --run "flow-$CLAUDE_CODE_SESSION_ID" --session "$CLAUDE_CODE_SESSION_ID" \
     --stateless --title "<title>" --hashkey <hashkey>
   ```
   `--stateless` を必ず付けること [test: crates/integration-tests/tests/flow_skill_queue_contract.rs::every_claim_task_invocation_is_marked_stateless]。
   `--stateless` を付けると condukt は owning session の transcript だけで生死を判定し、凍結していれば回収する [test: crates/condukt/src/claim.rs::stateless_marked_frozen_transcript_stale_heartbeat_is_reaped]。
   マーク無しの claim は run state 不在を「判定不能」として永久に保持する [test: crates/condukt/src/claim.rs::stateless_unmarked_missing_run_state_is_kept]。
   exit 1（別セッションが直前に claim 済み＝スキップされた）ならその item は諦めて 3-1 に戻り次候補へ進むこと。

#### 3-1.5. 正典が無い課題は、condukt の実装フェーズへ進めず spec 起こしへ戻すこと

正典（spec）が無いと分かった課題を、そのまま condukt の実装フェーズへ進めてはならない。specforge へ戻すこと。


次の3つのどれか1つが立ったら、divert するか否かを下の合意ゲートに通すこと。

- trigger 1: condukt-researcher の research brief が `open_questions` を出したとき [test: scripts/test_prose_flow_a.py::T::test_claim_decomp_fields_open_questions_is_the_researcher_field_not_the_interpreters]。3-2 の起動条件に従うこと。
- trigger 2: task が `confidence: low` を持つとき [test: scripts/test_prose_flow_a.py::T::test_claim_decomp_fields_engine_accepts_open_questions_and_confidence_low]。condukt Phase 1 の Decomposition で取得し、3-2 の起動条件に従うこと。
- trigger 3: specguard brief が `verdict=not-covered` を返したとき [test: crates/specguard/tests/integration.rs::brief_json_verdict_field_reaches_all_three_states]。condukt 起動前の flow のプリフライトで取得すること。

##### trigger 3 は condukt を起こす前にプリフライトで測ること

```bash
BRIEF=$(specguard brief --json "<課題文の要約>" 2>/dev/null); rc=$?
```

`specguard brief --json` は三値（`covered` / `not-covered` / `undetermined`）を返し、exit code でも `undetermined`（10）と判定できた場合（0）を区別する [test: crates/specguard/tests/integration.rs::brief_json_verdict_field_reaches_all_three_states]。

- 判定不能を `not-covered` へ丸めてはならない [test: crates/specguard/tests/integration.rs::brief_json_unobservable_store_states_are_undetermined_never_not_covered]。
- divert は書き込み側の動作なので、判定不能で divert を起こしてはならない。
- rc=0 かつ `verdict` が `covered` なら、divert せず 3-2 へ進むこと [test: crates/specguard/tests/integration.rs::brief_json_verdict_field_reaches_all_three_states]。
- rc=0 かつ `verdict` が `not-covered` なら、divert 候補として下の合意ゲートへ進むこと [test: crates/specguard/tests/integration.rs::brief_json_verdict_field_reaches_all_three_states]。
- それ以外すべて（rc=10 / specguard 不在 / 出力が読めない / verdict フィールドが無い）は `undetermined` として扱い、divert しないこと [test: crates/specguard/tests/integration.rs::brief_json_unobservable_store_states_are_undetermined_never_not_covered] [test: crates/specguard/tests/integration.rs::brief_json_verdict_field_reaches_all_three_states]。
- 代わりに「正典の有無を観測できなかった」ことを残課題として人間に surface し、その課題自体は通常どおり 3-2 へ進めること。

##### 合意ゲートに素の `AskUserQuestion` を使ってはならない [test: scripts/test_prose_flow_a.py::T::test_claim_judgment_divert_command_as_written_escalates_and_is_journaled]。`condukt policy answer` を通すこと

判断要求なので `--approval` を付けてはならない [test: scripts/test_prose_flow_a.py::T::test_claim_judgment_approval_flag_is_exactly_what_erases_the_question] [test: scripts/test_prose_flow_a.py::T::test_claim_judgment_no_approval_on_judgment_gates_in_skill_text] [test: scripts/test_prose_flow_a.py::T::test_claim_judgment_divert_command_as_written_escalates_and_is_journaled]。
trigger 1・2・3 のいずれかが立ったら、divert するか否かを Step 0.5 の policy-answer routing に通すこと。

```bash
condukt policy answer \
  --risk medium --reversible high --confidence low \
  --question "この課題は正典が無い (<発火した trigger>)。specforge へ戻すか、このまま condukt で実装するか?" \
  --option "specforge へ divert して spec を起こす" --option "このまま condukt で実装する" --recommend 0
```

risk・reversible・confidence の3値は固定値であり、ここで変えてはならない。
既定 verdict は **escalate**（`medium − high − low` は auto の閾値に届かない） [test: scripts/test_prose_flow_a.py::T::test_claim_judgment_divert_command_as_written_escalates_and_is_journaled]。exit code の扱いは
Step 0.5 の表に従うこと。この経路を通さず
素の `AskUserQuestion` を直接出すことは禁止 [test: scripts/test_prose_flow_a.py::T::test_claim_judgment_divert_command_as_written_escalates_and_is_journaled]。

##### divert したら次の手順を踏むこと

- divert 回数を数えて止めるカウンタを足してはならない。
- 元の課題を `backlog fail` にしてはいけない [test: scripts/test_prose_flow_b.py::TestFailVsPending::test_claim_fail_vs_pending_fail_withdraws_task]。
- `specforge draft` に課題文を位置引数で渡してはならない（`error: unexpected argument`） [test: scripts/test_prose_flow_b.py::TestSpecforge::test_claim_specforge_positional_arg_rejected]。
- `backlog list --status pending` はピックに使ってはいけない [test: scripts/test_prose_flow_b.py::TestListPure::test_claim_list_pure]。ピックは常に `next --claim` で行うこと [test: scripts/test_prose_flow_b.py::TestListPure::test_claim_list_pure]。
- 差し戻し先が未設定の repo でこの経路を踏んだら、divert を強行せず人間へ返すこと。
- 課題文は `--req <file>` か stdin で渡し、`--id` を必ず付けること [test: scripts/test_prose_flow_b.py::TestSpecforge::test_claim_specforge_draft_requires_id] [test: scripts/test_prose_flow_b.py::TestSpecforge::test_claim_specforge_draft_via_req_file_and_stdin]。`ratify` には `--reason` を必ず付けること [test: scripts/test_prose_flow_b.py::TestSpecforge::test_claim_specforge_ratify_requires_reason]。
- 元の課題は `fail` せず pending のまま残すこと [test: scripts/test_prose_flow_b.py::TestFailVsPending::test_claim_fail_vs_pending_fail_withdraws_task]。`backlog edit <id> --status pending` は lease を解放せず、`fail` 済みの task の defer も解かない [test: scripts/test_prose_flow_b.py::TestFailVsPending::test_claim_fail_vs_pending_edit_pending_keeps_task_deferred]。このセッションが `backlog driver heartbeat` を続ける間、divert した課題の lease は失効せず、他の driver には配られない [test: crates/backlog/tests/verifier_486e83ca.rs::verifier_live_holder_keeps_lease_past_window]。heartbeat が止まって `CLAIM_STALE_SECS`（1 時間）を過ぎると再び配られる [test: crates/backlog/tests/verifier_486e83ca.rs::verifier_dead_holder_is_reclaimable]。
- `ratify` が通らなければ（contract floor 違反で非0終了）、specforge が出す理由を添えて人間へ返すこと [test: scripts/test_prose_flow_b.py::TestSpecforge::test_claim_specforge_ratify_refuses_contract_violation_and_queues_nothing]。
- `ratify` は spec を批准したあと、要件ごとに `backlog add` を呼ぶ [test: scripts/test_prose_flow_b.py::TestSpecforge::test_claim_specforge_ratify_queues_one_item_per_requirement]。
- 本物の `backlog` はこの `add` を `--repro-test` 無しとして拒否するので、`ratify` は exit 9 で終了し、backlog には何も積まれない [test: scripts/test_prose_flow_b.py::TestSpecforge::test_claim_specforge_ratify_with_real_backlog_exits_9_and_files_nothing]。
- `ratify` が exit 9 なら、spec 由来の要件が queue されなかったことを人間へ返すこと [test: scripts/test_prose_flow_b.py::TestSpecforge::test_claim_specforge_ratify_with_real_backlog_exits_9_and_files_nothing]。
- `ratify` は contract floor 違反（`Spec::contract_violations()` が非空）の spec の批准を拒否し、非0で終了する [test: scripts/test_prose_flow_b.py::TestSpecforge::test_claim_specforge_ratify_refuses_contract_violation_and_queues_nothing]。

[record: 2026-09-07 rev: a81444b7] この起動形は実測で通した（specguard 0.2.54、使い捨て git repo で draft→ratify→backlog を通しで実行し、`ratify` が `backlog: 3 件を起票 / 0 件は起票済み (計 3 requirement)` を出力し、直後の確認用の `backlog list --status pending`（ピックに使ってはいけない純粋な read）に `[r4demo:R1]`〜`[r4demo:R3]` の3件が `p1 / pending` で並んだ）。

```bash
printf '%s\n' "<課題文>" > "$req"
specforge draft --id <spec id> --title "<課題タイトル>" --req "$req"

specforge ratify --id <spec id> --reason "<なぜこの spec を受け入れるか>"
```

`specforge` は `specforge.toml` が無いと exit 2 で拒否する [test: scripts/test_prose_flow_b.py::TestSpecforge::test_claim_specforge_without_config_exits_2]。

#### 3-2. 課題文を condukt で実行すること

- 分解を止めてはならない。止めるのは実装フェーズだけにすること。
- 1 セッション内で複数の condukt run を並走させてはならない。
- 課題文を `/condukt` に渡すこと [test: scripts/test_prose_flow_a.py::T::test_claim_frontmatter_declares_args_and_tools]。condukt が分解 JSON を出したら、`fugu-router route` が各タスクの `suggested_model` を実績から（Thompson sampling による確率的な選択で）上書きする（併用時） [test: scripts/test_prose_flow_a.py::T::test_claim_condukt_fugu_route_rewrites_suggested_model_from_episodes]:

```
/condukt <課題文>
```

- `/condukt` は `Task` ツールで非同期起動すること [test: scripts/test_prose_flow_a.py::T::test_claim_frontmatter_declares_args_and_tools]。
- spec-gap の trigger 1 は condukt-researcher の research brief から、trigger 2 は Phase 1 の Decomposition から取ること（3-1.5 の続き） [test: scripts/test_prose_flow_a.py::T::test_claim_decomp_fields_open_questions_is_the_researcher_field_not_the_interpreters] [test: scripts/test_prose_flow_a.py::T::test_claim_decomp_fields_engine_accepts_open_questions_and_confidence_low]。課題文に「**research brief に `open_questions` が出たか、Decomposition（Phase 1〜2）のいずれかのタスクの `confidence` が `low` なら、Phase 5（実装）へ進まずその事実を報告して停止せよ**」を明記して渡すこと [test: scripts/test_prose_flow_a.py::T::test_claim_decomp_fields_open_questions_is_the_researcher_field_not_the_interpreters] [test: scripts/test_prose_flow_a.py::T::test_claim_decomp_fields_engine_accepts_open_questions_and_confidence_low]。
- 停止して戻ってきたら 3-1.5 の合意ゲートへ入り、divert が選ばれたら specforge へ、選ばれなければ同じ Decomposition のまま `/condukt` を実装フェーズから再開させること [test: scripts/test_prose_flow_a.py::T::test_claim_frontmatter_declares_args_and_tools] [test: scripts/test_prose_flow_a.py::T::test_claim_judgment_divert_command_as_written_escalates_and_is_journaled]。
- compass 由来の一手なら、`compass gap` の `measuring_stick` と `dod` を文脈として課題文に添えること [test: scripts/test_prose_flow_b.py::TestGapRoute::test_claim_gap_route_gap_keys]。
- backlog バッチは 1 回の `/condukt` 呼び出しに束ねること [test: scripts/test_prose_flow_a.py::T::test_claim_frontmatter_declares_args_and_tools]。並列か直列かの実判定は condukt の `schedule.rs` が担う [test: crates/condukt/tests/schedule_conflict_capstone_e2e.rs::conflicting_pairs_are_forced_serial_and_independent_pair_stays_parallel]。

condukt 実行が完了したら、観測できた cost_usd・duration_secs を `fugu-router record` に `--delegation` を添えて記録し、直後に `audit-recent` でその記録が実際にストアへ着地したかを自己検証すること [test: scripts/test_prose_flow_a.py::T::test_claim_fugu_audit_record_then_audit_found]。
```bash
fugu-router record --title "<task title>" --class "flow-delegation" \
  --model <suggested_model> --status <verified|failed> \
  --cost <observed_cost_usd> --duration <observed_duration_secs> \
  --delegation <fork|inline>

fugu-router audit-recent --class "flow-delegation" --within 60
```
- `audit-recent` が exit 0（found:true）なら記録が確認できたので、通常どおり次のサイクルへ進むこと [test: scripts/test_prose_flow_a.py::T::test_claim_fugu_audit_record_then_audit_found]。
- `audit-recent` が exit 1（found:false）なら記録漏れなので、`fugu-router record` をもう一度試みること [test: scripts/test_prose_flow_a.py::T::test_claim_fugu_audit_record_then_audit_found] [test: scripts/test_prose_flow_a.py::T::test_claim_fugu_audit_other_class_does_not_satisfy_the_audit]。再試行でも記録できなければ、ユーザーに「delegation 記録に失敗した」と明示的に警告すること。

#### 3-3. 検証が通ったら結果を source に書き戻すこと

- バッチ全体を一括で成功扱いにも失敗扱いにもしてはならない。
- 新規 experiment の出荷直後に、この場で `hypothesis validate` / `hypothesis reject` してはならない [test: crates/hypothesis/src/store.rs::test_validate_requires_evidence]。

condukt の完了ゲートを通ったら結果を source に書き戻すこと。`backlog done` / `compass outcome` は cwd = このセッションの linked worktree で実行すること [test: crates/backlog/tests/store_worktree_boundary.rs::done_from_linked_worktree_succeeds_and_marks_done_there] [test: crates/compass/tests/store_worktree_boundary.rs::outcome_from_linked_worktree_succeeds_there_only]。`backlog fail` も同様に cwd = このセッションの linked worktree で実行すること [test: crates/backlog/tests/store_worktree_boundary_env.rs::fail_from_primary_checkout_is_refused_and_file_unchanged]。main の作業ツリーでは `backlog done` / `compass outcome` の store 書き込みが拒否される [test: crates/backlog/tests/store_worktree_boundary.rs::done_from_primary_checkout_is_refused_and_file_unchanged] [test: crates/compass/tests/store_worktree_boundary.rs::outcome_from_primary_checkout_is_refused_and_writes_nothing]。

バッチ（複数 backlog item を 1 run に束ねた場合）は item ごとに個別 sink すること。6 で控えた item id と condukt タスクの対応を使い、通ったタスクの item は `done`、blocked/失敗の item は `fail` と書き分けること（部分成功をそのまま反映し、一部が失敗しても通った分は done にする） [test: scripts/test_prose_flow_b.py::TestFailVsPending::test_claim_fail_vs_pending_done_closes_only_that_id] [test: scripts/test_prose_flow_b.py::TestFailVsPending::test_claim_fail_vs_pending_fail_withdraws_task]。

- 成功したら次のとおり sink すること。
  - backlog 由来なら、その item の再現テストを証拠にして `backlog done <id> --test "<再現テストのコマンド>" --red-rev <修正前の rev>` を実行すること（バッチなら通った各 item の id について実行） [test: crates/backlog/tests/closure_evidence.rs::allowlisted_bash_script_with_red_rev_closes_and_records_closure] [test: crates/backlog/tests/store_worktree_boundary.rs::done_from_linked_worktree_succeeds_and_marks_done_there]。
    素の `backlog done <id>` は拒否され、status は変わらない [test: crates/backlog/tests/closure_evidence.rs::bare_done_is_refused_and_status_unchanged]。
  - compass 由来なら、完了した move を measuring_stick で判定し、その verdict を記録すること。
    ```bash
    compass outcome --verdict <forward|unchanged|backward> --evidence "<観測した成果>"
    ```
    verdict は forward（前進）・unchanged（不変）・backward（後退）のいずれかとし、move の diff・テスト結果・gap への接近度から driver(LLM) が判定すること。
    `--evidence` には計測値（テスト数・ベンチ・観測した挙動）を必ず渡すこと [test: scripts/test_prose_flow_b.py::TestCompassOutcome::test_claim_compass_outcome]。
    記録後 `compass gap` を取り直すと `last_outcome` が次サイクルに反映される [test: scripts/test_prose_flow_b.py::TestCompassOutcome::test_claim_compass_outcome]。
  - hypothesis 由来（新規 experiment の build が完了）なら、linked_hypotheses を `awaiting-measurement`（出荷済み・未計測）へ遷移させるのは `condukt state gate` 自体ではなく、condukt skill が gate PASS 後に実行するブロックである [test: scripts/test_prose_flow_a.py::T::test_claim_linked_hypotheses_gate_pass_alone_does_not_transition_the_binary] [test: scripts/test_prose_flow_a.py::T::test_claim_linked_hypotheses_condukt_skill_block_moves_them_to_awaiting_measurement]。仮説は awaiting-measurement に残すこと。
    閉じるのは次サイクルの measure step（3-1 の 1）が観測値を添えて行うこと。「計測待ち N 件」を残課題として報告すること。
  - measure step 由来（3-1 の 1 で観測値を回収した awaiting-measurement 仮説）なら、観測した成果を添えて閉じること。反証なら `hypothesis reject <id> --reason "<反証内容>"` を使うこと [test: crates/hypothesis/src/store.rs::test_reject_with_run_id]:
    ```bash
    hypothesis validate <id> --run <RID> --evidence "<観測した成果>"
    ```
    これで awaiting-measurement から validated / rejected に遷移する [test: crates/hypothesis/src/store.rs::test_awaiting_measurement_can_still_validate]。
    `hypothesis validate` / `hypothesis reject` は証拠を必須とし、観測値の無い「出荷だけ」では status を変えられない [test: crates/hypothesis/src/store.rs::test_validate_requires_evidence]。
  - fugu-router 併用時は、検証結果（どのモデルが通ったか・コスト）を `fugu-router record` で書き戻すこと [test: crates/fugu-router/tests/backlog_c6ae7851.rs::backlog_c6ae7851_control_record_writes_the_store]。
- 失敗（blocked / needs-serial 等）したら次のとおり sink すること。
  - backlog 由来なら `backlog fail <id> --reason "<概要>"` を実行し、スキップして次へ進むこと（バッチなら失敗した item だけ fail、他は上記どおり done） [test: scripts/test_prose_flow_b.py::TestFailVsPending::test_claim_fail_vs_pending_fail_withdraws_task]。
  - ユーザーに失敗を通知し、ループは続行すること。

sink が成功でも失敗でも、`backlog done` / `backlog fail` の直後（または中断時）に、その item の claim を解放すること [test: scripts/test_prose_flow_a.py::T::test_claim_release_task_releases_only_that_hashkey]。
`backlog done` した item は `--outcome done` を付けて解放すること:
```bash
condukt state release-task --run "flow-$CLAUDE_CODE_SESSION_ID" --outcome done --hashkey <hashkey>
```
`backlog fail` した item・中断した item は `--outcome` を付けずに（または `--outcome failed` で）解放すること:
```bash
condukt state release-task --run "flow-$CLAUDE_CODE_SESSION_ID" --hashkey <hashkey>
```
release-task は解放の前に item の outcome を run の item-outcome log に記録する。flow run には run state が無いので、3-4 の `condukt circuit check` はこの log の末尾から連続する done 以外の件数を failure streak として数える [test: scripts/test_prose_flow_a.py::T::test_claim_circuit_flow_invocation_streak_and_budget_axes_are_dead]。`--outcome done` を付け忘れると成功した item も失敗として数えられる。
バッチなら item ごとに release すること（6/a で控えた `hashkey` を使う） [test: scripts/test_prose_flow_a.py::T::test_claim_release_task_releases_only_that_hashkey]。
別 run が保持している hashkey は解放されず、release-task は exit 1 で終わる [test: crates/condukt/tests/repro_release_task_ignores_run.rs::release_task_by_another_run_does_not_release_the_holders_claim]。

#### 3-4. ループを継続してよいか判定すること

3-1 に戻る前に、保持中の claim を live に保つため定期的に heartbeat すること [test: crates/condukt/tests/backlog_audit_s01.rs::control_task_claims_survive_a_heartbeat]:
```bash
condukt state heartbeat --run "flow-$CLAUDE_CODE_SESSION_ID"
```
長時間のループでは各サイクルで呼ぶこと。

Step 2 で登録した driver 自体も同じサイクルで heartbeat すること [test: crates/backlog/tests/verifier_486e83ca.rs::verifier_live_holder_keeps_lease_past_window]:
```bash
backlog driver heartbeat --session-id "$CLAUDE_CODE_SESSION_ID" --project "$PWD"
```
driver 登録の staleness は heartbeat_at ベースの TTL（既定30分）で判定され、TTL を超えた登録は stale として reap される [test: crates/backlog/tests/concurrent_drivers.rs::a_stale_driver_registration_is_reaped_like_a_stale_lock]。

続けて決定論の循環ブレーカーを1本のコマンドで判定すること [test: crates/condukt/tests/backlog_audit_v11.rs::backlog_9eeffa69_flow_skill_passes_session_to_circuit_check]:
```bash
condukt circuit check --run "flow-$CLAUDE_CODE_SESSION_ID" --session "$CLAUDE_CODE_SESSION_ID"   # trip なら nonzero [test: crates/condukt/src/circuit.rs::failure_streak_over_cap_trips_with_nonzero_exit]、continue なら exit 0 [test: crates/condukt/src/circuit.rs::healthy_run_continues_with_zero_exit]
```
nonzero（trip）なら人にも policy にも聞かず即 Step 4 へ進むこと。stop 理由 slug は JSONL に記録される [test: crates/condukt/src/circuit.rs::failure_streak_over_cap_trips_with_nonzero_exit]。exit 0 なら 3-1 に戻ること。

3-1 に戻ること。早期脱出条件に当たれば Step 4 へ進むこと。

### Step 4 — driver 登録を解除してサマリを報告すること

早期脱出時も driver 登録の解除を飛ばしてはならない。

source が尽きた / ユーザー中断 / 予算超過のいずれかで、次の解除を実行すること。ここでも cwd = このセッションの linked worktree にすること。main の作業ツリーでは `backlog` / `compass` の store 書き込みが拒否される [test: crates/backlog/tests/store_worktree_boundary.rs::done_from_primary_checkout_is_refused_and_file_unchanged] [test: crates/compass/tests/store_worktree_boundary.rs::outcome_from_primary_checkout_is_refused_and_writes_nothing]:

```bash
backlog driver unregister --session-id "$CLAUDE_CODE_SESSION_ID" --project "$PWD"
```

Step 2 で例外的に `backlog lock acquire` した場合のみ `backlog lock release --project "$PWD"` も打つこと [test: scripts/test_prose_flow_b.py::TestLockRelease::test_claim_lock_release]。

3-1 で `overwatch begin` した各 `<pdo-unit-id>` について、対応する `end` を呼ぶこと [test: crates/overwatch/tests/backlog_s04_audit.rs::s3b056c02_end_of_missing_key_is_distinguishable_from_release]:
```bash
overwatch end --key "<pdo-unit-id>" --status "<done|abandoned>"
```
- `--status` は sink の結果を反映すること（成功で閉じたなら `done`、失敗・未完で手放すなら `abandoned`） [test: scripts/test_prose_flow_b.py::TestOverwatchEnd::test_claim_overwatch_end]。
- バッチなら item ごとに `overwatch end` を呼ぶこと（begin と同じ key＝各 item の `hashkey` を使う） [test: scripts/test_prose_flow_b.py::TestOverwatchEnd::test_claim_overwatch_end] [test: scripts/test_prose_flow_b.py::TestOverwatchEnd::test_claim_overwatch_end_unknown_key_is_distinguishable]。

最後に「処理件数・成功・失敗・残キュー・次に取り直した gap」を報告すること。

#### ループ終端で方向判断を行うこと

勝手に `/compass` を起動してはならない（彫り直しは保留する） [test: scripts/test_prose_flow_a.py::T::test_claim_judgment_pivot_gate_no_approval_escalates_and_recommends_continue]。

driver 登録の解除直後、ループを正常終了した場合（中断・エラー以外）は以下を実行すること。

```bash
compass pivot-check   # {"recommendation":"persevere"|"pivot","streak":N,"threshold":N,"reason":"…"} [test: scripts/test_prose_flow_b.py::TestPivotCheckShape::test_claim_pivot_check_shape]
```

- `persevere` ならそのまま継続し、「次の gap を取り直す」と報告すること [test: crates/compass/tests/integration.rs::pivot_check_emits_persevere_with_no_outcomes]。
- `pivot` なら Step 0.5 の policy-answer routing に通すこと（`--risk medium --reversible high --confidence low`、既定 verdict は escalate、
  `--question "pivot 兆候: north_star を彫り直す?" --option "再オリエンテーション" --option "継続" --recommend 1`） [test: scripts/test_prose_flow_a.py::T::test_claim_judgment_pivot_gate_no_approval_escalates_and_recommends_continue]。
  - escalate（exit 2）／ 非自律・フォールバックなら、`reason`（streak 長・対象 verdict 列）を引用してユーザーに提示し、
    north_star を彫り直す（再オリエンテーション）か否かを問うこと（`AskUserQuestion`）、「再オリエンテーション」なら
    `/compass` を案内して終了し、「継続」なら通常どおり報告して終了すること [test: scripts/test_prose_flow_a.py::T::test_claim_judgment_pivot_gate_no_approval_escalates_and_recommends_continue] [test: scripts/test_prose_flow_b.py::TestPivotCheckShape::test_claim_pivot_check_shape] [test: scripts/test_prose_flow_a.py::T::test_claim_frontmatter_declares_args_and_tools]。
  - auto（exit 0）なら `chosen`（既定案＝継続/persevere）を採用し、`reason` を報告に引用しつつループは止めず「次の gap を取り直す」で継続すること [test: crates/condukt/tests/policy_cli.rs::auto_verdict_self_answers_recommended_option_and_journals]。自答は監査ログに残る [test: crates/condukt/tests/policy_cli.rs::auto_verdict_self_answers_recommended_option_and_journals]。
- outcomes が 0 件なら pivot-check はスキップしてよい [test: crates/compass/tests/integration.rs::pivot_check_emits_persevere_with_no_outcomes]。

## 早期脱出条件に当たったらループを止めること

- 予算の判定不能（`budgetguard check` の exit 0 以外）を予算内として続行してはならない [test: crates/budgetguard/tests/check_contract.rs::check_corrupt_ledger_is_undetermined_exit_2] [test: crates/budgetguard/tests/check_contract.rs::check_unreadable_ledger_is_undetermined_exit_2] [test: crates/budgetguard/tests/check_contract.rs::check_corrupt_config_is_undetermined_exit_2] [test: scripts/test_prose_flow_b.py::TestBudgetCheck::test_claim_budget_corrupt_ledger_is_nonzero]。
- `--session` を付け忘れてはならない [test: crates/condukt/tests/backlog_audit_v11.rs::backlog_9eeffa69_flow_skill_passes_session_to_circuit_check]。付け忘れると毎バッチ後に trip する [test: crates/condukt/tests/circuit_session_idle_e2e.rs::no_session_flag_trips_unmeasured_control]。
- 彫り直しを勝手に繰り返してはならない。
- 「仕事なし」にも「実行してよい」にも丸めてはならない。

ループの各イテレーション（3-4 の継続判定の一部）で `condukt circuit check --run "flow-$CLAUDE_CODE_SESSION_ID" --session "$CLAUDE_CODE_SESSION_ID"` を実行すること [test: crates/condukt/tests/backlog_audit_v11.rs::backlog_9eeffa69_flow_skill_passes_session_to_circuit_check]。
trip しなければ exit 0（continue）になる [test: crates/condukt/src/circuit.rs::healthy_run_continues_with_zero_exit]。
trip を観測したら人にも policy にも聞かず即 clean stop して Step 4 へ進むこと。停止理由 slug は JSONL に記録される [test: crates/condukt/src/circuit.rs::failure_streak_over_cap_trips_with_nonzero_exit]。
idle 軸は fail-closed で、測れない idle は `idle_unmeasured` で trip する [test: crates/condukt/src/circuit.rs::unmeasurable_idle_trips_instead_of_continuing]。
idle は live な stateless claim があれば、その所有 session の transcript の mtime から測る [test: crates/condukt/tests/circuit_stateless_idle_e2e.rs::stateless_claim_with_stale_transcript_trips_on_stall]。
claim が無ければ、`--session` で渡した session の transcript の mtime から測る [test: crates/condukt/tests/circuit_session_idle_e2e.rs::session_flag_stale_transcript_trips_on_idle_not_unmeasured]。
これは `--run` がちょうど `flow-<SESSION>` のときだけ有効である [test: crates/condukt/tests/circuit_session_idle_e2e.rs::session_flag_run_id_mismatch_trips_unmeasured]。
JSON の `idle_source` が `session_flag` になる [test: scripts/test_prose_flow_a.py::T::test_claim_circuit_idle_source_is_session_flag_with_fresh_transcript]。
transcript が見つからない場合は `idle_unmeasured` で trip する [test: crates/condukt/tests/circuit_session_idle_e2e.rs::session_flag_missing_transcript_trips_unmeasured]。
HOME 未設定・claim registry が読めない場合も `idle_unmeasured` で trip する [test: scripts/test_prose_flow_a.py::T::test_claim_circuit_idle_home_unset_trips_unmeasured] [test: scripts/test_prose_flow_a.py::T::test_claim_circuit_idle_unreadable_claim_registry_trips_unmeasured]。

- ユーザーが中断を指示したら、直ちに Step 4（driver 登録の解除）へ進むこと。
- 循環ブレーカーが trip（毎イテレーション `condukt circuit check --run RID` を実行し nonzero）したら、決定論的に clean stop すること（ループを止めて Step 4 へ進み、人にも policy にも聞かない hard stop）[test: scripts/test_prose_flow_a.py::T::test_claim_circuit_idle_missing_transcript_trips_unmeasured] [test: scripts/test_prose_flow_a.py::T::test_claim_policy_table_circuit_trip_exits_nonzero]。
  非自律で追加確認を入れたい場合のフォールバックのみ `AskUserQuestion`「続行 / 中止」を使うこと [test: scripts/test_prose_flow_a.py::T::test_claim_frontmatter_declares_args_and_tools] [test: scripts/test_prose_flow_a.py::T::test_claim_policy_table_circuit_trip_exits_nonzero]。
- `budgetguard check` が exit 0 以外（exit 1＝予算超過、exit 2＝判定不能、`budgetguard` 不在・起動失敗を含む）を返したら、予算超過とみなしてループ終了（Step 4）とし、残キューはそのまま次セッションへ引き継ぐこと [test: scripts/test_prose_flow_b.py::TestBudgetCheck::test_claim_budget_over_is_nonzero] [test: scripts/test_prose_flow_b.py::TestBudgetCheck::test_claim_budget_corrupt_ledger_is_nonzero] [test: scripts/test_prose_flow_b.py::TestBudgetCheck::test_claim_budget_corrupt_config_is_nonzero] [test: scripts/test_prose_flow_b.py::TestBudgetCheck::test_claim_budget_missing_binary_is_nonzero] [test: scripts/test_prose_flow_b.py::TestBudgetCheck::test_claim_budget_prose_nonzero_goes_to_step_4]。
- compass フォールバック（3-1 の 3）で charter を彫っても右サイズの一手が出ない、または `sentinel` で離席なら、ループを止め Step 4 へ進むこと [test: scripts/test_prose_flow_b.py::TestCarveSentinel::test_claim_carve_sentinel]。
- `compass` バイナリ不在・呼び出し失敗（charter の状態を観測できなかった）なら、その事実を報告して Step 4 へ進むこと [test: scripts/test_prose_flow_b.py::TestCompassFailDetectable::test_claim_compass_fail_detectable_no_charter] [test: scripts/test_prose_flow_b.py::TestCompassFailDetectable::test_claim_compass_fail_detectable_unreadable_charter]。
- `backlog next` が予期しないエラーなら、報告して Step 4 へ進むこと [test: scripts/test_prose_flow_b.py::TestBacklogNextError::test_claim_backlog_next_error]。

## 次のハードルールを守ること

- 停止条件は backlog pending・measure・compass 主筋 の 3 つだけとし、仕事が無いのに繰り返してはならない。
  この 3 つがどれも一手を出さないなら Step 4 へ抜けること。`open` 仮説を継続理由にしてはならない [test: scripts/test_prose_flow_b.py::TestHypStatus::test_claim_hyp_status]。
  残った `open` 仮説と観測不能な awaiting-measurement は報告するだけにすること [test: scripts/test_prose_flow_b.py::TestHypStatus::test_claim_hyp_status]。
  pending が残っているのに早期に止めてはならない。3 つのどれかが一手を出す限りは止めないこと。詳細は Step 3-1 の 5 を見ること。
- ユーザーの課題から逸脱してはならない。着手した課題は、終わるまでそれだけをやること。
  作業中に別の問題（より重要に見えるもの・途中で詰まった原因・気づいた欠陥）が現れても、そこへ乗り換えてはならない。
  判定基準は常に「いまユーザーの prompt に忠実か」とし、それが解決するまではその課題に注力すること。
  見つけた別課題は、その場で再現テストを書いて `backlog add --repro-test` し、戻ること [test: crates/backlog/tests/repro_evidence.rs::reproduced_lands_pending_and_records_outcome]。
  課題が完了してから、自律モードであれば次の backlog を処理すること。別課題を並行して処理してはならない。
  逸脱してよい唯一の例外は、その問題を解決しないと元の課題が物理的に前に進まない場合であり、そのときも「元の課題のために何を迂回しているか」を明示してから行い、済み次第すぐ戻ること。
- テストが無ければ起票してはならない（2026-10-04 ユーザー裁定:
  「testがないなら起票してはならない」）。気づいたことは、起票するかどうかを判断せず常に起票すること。
  起票は **(1) 再現テスト（RED）を書いてコミット → (2) `backlog add --repro-test "<cmd>"`** の順で行うこと [test: crates/backlog/tests/repro_evidence.rs::reproduced_lands_pending_and_records_outcome]。
  `backlog add` は再現した時だけ task を書き、それ以外は拒否して store に何も書かない [test: crates/backlog/tests/repro_evidence.rs::reproduced_lands_pending_and_records_outcome] [test: crates/backlog/tests/no_confirm_no_unproven.rs::add_without_repro_writes_nothing_anywhere] [test: crates/backlog/tests/no_confirm_no_unproven.rs::add_with_non_reproducing_repro_writes_nothing_anywhere]。
  テストが書けないなら Ask すること。
  「些細だ」「意図的な仕様かもしれない」「今回のスコープ外だ」を理由にこの手順を省かないこと。
  **それが本当に課題かどうかは、後でチケットから判定すること**。
  **起票の中身**には逐語の実測値・測定点（rev と日付）・判定不能なら**両方の読み**（(a) 実在の欠陥 / (b) 意図的な仕様）と、
  **どちらかを見分ける方法**を書くこと。
  起票を着手として扱わず、書いたら元の課題へ戻ること。
- source と executor の役割を混ぜてはならない。課題の選定は compass と backlog、実行は condukt に任せ、flow 自身は判定とループだけを担うこと。
- キューを独占してはならない。`backlog list` で覗いて上位 N 件を自分のものと決めるのは、純粋 read で重複着手するため禁止 [test: scripts/test_prose_flow_b.py::TestListPure::test_claim_list_pure]。
  待つのは解ではないので、他セッションが driver 登録済みでも見送ってはならない。
  `/flow` は同じ project で並走してよい（排他は task 単位＝`next --claim` + `condukt state claim-task`）[test: crates/backlog/tests/concurrent_drivers.rs::two_concurrent_driver_processes_get_disjoint_tasks]。
- 複数 condukt run を並走させてはならない。並列はバッチを 1 condukt run に束ねて実現すること。
  backlog に複数 ready 課題があれば順列ではなく 1 回の condukt run に束ねて渡し、並列か直列かの実判定は condukt の `schedule.rs` に委譲すること [test: crates/condukt/tests/schedule_conflict_capstone_e2e.rs::conflicting_pairs_are_forced_serial_and_independent_pair_stays_parallel]。
  flow 自身は独立候補を束ねるだけにし、予算逼迫や明白な相互依存が読めるときは flow 側でバッチ幅を絞ること。極端な場合は N=1 にすること。
- 盲目実行してはならない。compass 由来の一手は、charter が鮮明でない限り condukt へ流してはならない（3-1 の 3 のとおり、彫り直してから引くこと）。
  backlog の item には、この charter 鮮度ゲートを適用しないこと。
- 早期脱出・エラー時も、driver 登録の解除を絶対に飛ばしてはならない。
- Step 0.5 のとおり、自律モードでは human gate を `condukt policy answer` に通すこと [test: crates/condukt/tests/policy_cli.rs::auto_verdict_self_answers_recommended_option_and_journals]。`autonomy-check` が exit 0 のとき [test: crates/condukt/tests/autonomy_single_switch.rs::a1_switch_file_on_makes_autonomy_check_pass_with_switch_file_source]、
  各ゲートを per-gate の risk×reversible×confidence で `policy answer` に掛け、
  auto は自答（Ask 撤去・監査ログに追記）[test: crates/condukt/tests/policy_cli.rs::auto_verdict_self_answers_recommended_option_and_journals]、escalate は従来 Ask（残す質疑）[test: crates/condukt/tests/policy_cli.rs::escalate_verdict_falls_through_but_is_still_journaled]、block は拒否として扱うこと [test: crates/condukt/tests/policy_cli.rs::block_verdict_is_a_hard_stop_exit_3]。
  早期脱出は policy answer に通さず、決定論の `condukt circuit check` が trip すれば人にも policy にも聞かず clean stop すること（上記「早期脱出」）[test: scripts/test_prose_flow_a.py::T::test_claim_circuit_idle_missing_transcript_trips_unmeasured]。
- 判断を求める Ask には、迷ったときも `--approval` を付けてはならない [test: scripts/test_prose_flow_a.py::T::test_claim_judgment_approval_flag_is_exactly_what_erases_the_question] [test: scripts/test_prose_flow_a.py::T::test_claim_judgment_no_approval_on_judgment_gates_in_skill_text]。
  判断要求（pivot / worker blocked / merge conflict の pick-a-side / §2 の測れない決定 /
  spec-gap divert / charter 彫り直し中の問い）は `--approval` を付けず、escalate のまま残すこと [test: crates/condukt/tests/autonomy_invariant.rs::policy_answer_baseline_escalates_the_ambiguous_middle]。
  charter 彫り直しの問いを自答してはならない。人に聞くこと。
  YES/NO の権限認可（排他ロック競合＝stand down、resume＝優先 pick 先頭、deploy/push の GATED 承認、
  condukt Phase 3 の合意）には `--approval` を付けること [test: crates/condukt/tests/autonomy_invariant.rs::policy_answer_approval_self_answers_when_autonomous]。常設許諾（2026-08-07）により auto で自答される [test: crates/condukt/tests/autonomy_invariant.rs::policy_answer_approval_self_answers_when_autonomous]。
  常設許諾は block を緩めない [test: crates/condukt/tests/autonomy_invariant.rs::policy_answer_approval_never_relaxes_the_irreversible_block]。
  `--approval` は非自律では不活性である [test: crates/condukt/tests/autonomy_invariant.rs::policy_answer_approval_is_inert_when_not_autonomous]。
  自律でも escalate のまま残るのは (a) worker blocked (b) pivot (c) conflict/untestable の判断要求
  (d) spec-gap divert（3-1.5） (e) charter 彫り直し中の問い（3-1 の 3）であり、policy が block を返したゲートも止まる [test: scripts/test_prose_flow_a.py::T::test_claim_policy_table_rows_verdicts] [test: scripts/test_prose_flow_a.py::T::test_claim_policy_table_conflict_and_untestable_escalate_even_with_approval] [test: scripts/test_prose_flow_a.py::T::test_claim_policy_table_block_is_never_relaxed_by_approval]。
  exit 1（既定・非自律）は従来どおり全 Ask を維持すること。
  `autonomy-check` が exit 0 以外なら非自律とみなすこと [test: scripts/test_prose_flow_a.py::T::test_claim_autonomy_unknown_subcommand_never_exits_zero]。
