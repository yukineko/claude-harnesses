変更系コマンド（begin / run / end / heartbeat / reap / reassign / pause / resume）は、操作が行われなかったとき非 0 で終了する（lease ロック競合・lease root 解決不能・キー未保持〈end, reassign〉・`end --status` の未知値・condukt 失敗〈pause, resume〉）。`heartbeat` / `reap` の exit 0 は呼び出しが走ったことしか意味しないので、出力の `refreshed` / `reaped` の件数で実際の変化を確認すること。

begin が exit 1 + skip JSON を返したら、決して先に進んではいけない。

- いずれかのセッションが `overwatch begin --key <k>` を呼べば、原子的にそのキーを claim する。別セッションが既に live lease を保持していれば、リクエストは reject される。
- 新しいセッションが、既に claim されているキーで begin を試みれば、overwatch はそれを report し（exit 1 + skip JSON）、呼び出し元は重複実行をスキップする。
- **SessionStart** フックで、プロジェクト全体の進捗ビュー（`overwatch status`）を注入し、新セッションは、開始時点で「どのセッションが live lease を保持しているか・何が pending か（PDO 仮説・backlog）・どの condukt run があるか・compass gap」を把握できる。
- 1800 秒（30 分）を超えてハートビート更新が無い lease は stale になる。
- stale lease は**すべての** lease 変更操作（`begin` / `run` / `end` / `heartbeat` / `reassign` / `reap`）で削除され、`status` / `sessions` / `lease` では無視される。したがって dead な保持者が `begin` を阻むことはない。変更操作による削除は 1 件ずつ status `reaped` の `ended` イベントとして `events.jsonl` に追記される。
- `overwatch reap` はその掃除だけを単独で行う。

2 つのフック（SessionStart + Stop、どちらも `overwatch status` を hook timeout 5 秒で実行）がある。キャッシュされた status は、`aggregate::STATUS_CACHE_TTL_SECS` を過ぎたとき、cache 構築と同じ秒以降に `leases.json` が書かれたとき、またはその mtime を観測できないときに破棄される（`aggregate::cache_is_fresh`）。

`overwatch status` の各セクションは、空のとき `(none)` を出す。
`(none)` は**実際に観測できた source にしか出さない**。
読めなかった source — 壊れた/読めない lease 台帳、不在のヘルパーバイナリ、デコードできない JSON — は `(unknown: <理由>)` を出す。
該当 source を列挙した WARNING を stderr に出す。

`status --json` にも機械可読な `undetermined` 配列（`[{source, reason}]`）がある。全 source を観測できたときはキーごと省略される。

```
/plugin install overwatch@yukineko
```

```sh
cargo install --path .
```

`overwatch install` というサブコマンドは存在しない。

```sh
overwatch begin --key <k> --title <t> [--session <sid>] [--scope <csv>] [--done-criteria <text>]   # キー <k> を排他的に claim する、別セッションが live で保持中、または lease ロックが競合したら exit 1 + skip JSON
overwatch lease --session <sid> [--json]     # そのセッションが保持中の live lease（PDO anchor）を表示し、無ければ exit 1（無音）
overwatch run --key <k> [--note <text>]      # 保持中のリースにハートビート＋イベントを記録
overwatch status [--json]                    # プロジェクト全体の進捗ビュー: Sessions / PDO hypotheses / Backlog / Condukt runs / Compass gap
overwatch sessions [--json]                  # Sessions ロスターのみ: live（stale でない）lease を保持するセッション
overwatch pause --run <id>                   # `condukt state pause --run <id>` に委譲し、control イベントを記録
overwatch resume --run <id>                  # `condukt state resume --run <id>` に委譲し、control イベントを記録
overwatch reassign --key <k> --to <sid>      # <k> の lease を「解放」し reassign to=<sid> イベントを記録（<sid> 側に lease は作らない）
overwatch end --key <k> --status <done|abandoned>   # キーの lease を解放し、終端 status を記録（それ以外の status は非 0 で拒否され、lease は保持されたまま）
overwatch reap                               # stale lease（TTL 内にハートビート無し）を削除し {"reaped": N} を出力
overwatch heartbeat --key <k>                # キーと、同じセッションが保持する他の全 lease の heartbeat_at を更新し {"refreshed": N} を出力（キー未保持なら 0）
overwatch --protects                         # このゲートが何を・何から・どんな根拠で守るかを表示
```

`begin`/`run` のセッション ID は `--session`、無ければ `$CLAUDE_CODE_SESSION_ID`、それも無ければ `pid-<pid>` で決まる。

run ID は `$OVERWATCH_RUN_ID`、無ければタイムスタンプ由来の `run-…` になる。

`end` は lease ロックが競合したとき、非 0 で終了し、lease は解放されない。

`run` / `heartbeat` / `reap` / `reassign` / `end` は、lease ロックが競合したとき、または lease root を解決できないとき非 0 で終了する（何も書かない。`end` は加えて lease を解放しない）。未保持キーの `reassign`、condukt が失敗した `pause` / `resume` も非 0 で終了する。

`end` はキーが未保持のとき、非 0（nothing was released）で終了する。

バイナリ自身は `pause` / `resume` / `reassign` / `end` / `reap` の前に承認を求め**ない**。

バイナリを直接呼べば即座に実行される。

begin が非 0 を返したら、先に進んではいけない。

```bash
overwatch begin --key "hypothesis-v2.3"
```

- **exit 0**: Lease 取得成功（同じセッションが既に保持していた場合は refresh、`claimed_at` は維持）。
- **exit 1** + JSON `{ "skipped": true, "holder": { "session_id", "run_id", "heartbeat_at" } }`: このキーについて、別の live セッションが lease を保持中。**決して先に進んではいけない**。
- **exit 1** + JSON `{ "skipped": true, "reason": "lock_contended" }`: 待機中ずっと live な相手が lease レジストリのロックを保持しており、キーが空いているかを確認していない。後で再試行すること。**先に進んではいけない**。ロックの I/O 失敗（ロックディレクトリを作成・書き込みできない）は `lock_contended` としては報告せず、stderr に undetermined エラーを出して非 0 で終了する。

exit code を確認し、非 0 なら処理をスキップすること。

`leases.json` が壊れているときも exit **1** になり、エラーは stderr に出て、stdout に skip JSON は出ない。

store root を解決できないときも exit **1** になり、stderr は root が undetermined であることを示す。

lease は 2 つの任意 PDO-anchor フィールド、`scope` と `done_criteria` を持てる。

いずれも `#[serde(default)]` なので、これらが無かった頃に書かれた既存の `leases.json` もそのまま読める（後方互換）：

- `scope: Vec<String>` — このセッションが担当するファイル/glob。
- `done_criteria: Option<String>` — このセッションの「完了」の定義。

`overwatch begin` は、これらを設定する 2 つの任意フラグ `--scope` と `--done-criteria` を持つ：

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

- `scope_overlap` は、この lease と scope が重なる別 key の live lease を列挙する。
- `scope_undeclared` は、この lease か相手のどちらかが scope を宣言していないため重なりを**判定できなかった** live lease を列挙する。
- 未宣言は「重なりなし」ではない — どちらのリストにも出ない peer は、scope を宣言したうえで重なっていないものだけである。
- `scope_declared` は、この lease 自身が scope を宣言していないとき `false` になり、その場合はすべての peer が `scope_undeclared` に入る（backlog 56eb3f9a）。
- `possible_duplicate` は、title が語彙的に類似する（Jaccard 類似度、既定の閾値 0.6、環境変数 `OVERWATCH_DUP_THRESHOLD` で上書き可）別の live lease を列挙する。
- いずれのリストも既定は空配列で、**exit code は決して変えない**。

```bash
overwatch lease --session <sid> [--json]
```

`<sid>` が保持中の live lease（PDO anchor）を——複数保持していれば最も直近に claim したものを——表示する。無ければ exit 1 で無音終了する。

各 lease は `heartbeat_at`（および `claimed_at`）タイムスタンプを持つ。staleness の判定は**固定**のハートビート TTL **1800 秒（30 分）**（`store::LEASE_TTL_SECS`）に対して行う。TTL はコンパイル時定数であり、設定変更はできない。

- `now - heartbeat_at > 1800s` になると lease は stale になる。次の lease 変更操作（`begin` / `run` / `end` / `heartbeat` / `reassign` / `reap` のいずれか）が `leases.json` から削除し、別セッションによるそのキーの `begin` は成功する。
- `overwatch reap` はその掃除だけを行う。

状態はリポジトリごとに `~/.overwatch/<project-key>/overwatch/`（`leases.json` + `events.jsonl`）に保存される。キーは MAIN worktree root から導出する。

main checkout・全 linked `git worktree`・linked worktree の subdirectory は同一の store を共有する（main checkout から `overwatch status` を実行すると linked worktree 側のセッションも見える）。

main worktree root を解決できない場合（`.git` ファイルが解釈できない）、`begin`・`lease`・`end` は worktree 単位の store へ fallback せずエラーになる。

git repo 外の cwd は cwd 自身がキーになる。

- `<base>`: `~/.overwatch`（`$HOME/.overwatch`）。
- `<project-key>`: リポジトリの MAIN worktree root の `<basename>-<正規化パスの fnv1a32>`（linked worktree 間で共有）。
- lease は `end` されるか、`reassign` で解放されるか、後続の lease 変更操作で stale として削除されるまで `leases.json` に残る。overwatch は lease を Claude Code プロセスに紐づけておらず、セッション終了時に lease を消す overwatch フックも無い。

`overwatch` は project-wide の実行/lease レジストリに加えて、gate 違反イベントを **正規化した signature** 付きで記録し、同一 signature が複数タスク/セッションにまたがって時間窓内に N 件以上再発したら **systemic issue** としてエスカレーションする。

```sh
overwatch record-violation --source <gate> --discriminator <d> --task <key> [--symbol <sym>] [--session <sid>] [--detail <text>]
overwatch violations [--json] [--threshold N] [--window-secs S]     # 全 signature の再発状況
overwatch escalations [--json] [--threshold N] [--window-secs S]    # systemic と判定された signature のみ
```

- `--source` は `blastguard | propguard | specguard | mutategate | donegate | reviewgate | tdd | budgetguard | autoflow | ctxrot` を受け付け、未知の token は source として parse されない。
- **signature 正規化**（`violation::normalize_signature`）では、同種の失敗（例: blastguard の同一 rule、mutategate の同一 mutation operator、specguard の同一 drift kind + symbol）は、大文字小文字・空白の違いを問わず同じ signature になる。
- **再発検知**（`violation::detect_recurrence`）は時刻を引数で受け取る。
- 既定ポリシーは 24 時間窓・3 件以上（`RecurrencePolicy::default`）。CLI では `--threshold` / `--window-secs` で呼び出しごとに上書きできる。
- **systemic 判定**は「閾値以上」かつ「複数タスク or 複数セッションにまたがる」の両方を要求する。単一タスクが同じゲートに何度もリトライして失敗するのは systemic ではなく local retry loop として区別する。
- 保存先は既存の project-wide registry と同じ `<base>/<project-key>/overwatch/` 配下の `violations.jsonl`（events.jsonl とは別ストリーム、append-only）。

`overwatch --help` が全サブコマンドを説明付きで列挙する。

- canary rollout: `canary-plan`（純粋な stage 分割）、`canary-gate`（PROCEED/ROLLBACK の JSON、rollback 推奨時は **exit 3**）、`canary-rollback-plan`（純粋な復元計画）、`record-rollback`（`rollbacks.jsonl` に rollback イベントを記録）
- review 窓口: `review-queue [--json] [--since] [--limit]`（5 source — systemic 違反・canary rollback・AI review finding・condukt escalation・未解決の merge conflict — を severity 順の 1 リストに統合）、`record-finding`、`record-disposition`、`review-metrics`、`compact-findings`、`resolve-merge-conflict`、`reconcile-fixed [--since-ref\|--range\|--last-n] [--dry-run]`（既定は直近 50 commit）、`auto-approved [--since] [--sample 5] [--seed 0]`
- Continuous-Audit: `audit-round record` / `audit-round close`、`audit-metrics [--window 3]`
- テレメトリ: `undetermined-metrics [--window-days N]`、`gate-outcomes`（donegate / tdd / reviewgate の発火結果を `~/.<gate>/state/log.jsonl` から集計）

読み取り側の exit code 規約: ledger/journal が（不在・空ではなく）**読めない**とき、`review-queue`・`review-metrics`・`reconcile-fixed`・`gate-outcomes` は **exit 3** し、`auto-approved` は非 0 で終わる。git 履歴も同じ扱いの入力で、`git log` が読めない（repository でない・git が無い・未知の ref）とき `reconcile-fixed` は exit 3・`commits_scanned: null`・`undetermined_sources` に `git log` を出し、`review-metrics` は stale-undisposed 件数を 0 ではなく `null` と報告する。

MIT
