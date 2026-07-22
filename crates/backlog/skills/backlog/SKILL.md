---
name: backlog
description: クロスプロジェクト backlog の queue+state を操作するコマンド群へのショートカット。ループ driver は /flow に統合された。
argument-hint: [サブコマンド: list / next / done / fail / lock]
allowed-tools: Bash(backlog:*), Bash(git:*), Read
---

# /backlog — backlog queue+state へのショートカット

`/backlog` は **backlog binary が提供する queue・state 操作**（`list` / `next` / `done` / `fail` / `lock`）を
呼び出すための薄いエントリポイント。

> **ループ driver（lock 取得 → アイテムピック → /condukt → done/fail → lock 解放）は `/flow` に統合されました。**
> バックログのアイテムを順に実装したい場合は **`/flow`** を使ってください。
> `/flow` は compass ゲート・budgetguard・fugu-router によるモデル選択も含む上位互換 driver です。

## backlog binary が提供するコマンド

```bash
backlog list --status pending [--project <path>]   # キュー一覧
backlog next [--project <path>]                    # 次のアイテムをピック
backlog done <id>                                  # アイテムを完了マーク
backlog fail <id> --reason "<概要>"                # アイテムを失敗マーク
backlog lock status [--project <path>]             # ロックステータス確認（project 省略時は全 project 横断で「どこかで driver 稼働中か」を返す）
backlog lock acquire --session-id <id> --project <path>  # ロック取得
backlog lock release --project <path>              # ロック解放
```

> ロックは **project ごとにスコープ**される。無関係な project 同士は同時に `/flow` を走らせても
> 互いをブロックしない。`release` / `acquire` / `heartbeat` は `--project` 必須、`status` だけは
> 省略可（省略時は全 project 横断スキャン）。

## 使い分け

| やりたいこと | 使うコマンド |
|---|---|
| キューを確認したい | `backlog list --status pending` |
| 次のアイテムだけ確認したい | `backlog next` |
| 手動で完了 / 失敗マークしたい | `backlog done <id>` / `backlog fail <id>` |
| **キューを自動で全件消化したい** | **`/flow` を使う** |

## 失敗モード

- **`backlog` コマンド不在** → README の plugin 導入手順を案内する。
- **ロック競合** → `backlog lock status` でロック保有セッションを確認し、必要なら `--force` で奪取する（`/flow` が処理する）。
