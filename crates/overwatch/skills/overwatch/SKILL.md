---
name: overwatch
description: プロジェクト全体の実行レジストリを監視・制御する。進行中のタスク、リース状態、dedup 状況を表示し、リース取得・解放・一時停止・再割当などの操作を行う。
argument-hint: [status, sessions, pause, resume, reassign, reap]
allowed-tools: Bash(overwatch:*), Read, Write
---

# /overwatch — 実行レジストリの監視・制御

プロジェクト全体の実行状態（進行中のタスク・リース・イベント履歴）を監視し、必要に応じて制御操作を行う。

## 概要

overwatch は、複数セッション間で共有される **プロジェクト全体の実行レジストリ** を管理する。各セッションが同じキーで claim を取得しようとするときに、既に他のセッションが live lease を保持していれば、その事実を報告して（exit 1 + skip JSON）戻る。これにより、同じ作業の重複実行を防ぐ。

## コマンド

### 監視コマンド（読み取り専用）

#### `overwatch status [--json]`

プロジェクト全体の現在の状態を表示する。

- リース中のキー一覧
- リース保有者のセッション名
- ハートビート更新時刻
- イベント履歴（直近 N 件）

`--json` フラグで JSON 出力。

#### `overwatch sessions [--json]`

現在の session レコード一覧。

- セッション ID
- 保持中のリース
- 最終ハートビート
- status

`--json` フラグで JSON 出力。

### 制御コマンド（side-effect あり。必ず AskUserQuestion で確認してから実行すること）

#### `overwatch pause --run <run-id>`

指定した run を一時停止状態にマークする。既存の commit は保持。再開まで待機。

**HOTL gate**: 実行前に AskUserQuestion で確認。

#### `overwatch resume --run <run-id>`

一時停止状態の run を再開する。

**HOTL gate**: 実行前に AskUserQuestion で確認。

#### `overwatch reassign --key <key> --to <session-id>`

既存のリース（`--key <key>`）の保有者を別セッション（`--to <session-id>`）に変更する。

**HOTL gate**: 実行前に AskUserQuestion で確認。

#### `overwatch reap [--ttl-secs <N>]`

ハートビート TTL を超過した dead lease を削除し、キーを解放する。既定 TTL は設定で決まる（通常 5 分）。

**HOTL gate**: 実行前に AskUserQuestion で確認。

#### `overwatch end --key <key>`

指定したキー（`--key <key>`）のリースを明示的に終了・解放する。

**HOTL gate**: 実行前に AskUserQuestion で確認。

## Dedup Contract

任意のセッションが以下を実行するとき：

```bash
overwatch begin --key <k>
```

overwatch は次のいずれかを返す：

1. **success** (exit 0): リース取得成功。キー `<k>` は現在のセッションに排他的に割り当てられた。
2. **skip** (exit 1): そのキー `<k>` についての live lease を別セッションがすでに保持中。呼び出し元は **直ちに処理をスキップする**（重複実行禁止）。JSON の skip reason を確認して、人間に報告する。

caller 側は exit code を必ず確認し、exit 1 なら決して先に進んではならない。

## Liveness Model

各リースは heartbeat TTL で管理される。典型的な TTL は 5 分。

- セッションが begin で lease を取得すると、タイムスタンプが記録される。
- セッションが live（定期的にハートビートを送信）な限り、TTL はリセットされ、リースは有効。
- セッションが crash・timeout・abnormal termination に陥った場合、ハートビート更新が止まり、TTL が切れる。
- TTL 超過リースは `overwatch reap` で自動削除され、キーが再度利用可能になる。

## ファイル構造

すべてのデータは `<base>/<project-key>/overwatch/` に格納される（`<base>` は設定で決定、既定は `~/.local/share/claude-harnesses`）。

```
<base>/<project-key>/overwatch/
  leases.json              # 現在の lease レコード（キー → セッション割当）
  events.jsonl             # イベント履歴（各行 = 1 イベント JSON）
```

- `leases.json`: スナップショット。`{ "key": { "holder": "session-id", "updated_at": "...", ... } }`
- `events.jsonl`: append-only log。新しいレコードは行末に追加される。

## Integration with PDO（Pending Data Object）

overwatch は、プロジェクトの hypothesis (仮説) や awaiting-measurement (測定待ち) の状態を進捗ビューとして集約する。各リースの意味は、「このキーについて、どの仮説が pending か」を encoder する。

これにより、各セッションは、グローバルな progress state を一覧でき、「何が既知・何が pending か」を把握してから開始できる。

## 使い方（例）

```
/overwatch status          # 現在のリース・イベント・セッション状態を確認
/overwatch sessions        # セッション一覧
/overwatch pause --run X   # run X を一時停止（HOTL 確認後）
/overwatch reap            # dead lease を削除（HOTL 確認後）
```

すべての side-effect コマンド（pause / resume / reassign / reap / end）は AskUserQuestion で確認後に実行される。
