# overwatch

Claude Code 向けの、**プロジェクト全体の実行レジストリ**。Rust 製。

複数セッション間での調整が必要な分散システムでは、どのセッションがどのキー（タスク・仮説・リソース）を処理中かを共有する必要がある。overwatch は、プロジェクト全体の **claim registry** を 1 つ管理し、各キーが誰（どのセッション）に claim されているかを記録する。ハートビート TTL に基づいて liveness を判定し、dead lease を検出・削除できる。

新しいセッションが、既に claim されているキーで begin を試みれば、overwatch はそれを report し（exit 1 + skip JSON）、呼び出し元は重複実行をスキップする。

- **SessionStart** フックで、プロジェクト全体の進捗ビュー（`overwatch status`）を注入する。新セッションは、開始時点で「どのセッションが live lease を保持しているか・何が pending か（PDO 仮説・backlog）・どの condukt run があるか・compass gap」を把握できる。
- **Stop** フックで、status ビューをリフレッシュし、人間がレビューできるようにする。
- いずれかのセッションが `overwatch begin --key <k>` を呼べば、原子的にそのキーを claim する。別セッションが既に live lease を保持していれば、リクエストは reject される。
- **Dedup 契約**: exit 1 + skip JSON = 「別セッションが保持中」→ 呼び出し元は **決して先に進んではいけない**。
- Liveness はハートビート TTL で管理。1800 秒（30 分）を超えてハートビート更新が無い lease は stale になる。stale lease は**すべての** lease 変更操作（`begin` / `run` / `end` / `heartbeat` / `reassign` / `reap`）で削除され、`status` / `sessions` / `lease` では無視される。したがって dead な保持者が `begin` を阻むことはない。`overwatch reap` はその掃除だけを単独で行う。

単一 Rust バイナリと 2 つのフック（SessionStart + Stop。どちらも `overwatch status` を hook timeout 5 秒で実行）だけで動き、サブスクリプションネイティブである。追加の API key も要らない。2 つのフックは 10 秒のディスク上 status cache（`aggregate::STATUS_CACHE_TTL_SECS`）を共有する。

lease レジストリのほか、同じバイナリが fleet 単位の gate 違反再発検知・canary rollout の plan/gate ヘルパー・統合人間レビュー窓口（`review-queue` とその ledger 群）も担う。[その他のコマンド](#その他のコマンド) を参照。

### status ビューの `(none)` と `(unknown)`

`overwatch status` の各セクションは、空のとき `(none)` を出す。これは「**見た上で、無かった**」
という観測を意味するので、**実際に観測できた source にしか出さない**。読めなかった source —
壊れた/読めない lease 台帳、不在または非0終了のヘルパーバイナリ、デコードできない JSON — は
`(unknown: <理由>)` を出し、該当 source を列挙した WARNING を stderr に出す。

この区別が最も効くのは **Sessions** ペインである。ここでの `(none)` は「他に live なセッションは
いない」という主張であり、CLAUDE.md §8 が「決して仮定するな」と言っている当の事実であり、
condukt の main-tree guard が main の共有作業ツリーへの commit を許可する前に読む liveness 入力
そのものでもある。したがって `status --json` にも機械可読な `undetermined` 配列
（`[{source, reason}]`）を持たせた。全 source を観測できたときはキーごと省略される。

## 管理対象

プロジェクトごと、`~/.overwatch/<project-key>/overwatch/` に以下を置く：

```
leases.json              # 現在の lease スナップショット: { "<key>": { "key", "title", "session_id", "run_id", "claimed_at", "heartbeat_at", "scope", "done_criteria" } }
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
overwatch begin --key <k> --title <t> [--session <sid>] [--scope <csv>] [--done-criteria <text>]   # キー <k> を排他的に claim する。別セッションが live で保持中、または lease ロックが競合したら exit 1 + skip JSON
overwatch lease --session <sid> [--json]     # そのセッションが保持中の live lease（PDO anchor）を表示。無ければ exit 1（無音）
overwatch run --key <k> [--note <text>]      # 保持中のリースにハートビート＋イベントを記録（下の表を参照）
overwatch status [--json]                    # プロジェクト全体の進捗ビュー: Sessions / PDO hypotheses / Backlog / Condukt runs / Compass gap
overwatch sessions [--json]                  # Sessions ロスターのみ: live（stale でない）lease を保持するセッション
overwatch pause --run <id>                   # `condukt state pause --run <id>` に委譲し、control イベントを記録
overwatch resume --run <id>                  # `condukt state resume --run <id>` に委譲し、control イベントを記録
overwatch reassign --key <k> --to <sid>      # <k> の lease を「解放」し reassign to=<sid> イベントを記録（<sid> 側に lease は作らない）
overwatch end --key <k> --status <s>         # キーの lease を解放し、終端 status を記録
overwatch reap                               # stale lease（TTL 内にハートビート無し）を削除。{"reaped": N} を出力
overwatch heartbeat --key <k>                # キーの heartbeat_at を更新。{"refreshed": 0|1} を出力
overwatch --protects                         # このゲートが何を・何から・どんな根拠で守るかを表示
```

各変更操作が仕事を**しなかった**ときの報告（`src/lease.rs` / `src/control.rs` より）:

| コマンド | lease ロック競合 | キー未保持 | 下流の失敗 |
|---|---|---|---|
| `begin` | exit 1、`{"skipped": true, "reason": "lock_contended"}` | — | — |
| `end` | **非 0**、lease は解放されない | **非 0**（nothing was released） | — |
| `run` | **exit 0、何も書かない**（無言スキップ） | exit 0、導出 ID でイベントは記録 | — |
| `heartbeat` | exit 0、`{"refreshed": 0}` | exit 0、`{"refreshed": 0}` | — |
| `reap` | exit 0、`{"reaped": 0}`（stale なしと区別不能） | — | — |
| `reassign` | exit 0、`control_note` JSON、何も解放しない | exit 0、`control_note`、意図は記録 | — |
| `pause` / `resume` | — | — | `condukt` 不在・非 0 終了でも exit 0（`control_note` JSON を出すだけ） |

exit code で失敗を伝えるのは `begin` と `end` だけである。他は stdout を読む必要がある。

バイナリ自身は `pause` / `resume` / `reassign` / `end` / `reap` の前に承認を求め**ない**。
「HOTL gate」とは `/overwatch` スキルが実行前に AskUserQuestion で確認するよう指示されている
という意味であり（下記）、バイナリを直接呼べば即座に実行される。

## Dedup 契約

```bash
overwatch begin --key "hypothesis-v2.3"
```

戻り値：

- **exit 0**: Lease 取得成功（同じセッションが既に保持していた場合は refresh。`claimed_at` は維持）。先に進んでよい。
- **exit 1** + JSON `{ "skipped": true, "holder": { "session_id", "run_id", "heartbeat_at" } }`: このキーについて、別の live セッションが lease を保持中。**決して先に進んではいけない**。
- **exit 1** + JSON `{ "skipped": true, "reason": "lock_contended" }`: lease レジストリのロックが取れず、キーが空いているかを確認していない。**先に進んではいけない**（後で再試行）。

呼び出し元の責務は、exit code を確認し、非 0 なら処理をスキップすることである。内部エラー
（store root を解決できない、`leases.json` が読めない/壊れている）も exit **1** になる（`main` が
`anyhow::Result` を返すため。エラーは stderr に出て、stdout に skip JSON は出ない）。いずれにせよ
キーは取得されていない。

## PDO セッションアンカー（`--scope` / `--done-criteria`）

lease は、そのセッションが「何を担当していて」「何をもって完了とするか」を記録する 2 つの
任意 PDO-anchor フィールドを持てる。いずれも `#[serde(default)]` なので、これらが無かった頃に
書かれた既存の `leases.json` もそのまま読める（後方互換）：

- `scope: Vec<String>` — このセッションが担当するファイル/glob（condukt の `touched_files` と
  同じ語彙）。空 = まだ確定していない（調査中など）を意味し、false positive を避けるため
  overlap 検知の対象から除外される。
- `done_criteria: Option<String>` — このセッションの「完了」の定義。

`overwatch begin` に、これらを設定する 2 つの任意フラグを追加した。両方を省略すれば従来の挙動が
そのまま維持される（exit code 契約も不変）：

```bash
overwatch begin --key "hypothesis-v2.3" --title "cache the router table" \
  --scope "crates/fugu-router/src/**,crates/condukt/src/schedule.rs" \
  --done-criteria "fugu-router のテスト全 green・clippy 警告なし"
```

`begin` 成功時（exit 0）、advisory な JSON サマリを stdout に出力する：

```json
{ "scope_overlap": [ { "key": "...", "title": "...", "scope": ["..."] } ],
  "scope_undeclared": [ { "key": "...", "title": "..." } ],
  "scope_declared": true,
  "possible_duplicate": [ { "key": "...", "title": "...", "similarity": 0.72 } ] }
```

- `scope_overlap` は、この lease と scope が重なる別 key の live lease を、粗い glob-prefix
  マッチで列挙する。**blocking しない早期警告**であり——scope の重なりは必ずしも衝突ではない
  ——厳密な判定は condukt の Phase 3.5 `conflict-check` に委ねる。
- `scope_undeclared` は、この lease か相手のどちらかが scope を宣言していないため重なりを
  **判定できなかった** live lease を列挙する。未宣言は「重なりなし」ではない — どちらの
  リストにも出ない peer は、scope を宣言したうえで重なっていないものだけである。
- `scope_declared` は、この lease 自身が scope を宣言していないとき `false` になり、その場合は
  すべての peer が `scope_undeclared` に入る（backlog 56eb3f9a）。
- `possible_duplicate` は、title/done_criteria が語彙的に類似する（Jaccard ≥ 閾値、既定 0.6、
  環境変数 `OVERWATCH_DUP_THRESHOLD` で上書き可）別の live lease を、
  `harness_core::lessons::text_similarity` を再利用して列挙する。
- いずれのリストも既定は空配列で、**exit code は決して変えない**。

### セッションのアンカーを読む

```bash
overwatch lease --session <sid> [--json]
```

`<sid>` が保持中の live lease（PDO anchor）を——複数保持していれば最も直近に claim したものを
——表示する。無ければ exit 1 で無音終了する。これは ctxrot の anchor 再注入（セッションの
title / done_criteria を context に再注入する読み取り経路。`docs/DESIGN-pdo-session-anchor.md`
§4.3 参照）のための read path である。

## `/overwatch` で制御する

いつでも `/overwatch` を実行すると：

- プロジェクト全体のレジストリ状態を表示（`status`）
- セッション一覧を表示（`sessions`）
- 進行中の run を制御（pause/resume、再割当、reap）

スキルは、すべての side-effect コマンド（pause/resume/reassign/end/reap）を実行前に AskUserQuestion で確認するよう指示している。このゲートはスキルの散文にのみ存在し、バイナリは強制しない。

## Liveness と TTL

各 lease は `heartbeat_at`（および `claimed_at`）タイムスタンプを持つ。staleness の判定は
**固定**のハートビート TTL **1800 秒（30 分）**（`store::LEASE_TTL_SECS`）に対して行う。
TTL はコンパイル時定数であり、設定変更はできない。

- セッションが `overwatch begin` を呼ぶ → `claimed_at`／`heartbeat_at` が記録され、lease が取得される。
- セッションが活動中（定期的に `overwatch heartbeat --key <k>` を呼ぶ）→ `heartbeat_at` が更新され、lease は有効。
- セッションが crash・hang・異常終了に陥る → ハートビート更新が止まり、`heartbeat_at` が陳腐化。
- `now - heartbeat_at > 1800s` になると lease は stale になる。次の lease 変更操作（`begin` / `run` / `end` / `heartbeat` / `reassign` / `reap` のいずれか）が `leases.json` から削除し、別セッションによるそのキーの `begin` は成功する。`overwatch reap` はその掃除だけを行う。

## ストレージ

状態はリポジトリごとに `~/.overwatch/<project-key>/overwatch/`（`leases.json` +
`events.jsonl`）に保存される。キーは MAIN worktree root から導出するため、main
checkout・全 linked `git worktree`・それらの subdirectory は同一の store を共有する
（main checkout から `overwatch status` を実行すると linked worktree 側のセッションも見える）。
main worktree root を解決できない場合（`.git` ファイルが読めない/解釈できない、submodule の
worktree など）は worktree 単位の store へ fallback せずコマンドがエラーになる。git repo 外の
cwd は cwd 自身がキーになる。設定ファイルや TTL の調整ノブは存在せず、上記の挙動が
現在バイナリが実装している唯一のものである。

## ストレージ構成

すべてのデータはリポジトリの外、ユーザーのホームディレクトリ配下に置かれる：

```
<base>/<project-key>/overwatch/
  leases.json          # 有効な lease のスナップショット
  events.jsonl         # append-only レジストリ
  ...                  # 同じディレクトリの他の ledger（violations・review findings・
                       # audit rounds・status cache など。src/store.rs の *_path 関数を参照）
```

- `<base>`: `~/.overwatch`（`$HOME/.overwatch`。`src/store.rs` の `storage_root` が呼ぶ
  `harness_core::config::base_dir("overwatch")`）。設定では変えられない — 設定ファイルは存在しない（上の「ストレージ」参照）。
- `<project-key>`: リポジトリの MAIN worktree root の `<basename>-<正規化パスの fnv1a32>`（linked worktree 間で共有）。
- lease は `end` されるか、`reassign` で解放されるか、後続の lease 変更操作で stale として削除されるまで `leases.json` に残る。overwatch は lease を Claude Code プロセスに紐づけておらず、セッション終了時に lease を消す overwatch フックも無い（`/flow` などの呼び出し元が `overwatch end` で自分の lease を解放する）。

## fleet 単位の相関エラー検知（gate 違反 signature の再発検知）

`overwatch` は project-wide の実行/lease レジストリに加えて、gate 違反イベントを **正規化した
signature** 付きで記録し、同一 signature が複数タスク/セッションにまたがって時間窓内に N 件以上
再発したら **systemic issue** としてエスカレーションする。

```sh
overwatch record-violation --source <gate> --discriminator <d> --task <key> [--symbol <sym>] [--session <sid>] [--detail <text>]
overwatch violations [--json] [--threshold N] [--window-secs S]     # 全 signature の再発状況
overwatch escalations [--json] [--threshold N] [--window-secs S]    # systemic と判定された signature のみ
```

- `--source` は `blastguard | propguard | specguard | mutategate | donegate | reviewgate | tdd | budgetguard | autoflow | ctxrot` のいずれか。それ以外はエラー。
- **signature 正規化**（`violation::normalize_signature`）は純粋関数。同種の失敗（例:
  blastguard の同一 rule、propguard の同一 PROP、mutategate の同一 mutation operator、specguard の
  同一 drift kind + symbol）は、大文字小文字・空白の違いを問わず同じ signature になる。
- **再発検知**（`violation::detect_recurrence`）も時刻を引数で受け取る純粋関数（`Date.now()` を
  内部で読まない）。既定ポリシーは 24 時間窓・3 件以上（`RecurrencePolicy::default`）。CLI では
  `--threshold` / `--window-secs` で呼び出しごとに上書きできる。
- **systemic 判定**は「閾値以上」かつ「複数タスク or 複数セッションにまたがる」の両方を要求する。
  単一タスクが同じゲートに何度もリトライして失敗するのは systemic ではなく local retry loop として
  区別する。
- 保存先は既存の project-wide registry と同じ `<base>/<project-key>/overwatch/` 配下の
  `violations.jsonl`（events.jsonl とは別ストリーム、append-only）。

## その他のコマンド

`overwatch --help` が全サブコマンドを説明付きで列挙する。以下はその地図である。

| 分類 | コマンド |
|---|---|
| canary rollout | `canary-plan`（純粋な stage 分割）、`canary-gate`（PROCEED/ROLLBACK の JSON。rollback 推奨時は **exit 3**）、`canary-rollback-plan`（純粋な復元計画）、`record-rollback`（`scripts/rollout-plugins.sh` が呼ぶ。store 書き込みエラーは stderr に警告し、呼び出し元を失敗させない） |
| review 窓口 | `review-queue [--json] [--since] [--limit] [--to-backlog]`（6 source — systemic 違反・canary rollback・AI review finding・condukt escalation・未解決の merge conflict・`needs-ruling` の backlog task — を severity 順の 1 リストに統合。`--to-backlog` は表示専用の needs-ruling 以外の全 stream を backlog へ流す）、`record-finding`、`record-disposition`、`review-metrics`、`compact-findings`、`resolve-merge-conflict`、`reconcile-fixed [--since-ref\|--range\|--last-n] [--dry-run]`（既定は直近 50 commit）、`auto-approved [--since] [--sample 5] [--seed 0]` |
| Continuous-Audit | `audit-round record` / `audit-round close`、`audit-metrics [--window 3]` |
| テレメトリ | `undetermined-metrics [--window-days N]`、`gate-outcomes`（donegate / tdd / reviewgate の発火結果を `~/.<gate>/state/log.jsonl` から集計） |

読み取り側の exit code 規約: ledger/journal が（不在・空ではなく）**読めない**とき、
`review-queue`（`--to-backlog` を含む）・`review-metrics`・`reconcile-fixed`・`auto-approved`・
`gate-outcomes` は 0 件/空の答えを出す代わりに **exit 3** し、判定不能だった source を明示する。
`audit-metrics` と `undetermined-metrics` は 0 件レポートではなくエラー（非 0）を返す。
`reconcile-fixed` が fail-soft なのは **git** 側だけで、`git log` の失敗は「0 commit scanned」
（exit 0）として読まれる。`review-queue --to-backlog` は backlog 側で fail-soft
（`backlog` バイナリ不在や `backlog add` 失敗は警告してスキップ）。
finding の `#[ignore = "<finding-id>: ..."]` 回帰テストは `--repro-test` として `backlog add` に渡され、backlog はそのテストが再現した時だけ task を起こす [test: crates/overwatch/tests/bridge_repro_evidence.rs::confirmed_finding_with_failing_ignored_repro_test_is_filed_with_repro_test]。
拒否された add（exit 2、backlog の unproven 記録に残る）とそれ以外の失敗は、JSON summary の `unproven` と `failed` に数えられる [test: crates/overwatch/tests/bridge_repro_evidence.rs::refused_backlog_add_is_distinguishable_from_success]。

## PDO（Pending Data Object）との関連

overwatch は、pending な仮説と awaiting-measurement な状態を集約し、**progress view** として公開する（`status` 出力）。各 lease キーは、pending な特定の作業（仮説版、設計変種、測定対象）をエンコードする。レジストリを照会することで、セッションは「前セッションが未完了のまま残した作業は何か」を学習し、それを引き継ぐか、再割当するか、reap するかを判断できる。

これは **PDO** パターンに合致する：仮説は pending、測定は awaiting、overwatch は aggregator。

## License

MIT
