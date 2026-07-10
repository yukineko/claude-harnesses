# harness 使い方パターン集

各プラグインの詳細は `crates/<plugin>/README.md`、全体設計は [OVERVIEW.md](OVERVIEW.md) を参照。
ここでは「セッションを開いてから何を打てばよいか」を典型パターンで示す。

---

## パターン 0 — autopilot で開いている仕事を回す (/flow)

「次の課題を自分で選んで実行し続けてほしい」とき。`/flow` は **source（compass の一手 / backlog のキュー）→ executor（condukt）** を 1 本のループで束ねる。

```
/flow
```

- SessionStart で開いている仕事（compass の next move・open backlog・未完了 condukt run）があると、`flow propose` フックが `/flow` を **propose-then-confirm** で提案する。承認すると起動。
- 課題文を直接渡すと source 選択を飛ばして condukt に直行し、1 件だけ実行して終了する:

```
/flow READMEなどの関連文書は更新されたか?
```

ループは `compass gap` で鮮度をゲートし（charter が陳腐なら自動実行せず `/compass` を促す）、backlog ロックを取得して二重ループを防ぐ。`/flow` は `/backlog` の上位互換なので**併走させない**（backlog ロックで物理的に直列化される）。

**autonomy switch**: config の `autonomous` または env `CONDUKT_AUTONOMOUS=1` を立てると、`/condukt` が `condukt state autonomy-check` の exit code で分岐して Phase 3 の人間合意などのゲートを縮退する（完全自走）。既定は無効（HOTL 維持）。有効化前に `donegate` / `reviewgate` / `propguard` などの検証ゲートを整えておくこと。

```
/flow
  └─ compass ゲート: charter 鮮明 → 一手を to_condukt に保持
  └─ backlog lock acquire（クロスセッション直列化）
  └─ ループ: compass 主筋 → backlog next の順でピック → /condukt → 検証 → done
  └─ source が尽きる/予算超過/中断で backlog lock release + サマリ報告
```

---

## パターン 1 — "次に何をすべきか分からない" (Phase 0-next)

```
/condukt 次は何をする
```

open run が 0 件 かつ引数が「次の一手を探す」系のとき、condukt は以下を自動で確認して
最優先の一手を選ぶ:

1. `backlog list` — pending バックログ項目（standalone backlog crate が唯一の正典キュー）
2. `compass gap` — 北極星ゴールとの gap が示す next_action
3. `git log --oneline -10` — 直近コミットから自明な続き

候補が複数あり判断できない場合は `AskUserQuestion` で提示する。

**例: バックログに未解決項目がある場合**

```
セッション開始
  └─ SessionStart hook: condukt(restore) が "未完了 run 1件" を通知
                        specguard pending が drift sentinel を提示 (あれば)
                        compass が北極星ゴールを再提示

/condukt 次は何をする
  └─ backlog に pending decision-log プラグイン実装 が 1 件 → それを課題文として採用
  └─ Phase 1: interpreter が分割
  └─ Phase 3: AskUserQuestion で確認
  └─ Phase 5-6: 並列実装 → 検証
  └─ Phase 8: specguard が自動で drift 監査
```

---

## パターン 2 — compass で目標を再接地してから実装

北極星ゴールが曖昧になってきた、または久しぶりにセッションを開いたときに使う。

```
/compass
```

compass は現状のコードベースと charter (目標ドキュメント) を照合して gap を示し、
"右サイズの一手" を提案する。その提案文をそのまま condukt に渡す。

```
/condukt <compass が提案した一手>
```

**例: 新機能の方向性を compass に聞いてから実装**

```
/compass
  └─ gap: "PDO ワークフローに外部 MCP 認証が未統合"
  └─ next_action: "Linear MCP の authenticate を設定し condukt に組み込む"

/condukt Linear MCP の authenticate を設定し condukt に組み込む
  └─ Phase 0.5: researcher が Linear MCP API を調査
  └─ Phase 1: interpreter が分割 (認証設定 / condukt hook 追加 / テスト)
  └─ ...
```

---

## パターン 3 — backlog から直接選んで実装

`backlog list` でバックログを確認し、特定の項目を指定する。

```
backlog list
/condukt <バックログ項目の内容>
```

または `バックログ ID` を使って:

```
/condukt bk-b297184b の decision-log プラグインを実装する
```

---

## パターン 4 — 具体的な課題を直接渡す (最もシンプル)

何を実装するか決まっているとき。

```
/condukt specguard の bin/ にある pre-compiled binary を使うよう hooks.json を修正する
```

condukt は Phase 0 で open run をチェックし、無ければそのまま Phase 0.5 → 1 → 3 と進む。

**dry-run で計画だけ確認したい場合:**

```
/condukt --dry-run <課題>
```

Phase 3 の schedule 提示まで進んで停止する。

---

## パターン 5 — 中断した run を再開

セッションが途中で切れたり、`/stop` したりした場合。

**open run が 1 件だけなら引数なしで自動再開:**

```
/condukt
```

Phase 0 で open run を検知し `AskUserQuestion` なしで自動的に Phase 0-alt (resume) に入る。

**run ID を明示して再開:**

```
/condukt --resume run-20260626-xxxxxx
```

**stuck worker がいる場合 (running 状態が残っている):**

```
condukt state abandon --run <RID> --all-stuck
/condukt --resume <RID>
```

---

## パターン 6 — 実装前に仕様を確認する (specguard brief)

コードを書き始める前に「この変更は既存の仕様と整合しているか」を確認する。

```
/specguard:brief specguard の hooks.json を CLAUDE_PLUGIN_ROOT 形式に変更する
```

specguard は関連する canon ドキュメントを read-only で読み、変更が仕様と矛盾しないか・
仕様に沿ったアプローチを採っているかをブリーフィングする。その後 `/condukt` で実装。

---

## パターン 6.5 — fix/feature を F→P 再現ゲートに通す

`kind:"fix"|"feature"` のタスクは、`verified` に昇格する前に **task-specific な再現テストが
変更前ツリーで fail・変更後ツリーで pass する（Fail→Pass 遷移）** ことを機械検証される。
`condukt state set --status verified` が worktree 内で `tdd oracle --task <id>` を実行し、
有効な F→P でなければ昇格を拒否する（`condukt state check-oracle` と同じ判定）。

**使い方（worker 視点）:**

```
tdd red --task <id> --cmd "<test cmd>"   # テストが FAIL することを要求し赤を記録
# 実装
tdd green --task <id>                     # 赤 proof を要求しテスト PASS を要求、緑を記録
tdd oracle --task <id>                    # {"transition":"fail_to_pass","valid_fp_oracle":true} なら通過
```

- **発火条件**: `kind` が fix / feature のときだけ必須（`Task::requires_fp_oracle()`）。
  chore / 未設定は免除。`kind` はスケジュール後
  `~/.condukt/state/<proj>/<run>.decomposition.json` に保存される（`state set` に `--kind` フラグは無い）。
- **fail-soft（never break a turn）**: tdd バイナリ不在・オラクル生成不能・baseline 取得失敗 →
  従来の done_criteria ゲートに縮退する。
- **落とし穴（実地で確認済み）**: 追加専用の内部モジュールは自然な F→P を持たない。condukt は
  bin crate なので、新規モジュールの unit test は変更前ツリーに存在せず「失敗」ではなく
  「不在＝素通り」になり、赤を記録できない。この種のタスクは `kind:"chore"` にし、実際の F→P は
  「配線されて観測可能になる層」——新サブコマンド / フックを叩く `tests/` の integration test
  （変更前は subcommand 不在で fail、変更後 pass）——に置く。

---

## パターン 7 — drift が見つかったセッション開始

```
SessionStart hook: specguard pending
  → "修正候補あり: condukt-interpreter.md のスキーマに linked_hypotheses が未追加"

(修正する)

/specguard:ack    # sentinel をクリア
```

drift を放置したまま実装を続けると次回の監査でも同じ指摘が出る。
`ack` は修正が完了してから呼ぶ。

---

## condukt に自動統合されているツール

以下は `/condukt` を実行すると自動的に呼ばれる。個別に打つ必要はない。

| タイミング | ツール | 何をするか |
|---|---|---|
| Phase 1 (interpreter 前) | `condukt knowledge` | プロジェクト固有の知識を interpreter に注入 |
| Phase 1 (interpreter 前) | `fugu-router procedures search` | 類似過去タスクの手順を注入 |
| Phase 1 (interpreter 前) | `hypothesis list --status open` | open 仮説を interpreter に注入 |
| Phase 1 (interpreter 前) | `.deepwiki/*.md` 一覧 | アーキテクチャ wiki ページを interpreter に渡す |
| Phase 2 | `fugu-router route` | 過去実績から最安モデルを選択 |
| verified 昇格時 | `tdd oracle`（F→P 再現ゲート） | fix/feature タスクの Fail→Pass 再現を機械検証し、無効なら `verified` 昇格を拒否（fail-soft で done_criteria ゲートに縮退） |
| worker の Edit/Write 時 | `condukt editgate`（edit-time compile ゲート） | worktree 内 `.rs` の broken edit を PostToolUse で弾き、同一ターンで修正させる（fail-soft） |
| Phase 8 (gate PASS 後) | `hypothesis validate` | linked_hypotheses を自動クローズ |
| Phase 8 (gate PASS 後) | `specguard ingest` | drift 監査 (non-blocking) |
| Phase 8 (gate PASS 後) | `deepwiki refresh` | アーキテクチャ wiki を更新 (non-blocking) |

### edit-time compile ゲート (editgate)

condukt-worker の frontmatter に **PostToolUse フック**（matcher `Edit|Write|MultiEdit`）が仕込まれており、
worker が worktree 内の Rust ファイルを Edit / Write すると
`condukt editgate`（stdin から PostToolUse payload を読む）が `cargo check` を走らせる。

- **broken edit**（worktree 内の `.rs` が compile / type error）→
  `{"decision":"block","reason":"<診断>"}` を stdout に返し、worker に同一ターンで修正させる
  （壊れた編集を完了ゲートまで持ち越さない）。
- **fail-soft**: 非 Rust・アクティブ worktree 外・checker 起動不能・空 / 不正 stdin →
  何も出力せず exit 0（never break a turn）。
- **位置づけ**: F→P 再現ゲートと同じ「実行オラクル」基盤の write-time 版。壊れた編集を edit 時点で弾く。

---

## 手動で使うツール

### コンテキスト管理 (ctxrot)

context が膨らんできたとき:
```
/distill
```
確定した決定事項・残課題・触ったファイルだけを `~/.ctxrot/store/` に蒸留保存する。
`/compact` の直前に打つと次セッションでも引き継がれる。PreCompact フックで自動化もされているが、
手動でも使える。

特定のノートを読み込む:
```
/ctx load <note-name>     # ノートをコンテキストに追加
/ctx pin <note-name>      # セッション中ずっと保持
/ctx list                 # 保存済みノート一覧
```

### TDD ゲート

テストファーストで実装するとき (donegate/tdd Stop フックが通っていない場合):
```
/tdd red      # 失敗するテストを書いたことを証明
/tdd green    # テストを通す実装を完了
/tdd verify   # RED→GREEN の順序を検証して Stop を解除
```

### specguard の追加コマンド

```
/specguard:scope                   # エージェントを起動せずスコープだけ確認
/specguard:decide <title>          # 仕様変更の理由を ADR として記録
/specguard:accept-prompt <reason>  # 監査プロンプト (meta-canon) を批准
```

`/specguard:decide` は「なぜこの仕様にしたか」を canon commit にピン留めする。
次回以降の監査で「理由が陳腐化していないか」を D3 として自動チェックする。

### deepwiki

アーキテクチャ wiki を手動で生成・更新するとき:
```
/deepwiki
```
`.deepwiki/*.md` を生成または鮮度更新する。大きなリファクタリング後や新メンバーへの説明前に。
condukt Phase 8 でも自動実行されるが、任意のタイミングで手動更新も可。

### セッション記録 (/record)

セッション終了後、Obsidian vault に AEGIS 形式のセッション記録ノートを書く:

```
/session-insights:record
```

1. `session-insights record-now` が数値ブロック（コスト・トークン・ターン数・ファイル数）を自動生成し、ノートパスを返す。
2. 散文セクション（完了サマリ・つまずき・振り返り・残課題・関連）を Sonnet サブエージェントがこのセッションの transcript から埋める。
3. `backlog`（standalone backlog crate）でバックログを更新（完了項目を `backlog done <id>`、新規残課題を `backlog add --title ... --project ...`）。

`record = true` が `session-insights.toml` に設定されていれば **SessionEnd フックで自動実行**（数値ブロックのみ）。
`/record` は散文セクションまで埋める追加ステップ。

### バックログの移行（旧 session-insights backlog → standalone backlog）

クロスプロジェクト残課題キューは **standalone `backlog` crate**（`~/.backlog/tasks.toml`）が唯一の正典になった。
旧 `session-insights backlog`（state_dir の `backlog.json` / Obsidian `backlog.md`）は廃止済み。
旧 `backlog.json` が残っている場合は、open 項目を一度だけ standalone へ投入する（冪等。`backlog.json` が空・不在ならノーオペ）:

```sh
# 既定の state_dir は ~/.session-insights/state（session-insights.toml の state_dir で上書き可）
STATE_DIR="$HOME/.session-insights/state"
BACKLOG_JSON="$STATE_DIR/backlog.json"
if [ -s "$BACKLOG_JSON" ]; then
  jq -r '.[] | select(.status=="open") | [.project, .text] | @tsv' "$BACKLOG_JSON" \
    | while IFS=$'\t' read -r project text; do
        [ -n "$text" ] && backlog add --title "$text" --project "${project:-default}"
      done
else
  echo "no backlog.json to migrate (nothing to do)"
fi
```

移行後は `backlog.json` を削除してよい。Obsidian の `backlog.md` は session-insights の自動生成物で、今後は生成されない。

### 観測・振り返り

```
/harness-status:status      # コスト・ターン数・残 budget を 1 画面で確認
condukt state stats         # 全 run の完了率・モデル分布を集計
/difflog                    # セッション中の git diff を構造化ログで振り返る
```

harness-status は HOTL 点検ダッシュボード（CLI 専用）で、面ごとにサブコマンドがある。加えて
`SessionStart` hook を1本だけ持ち、登録済み hook の binary がディスク上に無いときだけ警告する
（健全時は無出力・fail-soft）:

```
harness-status budget       # 今日のコスト
harness-status sessions     # 直近セッション
harness-status progress     # 進捗ファイル
harness-status hooks        # Stop ゲートの遅延集計
harness-status inject       # UserPromptSubmit 注入サイズ集計
harness-status hooks-health # 登録済み hook の binary 欠損チェック
harness-status plugins      # 全プラグインの activation-scope 分類
```

### hypothesis (仮説管理)

condukt Phase 8 で linked_hypotheses は自動 validate されるが、手動でも操作できる:
```
hypothesis list --status open        # 未検証仮説一覧
hypothesis validate <id> --run <RID> # 仮説を検証済みに
hypothesis reject <id> --run <RID>   # 仮説を棄却
```

---

## セッション開始チェックリスト

セッションを開いたとき SessionStart フックが以下を順に提示する:

| フック | 何を表示するか |
|---|---|
| `ctxrot restore` | 前セッションの決定事項・残課題 (ctxrot ノート) |
| `condukt restore` | 未完了 run・orphan worktree の有無 |
| `compass` | 北極星ゴールのリマインド |
| `daily session-start` | `cargo deny` セキュリティ監査の所見 (1 日 1 回・所見があれば) |
| `flow propose` | 開いている仕事があれば `/flow` を提案 (propose-then-confirm) |
| `specguard pending` | 未処理 drift sentinel (あれば) |
| `taskprog` | progress.md の現状 |

これらを読んでから上記パターンのどれかで作業を始める。

---

## よく使うコマンド早見表

| やりたいこと | コマンド |
|---|---|
| 開いている仕事を autopilot で回す | `/flow` |
| 課題を flow に直渡しで 1 件実行 | `/flow <課題>` |
| 次の一手を自動決定 | `/condukt 次は何をする` |
| 北極星を再接地 | `/compass` |
| バックログ確認 | `backlog list` |
| 具体的な課題を実装 | `/condukt <課題>` |
| 計画だけ確認 | `/condukt --dry-run <課題>` |
| 中断した run を再開 | `/condukt` (1件のみ) or `/condukt --resume <RID>` |
| 実装前に仕様確認 | `/specguard:brief <課題>` |
| drift 監査を手動実行 | `/specguard:run` |
| drift sentinel を解除 | `/specguard:ack` |
| セッション記録を書く | `/session-insights:record` |
| コスト・進捗を確認 | `/harness-status:status` |
| 全 run の統計を見る | `condukt state stats` |
| context を蒸留 | `/distill` |
| ノートを読み込む | `/ctx load <note>` |
| TDD ライフサイクル | `/tdd red` / `/tdd green` / `/tdd verify` |
| F→P の赤/緑 proof を記録 | `tdd red --task <id> --cmd "<test>"` / `tdd green --task <id>` |
| 赤→緑遷移を分類し valid_fp_oracle を判定 | `tdd oracle --task <id>` |
| worktree で F→P オラクルを実行 (fail-soft) | `condukt state check-oracle --run <RID> --task <id>` |
| edit-time gate 本体 (PostToolUse hook が呼ぶ・手動実行は通常不要) | `condukt editgate` |
| 仕様変更を ADR 記録 | `/specguard:decide <title>` |
| アーキテクチャ wiki 更新 | `/deepwiki` |
| 仮説一覧 | `hypothesis list --status open` |
