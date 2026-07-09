# overwatch

Claude Code 向けの、**プロジェクト全体の実行レジストリ**。Rust 製。

複数セッション間での調整が必要な分散システムでは、どのセッションがどのキー（タスク・仮説・リソース）を処理中かを共有する必要がある。overwatch は、プロジェクト全体の **claim registry** を 1 つ管理し、各キーが誰（どのセッション）に claim されているかを記録する。ハートビート TTL に基づいて liveness を判定し、dead lease を検出・削除できる。

新しいセッションが、既に claim されているキーで begin を試みれば、overwatch はそれを report し（exit 1 + skip JSON）、呼び出し元は重複実行をスキップする。

- **SessionStart** フックで、プロジェクト全体の進捗ビュー（`overwatch status`）を注入する。新セッションは、開始時点で「何が進行中か・何が pending か・何が reaped されたか」を把握できる。
- **Stop** フックで、status ビューをリフレッシュし、人間がレビューできるようにする。
- いずれかのセッションが `overwatch begin --key <k>` を呼べば、原子的にそのキーを claim する。別セッションが既に live lease を保持していれば、リクエストは reject される。
- **Dedup 契約**: exit 1 + skip JSON = 「別セッションが保持中」→ 呼び出し元は **決して先に進んではいけない**。
- Liveness はハートビート TTL で管理。dead lease（1800 秒 / 30 分ハートビート更新なし）は `overwatch reap` で手動 reap する（HOTL gate）。自動 expire はしない。

単一 Rust バイナリと 2 つのフック（SessionStart + Stop）だけで動き、サブスクリプションネイティブである。追加の API key も要らない。

## 管理対象

プロジェクトごと、`~/.overwatch/<project-key>/overwatch/` に以下を置く：

```
leases.json              # 現在の lease スナップショット: { "<key>": { "key", "title", "session_id", "run_id", "claimed_at", "heartbeat_at" } }
events.jsonl             # append-only イベント ログ
```

## インストール（プラグイン）

```
/plugin install overwatch@yukineko
```

## 手動インストール

```sh
cargo install --path .
```

`overwatch install` というステップは存在しない。プラグインは同梱バイナリを配布し、
レジストリ／ストアは初回利用時に遅延生成される。インストールは単なるコピーであり、
実行すべきセットアップコマンドはない。

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

各 lease は `heartbeat_at`（および `claimed_at`）タイムスタンプを持つ。staleness の判定は
**固定**のハートビート TTL **1800 秒（30 分）**（`store::LEASE_TTL_SECS`）に対して行う。
TTL はコンパイル時定数であり、設定変更はできない。

- セッションが `overwatch begin` を呼ぶ → `claimed_at`／`heartbeat_at` が記録され、lease が取得される。
- セッションが活動中（定期的に `overwatch heartbeat --key <k>` を呼ぶ）→ `heartbeat_at` が更新され、lease は有効。
- セッションが crash・hang・異常終了に陥る → ハートビート更新が止まり、`heartbeat_at` が陳腐化。
- `now - heartbeat_at > 1800s` になると、`overwatch reap` で dead lease を削除でき、キーが再利用可能になる。

## ストレージ

状態はリポジトリごとに `~/.overwatch/<project-key>/overwatch/`（`leases.json` +
`events.jsonl`）に保存される。設定ファイルや TTL の調整ノブは存在せず、上記の挙動が
現在バイナリが実装している唯一のものである。

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

## fleet 単位の相関エラー検知（gate 違反 signature の再発検知）

`overwatch` は project-wide の実行/lease レジストリに加えて、gate 違反イベント（blastguard denial・
propguard PROP failure・specguard drift finding・mutategate kill failure）を **正規化した
signature** 付きで記録し、同一 signature が複数タスク/セッションにまたがって時間窓内に N 件以上
再発したら **systemic issue** としてエスカレーションする。

```sh
overwatch record-violation --source blastguard --discriminator rm-rf --task <key> [--symbol <sym>] [--detail <text>]
overwatch violations [--json] [--threshold N] [--window-secs S]     # 全 signature の再発状況
overwatch escalations [--json] [--threshold N] [--window-secs S]    # systemic と判定された signature のみ
```

- **signature 正規化**（`violation::normalize_signature`）は純粋関数。同種の失敗（例:
  blastguard の同一 rule、propguard の同一 PROP、mutategate の同一 mutation operator、specguard の
  同一 drift kind + symbol）は、大文字小文字・空白の違いを問わず同じ signature になる。
- **再発検知**（`violation::detect_recurrence`）も時刻を引数で受け取る純粋関数（`Date.now()` を
  内部で読まない）。既定ポリシーは 24 時間窓・3 件以上。`RecurrencePolicy { threshold, window_secs }`
  で両方とも設定可能。
- **systemic 判定**は「閾値以上」かつ「複数タスク or 複数セッションにまたがる」の両方を要求する。
  単一タスクが同じゲートに何度もリトライして失敗するのは systemic ではなく local retry loop として
  区別する。
- 保存先は既存の project-wide registry と同じ `<base>/<project-key>/overwatch/` 配下の
  `violations.jsonl`（events.jsonl とは別ストリーム、append-only）。

## PDO（Pending Data Object）との関連

overwatch は、pending な仮説と awaiting-measurement な状態を集約し、**progress view** として公開する（`status` 出力）。各 lease キーは、pending な特定の作業（仮説版、設計変種、測定対象）をエンコードする。レジストリを照会することで、セッションは「前セッションが未完了のまま残した作業は何か」を学習し、それを引き継ぐか、再割当するか、reap するかを判断できる。

これは **PDO** パターンに合致する：仮説は pending、測定は awaiting、overwatch は aggregator。

## License

MIT
