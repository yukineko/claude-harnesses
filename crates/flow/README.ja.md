```
SOURCE（課題の供給）                          EXECUTOR（解決手段の実行）
  compass     … 次の右サイズの一手             ─┐
  backlog     … 確定済みキュー                  ├─▶  condukt（fugu-router がモデル選択）─▶ verify
  hypothesis  … PDO 仮説の build / measure      │
  prompt      … ユーザー直の課題文             ─┘
```

キュー全体は**ロックしない**: 同じ project で複数セッションが同時に `/flow` を回してよい [test: crates/integration-tests/tests/flow_skill_queue_contract.rs::the_loop_does_not_acquire_the_exclusive_project_lock] [test: crates/integration-tests/tests/flow_skill_queue_contract.rs::a_peer_driver_is_not_a_reason_to_stand_down]。
排他は task 単位（選択と予約を同一クリティカルセクションで行う `backlog next --claim` と `condukt state claim-task`）で、各セッションは `backlog driver register` で非排他に driver 登録する [test: crates/integration-tests/tests/flow_skill_queue_contract.rs::the_pick_reserves_the_task_instead_of_only_reading_it] [test: crates/integration-tests/tests/flow_skill_queue_contract.rs::the_per_task_claim_guards_are_retained] [test: crates/integration-tests/tests/flow_skill_queue_contract.rs::the_loop_does_not_acquire_the_exclusive_project_lock]。

早期脱出・エラー時も、driver 登録の解除を飛ばしてはならない。
盲目実行をしない。

```
0. 引数分岐 — 課題文があれば condukt に直行（1 件だけ実行）
0.5. 自律ゲート — `condukt state autonomy-check`。自律モードなら各 human gate を
     `condukt policy answer`（risk×reversible×confidence の graded 判定）に通す（下記参照）
1. compass は先に引かない — 引数なしの /flow は backlog 優先（2026-09-23 裁定）。
     compass は backlog が一手も出さなかったときのフォールバック
2. driver 登録（非排他）— backlog driver register。キュー全体はロックしない。
     明示的に lock acquire された排他ロックが生きている場合だけ尊重する
3. 実行ループ — ピック（backlog next --claim、claim-skip ゲート）→ spec-gap ゲート
       → 着手前に claim（condukt state claim-task、TOCTOU ガード）
       → overwatch anchor 登録（overwatch begin）→ /condukt → 検証 → sink
       ピック順: measure step → backlog（主経路）→ backlog が空のときだけ compass gap
                 （open 仮説はユーザーが明示指示したときだけ。自動ループでは報告のみ）
       sink: backlog done / compass outcome（前進・不変・後退を記録）
             / hypothesis は出荷で awaiting-measurement、計測後に validate/reject（証拠必須）
             / fugu-router に record
             / いずれも claim を release、ループ中は heartbeat で claim を live に保つ
4. 終了 — source が尽きる/予算超過/中断で backlog driver unregister（例外的に lock acquire した
     場合のみ lock release）+ overwatch end + サマリ報告 + pivot-check
```

自律判定とゲートの振り分けは `skills/flow/SKILL.md` の Step 0.5 を正とする [test: crates/condukt/tests/autonomy_invariant.rs::invariant_prose_anchors_are_present]。

verdict が **auto**（exit 0）のとき、`condukt policy answer` は推奨オプションを自答し、`gate-decisions.jsonl` に記録する
（`condukt policy answers` で後から監査可能） [test: crates/condukt/tests/policy_cli.rs::auto_verdict_self_answers_recommended_option_and_journals] [test: crates/condukt/tests/policy_cli.rs::answers_reader_replays_the_audit_trail]。
auto の verdict では **Ask しない**こと。

**flow は hook を持たない [test: scripts/test_prose_flow_a.py::T::test_claim_layout_no_cargo_no_bin_no_hooks] [test: scripts/test_prose_flow_a.py::T::test_claim_no_propose_no_hook_runs_flow]。`/flow` は明示的に起動したときだけ走る [test: scripts/test_prose_flow_a.py::T::test_claim_no_propose_no_hook_runs_flow] [test: scripts/test_prose_flow_a.py::T::test_claim_no_propose_autoflow_has_no_sessionstart_and_no_proposal_strings] [test: scripts/test_prose_flow_a.py::T::test_claim_no_propose_autoflow_stop_output_never_proposes_flow]。**

[record: 2026-08-20 rev: 5289312f] 0.2.6 までは `flow propose` という SessionStart hook があり、backlog に pending が
あれば「他の作業を始める前に、AskUserQuestion 1 回で `/flow` を開始するかユーザーに
確認してください」というディレクティブを毎セッション注入しており、**ユーザーの指示で
これを廃止した**（同時に autoflow 側の 2 経路 — SessionStart の
「バックログに N 件…/flow で開始しますか？」と、Stop hook で毎ターン
「/backlog を実行してください」と block していたアーム — も撤去）。

flow は **skill のみのプラグイン**である [test: scripts/test_prose_flow_a.py::T::test_claim_layout_no_cargo_no_bin_no_hooks]。バイナリ・ランチャ・hook はいずれも無く、Cargo クレートでもない [test: scripts/test_prose_flow_a.py::T::test_claim_layout_no_cargo_no_bin_no_hooks] [test: scripts/test_prose_flow_a.py::T::test_claim_layout_not_a_workspace_member]。

プラグインは `/flow` skill のみを同梱する [test: scripts/test_prose_flow_a.py::T::test_claim_layout_only_skill_is_flow]。

```text
/plugin marketplace add yukineko/claude-harnesses
/plugin install flow@yukineko
```

```
.claude-plugin/plugin.json     # プラグインマニフェスト [test: scripts/test_prose_flow_a.py::T::test_claim_layout_plugin_json_valid_name_flow_no_hooks] [test: scripts/test_prose_flow_a.py::T::test_claim_layout_readme_prose_and_fence_comments]
skills/flow/SKILL.md           # /flow skill（source→executor ループを駆動） [test: scripts/test_prose_flow_a.py::T::test_claim_layout_only_skill_is_flow] [test: scripts/test_prose_flow_a.py::T::test_claim_layout_readme_prose_and_fence_comments]
```

[record: 2026-08-20 rev: 5289312f] 0.2.6 までは `hooks/hooks.json`・`bin/flow`（POSIX ランチャ）・`bin/flow-<os>-<arch>`・
`src/main.rs`・`Cargo.toml` も同梱しており、すべて `flow propose` のためのもので、それが廃止されたので一緒に消えた。

Rust クレートではないので `cargo` は関わらない [test: scripts/test_prose_flow_a.py::T::test_claim_layout_no_cargo_no_bin_no_hooks] [test: scripts/test_prose_flow_a.py::T::test_claim_layout_not_a_workspace_member]。`/flow` skill の
queue-driving 契約は `crates/integration-tests/tests/flow_skill_queue_contract.rs`
がテキストとして pin している [test: crates/integration-tests/tests/flow_skill_queue_contract.rs::the_loop_does_not_acquire_the_exclusive_project_lock] [test: crates/integration-tests/tests/flow_skill_queue_contract.rs::the_pick_reserves_the_task_instead_of_only_reading_it] [test: crates/integration-tests/tests/flow_skill_queue_contract.rs::a_peer_driver_is_not_a_reason_to_stand_down] [test: crates/integration-tests/tests/flow_skill_queue_contract.rs::the_per_task_claim_guards_are_retained] [test: crates/integration-tests/tests/flow_skill_queue_contract.rs::undetermined_liveness_is_not_read_as_free] [test: crates/integration-tests/tests/flow_skill_queue_contract.rs::every_claim_task_invocation_is_marked_stateless]。

```sh
cargo test -p integration-tests --test flow_skill_queue_contract
```

MIT [test: scripts/test_prose_flow_a.py::T::test_claim_license_mit]
