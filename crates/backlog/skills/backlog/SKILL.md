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
backlog done <id>                                  # アイテムを完了マーク
backlog fail <id> --reason "<概要>"                # アイテムを失敗マーク

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

## 使い分け

| やりたいこと | 使うコマンド |
|---|---|
| キューを確認したい | `backlog list --status pending` |
| 次のアイテムだけ確認したい | `backlog next`（覗くだけ。着手するなら `next --claim`） |
| 手動で完了 / 失敗マークしたい | `backlog done <id>` / `backlog fail <id>` |
| **キューを自動で全件消化したい** | **`/flow` を使う** |

## 失敗モード

- **`backlog` コマンド不在** → README の plugin 導入手順を案内する。
- **他セッションが driver 登録中** → 競合ではない。並走してよい（task 単位の排他は `next --claim` が保証する）。
- **排他ロック競合**（`lock status` の `kind` が `exclusive-lock`） → 誰かが意図的に全セッションを締め出している。
  保有セッションを確認し、必要なら `--force` で奪取する（`/flow` が処理する）。
- **`lock status` が `kind: undetermined` を返す** → backlog が registry を読めなかった状態。
  **「driver 不在」とは読まない**（安全側に倒して見送る）。
