```
SOURCE（課題の供給）                          EXECUTOR（解決手段の実行）
  compass     … 次の右サイズの一手             ─┐
  backlog     … 確定済みキュー                  ├─▶  condukt（fugu-router がモデル選択）─▶ verify
  hypothesis  … PDO 仮説の build / measure      │
  prompt      … ユーザー直の課題文             ─┘

出荷した仮説は condukt skill が gate PASS 後に awaiting-measurement へ遷移させ、次サイクルの measure step が
計測して validate/reject する（出荷 ≠ 検証 ＝ build ≠ validate）。
```

It does **not** lock the whole queue: several sessions may run `/flow` on the same project at once [test: crates/integration-tests/tests/flow_skill_queue_contract.rs::the_loop_does_not_acquire_the_exclusive_project_lock] [test: crates/integration-tests/tests/flow_skill_queue_contract.rs::a_peer_driver_is_not_a_reason_to_stand_down].
Exclusion is per task — `backlog next --claim` (pick and reserve in one critical section) plus `condukt state claim-task` — and each session registers itself non-exclusively with `backlog driver register` [test: crates/integration-tests/tests/flow_skill_queue_contract.rs::the_pick_reserves_the_task_instead_of_only_reading_it] [test: crates/integration-tests/tests/flow_skill_queue_contract.rs::the_per_task_claim_guards_are_retained] [test: crates/integration-tests/tests/flow_skill_queue_contract.rs::the_loop_does_not_acquire_the_exclusive_project_lock].

早期脱出・エラー時も、driver 登録の解除を飛ばしてはならない。
盲目実行をしない。

```
0. 引数分岐 — 課題文があれば source 選択を飛ばして condukt に直行（1 件だけ実行）
0.5. 自律ゲート — `condukt state autonomy-check`。自律モードなら各 human gate を
     `condukt policy answer`（risk×reversible×confidence の graded 判定）に通す（下記参照）
1. compass は先に引かない — 引数なしの /flow は backlog 優先（2026-09-23 裁定）。
     compass は 3-1 の backlog が一手も出さなかったときのフォールバック
2. driver 登録（非排他）— `backlog driver register`。キュー全体はロックしない。
     他セッションの driver が居ても見送らない。誰かが明示的に `backlog lock acquire` した
     排他ロックが生きている場合だけ尊重する
3. 実行ループ — ピック（`backlog next --claim` で選択と予約を同一クリティカルセクションで行い、
       claim-skip ゲート）→ spec-gap ゲート → 着手前に claim（`condukt state claim-task`、TOCTOU ガード）
       → overwatch anchor 登録（overwatch begin）→ /condukt → 検証 → sink
       ピック順: measure step（awaiting-measurement の仮説を計測して validate/reject で閉じる）
                 → `backlog`（主経路。複数 ready 課題は順列でなく 1 condukt run に束ね、並列/直列は
                    condukt の schedule.rs に委譲＝非衝突は並列・衝突/危険は自動で直列。sink は item ごと）
                 → backlog が空のときだけ `compass gap`（charter が陳腐ならその場で彫り直す）
                 （open 仮説はユーザーが明示指示したときだけ。自動ループでは残課題として報告のみ）
       成功 sink: backlog done
                 / compass は `compass outcome` で measuring_stick 判定（前進/不変/後退）を記録
                 / hypothesis は出荷で awaiting-measurement、計測後に validate/reject（証拠必須）
                 / fugu-router に record
                 / いずれも claim を release、ループ中は heartbeat で claim を live に保つ
       失敗: backlog fail --reason …、スキップして次へ
4. 終了 — source が尽きる/予算超過/中断で `backlog driver unregister`（例外的に lock acquire した
     場合のみ lock release）+ `overwatch end` + サマリ報告 + pivot-check
```

The autonomy check and the gate routing are specified in `skills/flow/SKILL.md` Step 0.5 [test: crates/condukt/tests/autonomy_invariant.rs::invariant_prose_anchors_are_present].

On an **auto** verdict (exit 0), `condukt policy answer` self-answers the recommended option and logs it to
`gate-decisions.jsonl` (auditable via `condukt policy answers`) [test: crates/condukt/tests/policy_cli.rs::auto_verdict_self_answers_recommended_option_and_journals] [test: crates/condukt/tests/policy_cli.rs::answers_reader_replays_the_audit_trail].
Do **not** ask the user on an auto verdict.

**flow has no hook [test: scripts/test_prose_flow_a.py::T::test_claim_layout_no_cargo_no_bin_no_hooks] [test: scripts/test_prose_flow_a.py::T::test_claim_no_propose_no_hook_runs_flow]. `/flow` runs when, and only when, you invoke it.** [test: scripts/test_prose_flow_a.py::T::test_claim_no_propose_no_hook_runs_flow] [test: scripts/test_prose_flow_a.py::T::test_claim_no_propose_autoflow_has_no_sessionstart_and_no_proposal_strings] [test: scripts/test_prose_flow_a.py::T::test_claim_no_propose_autoflow_stop_output_never_proposes_flow]

[record: 2026-08-20 rev: 5289312f] Through 0.2.6 a `flow propose` SessionStart hook injected an L2
propose-then-confirm directive every session the backlog had pending items: *"before
starting other work, ask the user with a single AskUserQuestion whether to start
`/flow`."* **It was retired on the user's instruction (2026-08-20)**, together with
autoflow's two paths that said the same thing — its SessionStart "バックログに N 件…
/flow で開始しますか？" and its Stop-hook arm that blocked every single turn with
"/backlog を実行してください".

flow is a **skills-only plugin**: no binary, no launcher, no hook, not a Cargo crate. [test: scripts/test_prose_flow_a.py::T::test_claim_layout_no_cargo_no_bin_no_hooks] [test: scripts/test_prose_flow_a.py::T::test_claim_layout_not_a_workspace_member]

The plugin bundles the `/flow` skill and nothing else. [test: scripts/test_prose_flow_a.py::T::test_claim_layout_only_skill_is_flow]

```text
# in Claude Code:
/plugin marketplace add yukineko/claude-harnesses
/plugin install flow@yukineko
```

```
.claude-plugin/plugin.json     # plugin manifest [test: scripts/test_prose_flow_a.py::T::test_claim_layout_plugin_json_valid_name_flow_no_hooks] [test: scripts/test_prose_flow_a.py::T::test_claim_layout_readme_prose_and_fence_comments]
skills/flow/SKILL.md           # the /flow skill (drives the source→executor loop) [test: scripts/test_prose_flow_a.py::T::test_claim_layout_only_skill_is_flow] [test: scripts/test_prose_flow_a.py::T::test_claim_layout_readme_prose_and_fence_comments]
```

[record: 2026-08-20 rev: 5289312f] Through 0.2.6 it also shipped `hooks/hooks.json`, `bin/flow` (a POSIX launcher),
`bin/flow-<os>-<arch>`, `src/main.rs` and `Cargo.toml`, all of which existed to serve
`flow propose` and went with it.

Not a Cargo crate, so `cargo` is not involved in shipping it [test: scripts/test_prose_flow_a.py::T::test_claim_layout_no_cargo_no_bin_no_hooks] [test: scripts/test_prose_flow_a.py::T::test_claim_layout_not_a_workspace_member]. The `/flow` skill's
queue-driving contract is pinned as text by
`crates/integration-tests/tests/flow_skill_queue_contract.rs`: [test: crates/integration-tests/tests/flow_skill_queue_contract.rs::the_loop_does_not_acquire_the_exclusive_project_lock] [test: crates/integration-tests/tests/flow_skill_queue_contract.rs::the_pick_reserves_the_task_instead_of_only_reading_it] [test: crates/integration-tests/tests/flow_skill_queue_contract.rs::a_peer_driver_is_not_a_reason_to_stand_down] [test: crates/integration-tests/tests/flow_skill_queue_contract.rs::the_per_task_claim_guards_are_retained] [test: crates/integration-tests/tests/flow_skill_queue_contract.rs::undetermined_liveness_is_not_read_as_free] [test: crates/integration-tests/tests/flow_skill_queue_contract.rs::every_claim_task_invocation_is_marked_stateless]

```sh
cargo test -p integration-tests --test flow_skill_queue_contract
```

MIT [test: scripts/test_prose_flow_a.py::T::test_claim_license_mit]
