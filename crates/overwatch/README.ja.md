# overwatch

Claude Code 向けの、**プロジェクト全体の実行レジストリ**。Rust 製。

複数セッション間での調整が必要な分散システムでは、どのセッションがどのキー（タスク・仮説・リソース）を処理中かを共有する必要がある。overwatch は、プロジェクト全体の **claim registry** を 1 つ管理し、各キーが誰（どのセッション）に claim されているかを記録する。ハートビート TTL に基づいて liveness を判定し、dead lease を検出・削除できる。

新しいセッションが、既に claim されているキーで begin を試みれば、overwatch はそれを report し（exit 1 + skip JSON）、呼び出し元は重複実行をスキップする。

- **SessionStart** フックで、プロジェクト全体の進捗ビュー（`overwatch status`）を注入する。新セッションは、開始時点で「何が進行中か・何が pending か・何が reaped されたか」を把握できる。
- **Stop** フックで、status ビューをリフレッシュし、人間がレビューできるようにする。
- いずれかのセッションが `overwatch begin --key <k>` を呼べば、原子的にそのキーを claim する。別セッションが既に live lease を保持していれば、リクエストは reject される。
- **Dedup 契約**: exit 1 + skip JSON = 「別セッションが保持中」→ 呼び出し元は **決して先に進んではいけない**。
- Liveness はハートビート TTL で管理。dead lease（N 秒間ハートビート更新なし）は手動で reap するか自動 expire できる。

単一 Rust バイナリと 2 つのフック（SessionStart + Stop）だけで動き、サブスクリプションネイティブである。追加の API key も要らない。

## 管理対象

プロジェクトごと、`<base>/<project-key>/overwatch/` に以下を置く：

```
leases.json              # 現在の lease スナップショット: { "key": { "holder": "session-id", "updated_at": "...", ... } }
events.jsonl             # append-only イベント ログ
```

## インストール（プラグイン）

```
/plugin install overwatch@yukineko
```

## 手動インストール

```sh
cargo install --path .
overwatch install
```

## コマンド

```sh
overwatch begin --key <k> --title <t> [--session <sid>]   # キー <k> を排他的に claim する。別セッションが live で保持していれば exit 1 + skip JSON
overwatch run --key <k> [--note <text>]      # 保持中のリースにハートビート＋イベントを記録（キー未保持でも fail-soft）
overwatch status [--json]                    # プロジェクト全体の進捗: 有効な lease・イベント・セッション
overwatch sessions [--json]                  # セッション一覧（live または dead）
overwatch pause --run <id>                   # run を一時停止（HOTL gate）
overwatch resume --run <id>                  # 一時停止状態の run を再開（HOTL gate）
overwatch reassign --key <k> --to <sid>    # lease の保有者を別セッションに再割当（HOTL gate）
overwatch end --key <k> --status <s>         # キーの lease を解放し、終端 status を記録（HOTL gate）
overwatch reap                               # dead lease（TTL 超過）を削除（HOTL gate）
overwatch heartbeat --key <k>                # lease の TTL をリセット（lease を keep-alive）
```

## Dedup 契約

```bash
overwatch begin --key "hypothesis-v2.3"
```

戻り値：

- **exit 0**: Lease 取得成功。先に進んでよい。
- **exit 1** + JSON `{ "skip": "reason", ... }`: このキーについて、別の live セッションが lease を保持中。**決して先に進んではいけない**。

呼び出し元の責務は、exit code を確認し、1 なら処理をスキップすることである。

## `/overwatch` で制御する

いつでも `/overwatch` を実行すると：

- プロジェクト全体のレジストリ状態を表示（`status`）
- セッション一覧を表示（`sessions`）
- 進行中の run を制御（pause/resume、再割当、reap）

すべての side-effect コマンド（pause/resume/reassign/end/reap）は、実行前に AskUserQuestion で確認される。

## Liveness と TTL

各 lease は `updated_at` タイムスタンプと設定可能なハートビート TTL（既定: 5 分）を持つ。

- セッションが `overwatch begin` を呼ぶ → タイムスタンプが記録され、lease が取得される。
- セッションが活動中（定期的に `overwatch heartbeat --key <k>` を呼ぶ）→ TTL がリセットされ、lease は有効。
- セッションが crash・hang・異常終了に陥る → ハートビート更新が止まり、タイムスタンプが陳腐化。
- TTL が切れると、`overwatch reap` で dead lease を削除でき、キーが再利用可能になる。

## 設定（`overwatch.toml`）

```toml
enabled = true
# base = "~/.local/share/claude-harnesses"  # 既定: ~/.local/share/claude-harnesses
ttl_secs = 300                              # ハートビート TTL（既定: 5 分）
```

セッション単位で無効化したいときは `OVERWATCH_DISABLED=1` を指定する。

## ストレージ構成

すべてのデータは version-controlled な場所またはユーザー所有キャッシュに置かれる：

```
<base>/<project-key>/overwatch/
  leases.json          # 有効な lease のスナップショット
  events.jsonl         # append-only レジストリ
```

- `<base>`: 既定は `~/.local/share/claude-harnesses`（overwatch.toml で設定可能）。
- `<project-key>`: プロジェクト名由来（例: `claude-harnesses`）。
- Lease は full reaper を超えて persist **しない**。セッション有効期間の ephemeral レコード。

## PDO（Pending Data Object）との関連

overwatch は、pending な仮説と awaiting-measurement な状態を集約し、**progress view** として公開する（`status` 出力）。各 lease キーは、pending な特定の作業（仮説版、設計変種、測定対象）をエンコードする。レジストリを照会することで、セッションは「前セッションが未完了のまま残した作業は何か」を学習し、それを引き継ぐか、再割当するか、reap するかを判断できる。

これは **PDO** パターンに合致する：仮説は pending、測定は awaiting、overwatch は aggregator。

## License

MIT
