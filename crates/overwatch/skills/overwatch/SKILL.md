---
name: overwatch
description: プロジェクト全体の実行レジストリを監視・制御したいときに使うこと。
argument-hint: [status, sessions, pause, resume, reassign, reap]
allowed-tools: Bash(overwatch:*), Read, Write
---

caller 側は exit code を必ず確認し、非 0 なら決して先に進んではならない。

制御コマンド（pause / resume / reassign / reap / end）は AskUserQuestion で確認せずに実行しない。

### 制御コマンド（side-effect あり。必ず AskUserQuestion で確認してから実行すること）

#### `overwatch pause --run <run-id>`

指定した run の一時停止を condukt に委譲する（condukt が失敗したら非 0 で終了する）。

#### `overwatch resume --run <run-id>`

一時停止状態の run の再開を condukt に委譲する（condukt が失敗したら非 0 で終了する）。

#### `overwatch reassign --key <key> --to <session-id>`

既存のリース（`--key <key>`）を解放し（key が未保持・ロック不成立・lease root 解決不能のときは何も記録せず非 0 で終了する）、`reassign to=<session-id>` イベントを記録する（`--to` 側に lease は作らない）。

#### `overwatch reap`

ハートビート TTL を超過した dead lease を削除し、キーを解放する。

#### `overwatch end --key <key> --status <done|abandoned>`

解放できたときだけ exit 0 + `released lease <key>`。
`--status` は `done` か `abandoned` のみ。それ以外は非 0 で拒否され lease は保持されたまま。key に lease が無い（未 begin・解放済み・stale reap 済み）ときと、lease ロックが取れず判定不能のときは非 0（いずれも「解放した」ではない）。


```bash
overwatch begin --key <k>
```

1. **success** (exit 0): リース取得成功。
2. **skip** (exit 1 + `holder` を含む skip JSON): そのキー `<k>` についての live lease を別セッションがすでに保持中。

非 0 のときは stdout の skip JSON（無ければ stderr のエラー）を確認して、人間に報告すること。


#### `overwatch status [--json]`

プロジェクト全体の現在の状態を表示する。

`--json` フラグで JSON 出力。

#### `overwatch sessions [--json]`

live（stale でない）lease を保持するセッションの一覧。


すべてのデータは `<base>/<project-key>/overwatch/` に格納される（`<base>` は `~/.overwatch` 固定）。

`<project-key>` はリポジトリの MAIN worktree root から導出されるので、main checkout と linked worktree は同じ store を共有する。

```
<base>/<project-key>/overwatch/
  leases.json
  events.jsonl
```

`events.jsonl`: append-only log。

```
/overwatch status
/overwatch sessions
/overwatch pause --run X
/overwatch reap
```
