---
name: backlog
description: クロスプロジェクト backlog の queue+state を操作するコマンド群へのショートカット。ループ driver は /flow に統合された。
argument-hint: [サブコマンド: list / next / done / fail / driver / lock]
allowed-tools: Bash(backlog:*), Bash(git:*), Read
---

# /backlog — backlog queue+state へのショートカット

`/backlog` は **backlog binary が提供する queue・state 操作**（`list` / `next` / `done` / `fail` /
`driver` / `lock`）を呼び出すための薄いエントリポイント。

> **ループ driver（driver 登録 → `next --claim` でピック → /condukt → done/fail → 登録解除）は `/flow` に統合されました。**
> バックログのアイテムを順に実装したい場合は **`/flow`** を使ってください。
> `/flow` は compass ゲート・budgetguard・fugu-router によるモデル選択も含む上位互換 driver です。

## backlog binary が提供するコマンド

```bash
backlog list --status pending [--project <path>]   # キュー一覧（純粋な read。ピックには使わない）
backlog next [--project <path>]                    # 次のアイテムを覗く（予約しない）
backlog next --claim [--project <path>]            # 次のアイテムを予約して取る（driver はこちら）
backlog done <id> --test "<cmd>" --red-rev <rev>   # 完了（committed test が REV で RED・HEAD で GREEN を実測）
backlog done <id> --doc-only <commit>              # 完了（doc だけを触る祖先 commit が証拠）
backlog done <id> --duplicate-of <id>              # 完了（重複。相手は pending / claimed / done）
backlog add --repro-test "<cmd>" ...               # 再現した時だけ pending で起票し、それ以外は非 0 で拒否して store に何も書かない [test: crates/backlog/tests/repro_evidence.rs::reproduced_lands_pending_and_records_outcome] [test: crates/backlog/tests/no_confirm_no_unproven.rs::add_without_repro_writes_nothing_anywhere] [test: crates/backlog/tests/no_confirm_no_unproven.rs::add_with_non_reproducing_repro_writes_nothing_anywhere]
backlog audit-open [--json]                        # open 行（pending/failed）に記録済み再現テストがあるかを、テストを実行せずに分類する [test: crates/backlog/tests/audit_open_once.rs::first_run_classifies_open_rows_and_excludes_terminal] [test: crates/backlog/tests/audit_open_once.rs::no_test_is_executed]
backlog audit-closures [--json]                    # 既存 close の証拠クラスを集計（read-only）
backlog fail <id> --reason "<概要>"                # アイテムを失敗マーク（2 日後に再キューされる）
backlog cancel <id> --reason "<理由>"              # やらないと決めて捨てる（終端。`discard` closure として理由を記録。証明できない推測はこれで捨てる。done と違い完了を主張しない）

# driver の存在通知（非排他。何セッションでも同時に登録できる）
backlog driver register   --session-id <id> --project <path>
backlog driver heartbeat  --session-id <id> --project <path>
backlog driver unregister --session-id <id> --project <path>
backlog driver status [--project <path>]           # 登録中の driver 一覧（active / count / drivers）

# 排他ロック（「全セッションを締め出す」ときだけ。キューを回すのに取る必要は無い）
backlog lock status [--project <path>]             # 排他ロック + driver 登録の合算ステータス（project 省略時は全 project 横断）
backlog lock acquire --session-id <id> --project <path>
backlog lock release --project <path>
```

> **キューを回すのに排他ロックは要らない。** `next --claim` が選択と予約を同一クリティカル
> セクションで行うので、複数セッションが同じ project のキューを同時に消化しても同じ task を
> 二重に掴むことはない。`lock acquire` はキュー全体を独占するので、**人が意図的に全セッションを
> 締め出すときだけ**使う。
> driver 登録も排他ロックも **project ごとにスコープ**される。`--project` は `status` 以外必須
> （`status` 省略時は全 project 横断スキャン＝`daily` の起動判定用）。
>
> **キュー側の `--project` は意味が違う。** store は repo ごと（`<repo root>/.backlog/tasks.toml`）
> なので、`list` / `next` の `--project` は絞り込みではなく「どの store のことを言っているか」の
> assertion であり、別 repo を指すとエラーになる（絞り込まれた一覧は返らない）。この repo の
> タスクは全部スコープ内なので、通常は **付けなくてよい**。repo root が上に無い cwd では
> `add`/`list`/`next` は拒否する（`~/.backlog` へフォールバックしない）ので、キュー操作は
> 必ず repo の中から実行する。

## 起票の基準 — テストが無ければ起票しない

- 見つけたら**まず再現テスト（RED）を書いてコミット**し、`backlog add --repro-test "<cmd>"` で起票すること [test: crates/backlog/tests/repro_evidence.rs::reproduced_lands_pending_and_records_outcome]。
  `<cmd>` が HEAD で振る舞いとして失敗（＝再現）した時だけ task が `pending` で書かれる [test: crates/backlog/tests/repro_evidence.rs::reproduced_lands_pending_and_records_outcome]。
- それ以外は非 0 終了で拒否し、store に何も書かない: `--repro-test` 無し [test: crates/backlog/tests/no_confirm_no_unproven.rs::add_without_repro_writes_nothing_anywhere]、再現せず [test: crates/backlog/tests/no_confirm_no_unproven.rs::add_with_non_reproducing_repro_writes_nothing_anywhere]、判定不能 [test: crates/backlog/tests/repro_evidence.rs::undetermined_repro_files_no_row_and_writes_nothing]。
- `next` / `next --claim` は reproduced な repro の無い行を渡さない [test: crates/backlog/tests/no_confirm_no_unproven.rs::next_skips_pending_row_without_repro] [test: crates/backlog/tests/no_confirm_no_unproven.rs::next_claim_skips_pending_row_without_repro]。
- status が `unconfirmed` / `needs-ruling` の行を含む store は `list` / `next` が拒否し、その行を名指しする [test: crates/backlog/tests/no_unconfirmed.rs::list_fails_closed_on_an_unconfirmed_row_in_tasks_toml] [test: crates/backlog/tests/no_unconfirmed.rs::next_fails_closed_on_an_unconfirmed_row_in_tasks_toml] [test: crates/backlog/tests/no_needs_ruling.rs::list_fails_closed_on_a_needs_ruling_row_in_tasks_toml] [test: crates/backlog/tests/no_needs_ruling.rs::next_fails_closed_on_a_needs_ruling_row_in_tasks_toml] [test: crates/backlog/tests/status_type.rs::store_with_unconfirmed_row_fails_naming_id_and_status] [test: crates/backlog/tests/status_type.rs::store_with_needs_ruling_row_fails_naming_id_and_status]。
- `backlog audit-open [--json]` は pending / failed の各行が **記録済みの** 再現テストを持つかを
  `has-test` / `no-test` / `undetermined` で返す [test: crates/backlog/tests/audit_open_once.rs::first_run_classifies_open_rows_and_excludes_terminal]。
- `audit-open` はテストを実行しない [test: crates/backlog/tests/audit_open_once.rs::no_test_is_executed]。
- 判定済みは `~/.backlog/audit-open/<project-slug>.json` に記録され、以後読み飛ばす [test: crates/backlog/tests/audit_open_once.rs::first_run_ledger_records_has_and_no_test_but_not_undetermined] [test: crates/backlog/tests/audit_open_once.rs::row_checked_as_no_test_stays_skipped_after_repro_added]。
- undetermined の行は次回また見る [test: crates/backlog/tests/audit_open_once.rs::second_run_skips_checked_and_reexamines_undetermined]。
- 記録が読めなければ失敗する [test: crates/backlog/tests/audit_open_once.rs::garbage_ledger_is_undetermined_human] [test: crates/backlog/tests/audit_open_once.rs::garbage_ledger_is_undetermined_json]。
- `cancel` は空でない `--reason` が要る [test: crates/backlog/tests/cancel_evidence.rs::cancel_with_empty_or_whitespace_reason_is_refused]。
- `edit --status done|cancelled` は拒否される [test: crates/backlog/tests/closure_evidence.rs::edit_status_done_is_refused] [test: crates/backlog/tests/closure_evidence.rs::edit_status_cancelled_is_refused]。

## 使い分け

| やりたいこと | 使うコマンド |
|---|---|
| キューを確認したい | `backlog list --status pending` |
| 次のアイテムだけ確認したい | `backlog next`（覗くだけ。着手するなら `next --claim`） |
| 手動で完了 / 失敗マークしたい | `backlog done <id> --test ... --red-rev ...`（証拠必須。素の `done` は拒否）/ `backlog fail <id>` |
| やらないと決めて閉じたい | `backlog cancel <id> --reason "<理由>"`（テスト無しで `discard` として記録される [test: crates/backlog/tests/cancel_evidence.rs::cancel_pending_task_records_a_discard_closure_without_tty]） |
| **キューを自動で全件消化したい** | **`/flow` を使う** |

## 失敗モード

- **`backlog` コマンド不在** → README の plugin 導入手順を案内する。
- **他セッションが driver 登録中** → 競合ではない。並走してよい（task 単位の排他は `next --claim` が保証する）。
- **排他ロック競合**（`lock status` の `kind` が `exclusive-lock`） → 誰かが意図的に全セッションを締め出している。
  保有セッションを確認し、必要なら `--force` で奪取する（`/flow` が処理する）。
- **`lock status` が `kind: undetermined` を返す** → backlog が registry を読めなかった状態。
  **「driver 不在」とは読まない**（安全側に倒して見送る）。
