# condukt 内部仕組みドキュメント

## 概要

condukt は「判断」と「決定論」を明確に分離したオーケストレーションエンジンです。

```
判断 (LLM が担う)              決定論 (condukt バイナリが担う)
  ├ 課題の解釈                    ├ schedule: 衝突解析 → 並列/直列バッチ生成
  ├ タスクへの分割 (JSON)          ├ worktree: create / merge / remove / cleanup
  ├ 各タスクの実装                 ├ state: run 追跡 + ステータス管理
  └ done_criteria に対する検証     └ gate: 完了条件の判定
```

LLM は interpreter / researcher / worker / verifier の 4 種のエージェントとして機能し、
condukt バイナリはそれらの間をつなぐ決定論的なグルーとして動作します。バイナリは
Rust 製の単一実行ファイルで、各サブコマンドが 1 つの責務を持ちます。

---

## アーキテクチャ概観

```
  /condukt <課題>
       |
  Phase 0    受領・open run チェック
       |
  Phase 0.5  リサーチ (条件付き)         ← condukt-researcher
       |
  Phase 1    解釈 → Decomposition JSON    ← condukt-interpreter
       |
  Phase 2    validate + route + schedule   ← condukt バイナリ + fugu-router (任意)
       |
  Phase 3    合意 (AskUserQuestion)        ← main skill (ユーザーとの対話)
       |
  Phase 3.5  競合チェック
       |
  Phase 4    run 初期化 (state init)
       |
  Phase 4.5  ベースライン取得 (state test)
       |
  Phase 5    並列実装 (batches)            ← condukt-worker (複数並列)
       |
  Phase 6    検証 (done_criteria 照合)     ← condukt-verifier + fugu-router record
       |
  Phase 7    完了ゲート + worktree 統合    ← condukt バイナリ (gate / merge / remove)
       |
  Phase 8    クローズ (commit/push はユーザー指示時のみ)
```

---

## Phase フロー（全フェーズ）

### Phase 0 — 受領

引数から課題文を取得します（引数が無ければ直前の会話の依頼を使います）。
`--dry-run` フラグが指定された場合は Phase 3 のスケジュール提示で処理を止めます。

**open run チェック**: まず停止中 run の有無を確認します。

```
condukt state list
```

確認結果に応じて以下の通り分岐します。

| open run 数 | 引数の性質 | 対応 |
|---|---|---|
| 0 件 | 「次は何をする」系 | Phase 0-next（プロジェクト状態から次の一手を探索）|
| 0 件 | 具体的な課題あり | 通常フロー（Phase 0.5 へ） |
| 0 件 | 空 | 直前の会話から課題を取る |
| **1 件** | **空** | **AskUserQuestion なしで自動的に Phase 0-alt（resume）へ** |
| 1 件 | あり | 新規課題として扱う（既存 run は放置） |
| 2 件以上 | 空 | AskUserQuestion でどれを再開するか確認 |
| 2 件以上 | あり | 新規課題として扱う |

**STUCK タスクの検知と回復**: `condukt state list` の結果に `running` 状態のタスクが
含まれる場合、前セッションの worker が途中で終了した可能性があります（stuck worker）。

```
condukt state abandon --run $RID --all-stuck
# コマンドが無い場合は個別に戻す:
condukt state set --run $RID --task <t.id> --status pending
```

`--all-stuck` の候補は TTL 超過（最終更新から 30 分超）の `running` タスクですが、TTL 超過は
**必要条件にすぎません**（backlog `356bd51d`）。実際に pending へ戻すのは、そのタスク自身の
worktree HEAD と `updated_at` を multi-sample で観測して `Known(Stalled)` が確定したタスクだけで、
`Progressing` も `Undetermined` も戻しません（fail-closed）。1 回目の呼び出しでは何も戻らないのが
正常です（1 回の観測は「凍結」を意味しない）。worktree が消えた本当に死んだ worker は
`Undetermined` のままなので、人間が明示する `--task <id>`（意図的にゲート無し）で戻します。
pending に戻したタスクは Phase 5 で通常通り再投入します。

---

### Phase 0-alt — Resume（中断 run の再開）

`--resume <RID>` が指定された場合、または Phase 0 でユーザーが再開を選んだ場合、
Phases 0〜4 をスキップして以下を実行します。

```
condukt state resume-context --run <RID>
```

返される JSON の内容で分岐します。

| 条件 | 次のアクション |
|---|---|
| `verified_count == total_count` | Phase 7（完了ゲート）へ |
| `needs_verification` が空でない | Phase 6（検証）から再開 |
| `pending_tasks` / `failed_tasks` が空でない | Phase 5（実装）から再開 |

`failed_tasks` の `failure_context` は以前の verifier 理由が state に無い場合は省略し、
`done_criteria` と `touched_files` のみを渡します。

---

### Phase 0-next — 次の一手の探索

open run が 0 件かつ引数が「次は何をする」系のとき、プロジェクト状態を確認して次の一手を導きます。

```bash
# 1. バックログを確認
BACKLOG=$(backlog list --status pending 2>/dev/null | head -10 || true)

# 2. compass の gap を確認（charter があれば）
COMPASS_GAP=$(compass gap 2>/dev/null | head -30 || true)

# 3. 直近の変更を確認
GIT_LOG=$(git log --oneline -10 2>/dev/null || true)
```

| 状態 | 対応 |
|---|---|
| バックログに pending 項目あり | 最優先の 1 件を課題文として Phase 0.5 へ進む |
| compass gap が明確な next_action を示す | それを課題文として Phase 0.5 へ進む |
| どちらもなく直近コミットから自明な続きがある | それを課題文として Phase 0.5 へ進む |
| 判断できない・選択肢が複数ある | AskUserQuestion でユーザーに候補を提示して選ばせる |

このフェーズで課題を自律決定して進む場合でも、Phase 3 の合意（AskUserQuestion）は省略しません。
「次の一手の探索」は課題の*発見*であり、実装の*承認*は別物です。

---

### Phase 0.5 — リサーチ（researcher agent、条件付き）

以下のいずれかを満たす場合に `condukt-researcher` を起動します。

- 課題が外部ライブラリ / API に依存しており、仕様が手元に無い
- 既知の落とし穴（breaking change・互換性問題）が想定される
- 新しいアーキテクチャパターンを導入する場合

以下の場合は省略して Phase 1 に進みます。

- 課題がコードベース内完結で外部依存が明らか
- 簡単なリファクタリングや設定変更

researcher を起動した場合、その出力 JSON を変数に受け取り、Phase 1 の interpreter
プロンプトに含めます。

```
RESEARCH_BRIEF=$(Task condukt-researcher "...")
```

---

### Phase 1 — 解釈（interpreter agent）

**knowledge 注入（soft 依存）**: interpreter を起動する前に知識ファイルを取得します。

```
KNOWLEDGE=$(condukt knowledge 2>/dev/null || true)
```

**playbook 検索（soft 依存）**: fugu-router が利用可能なら、類似過去タスクの手順を取得します
（Devin Playbooks 相当）。

```
PLAYBOOKS=$(fugu-router procedures search --query "<課題文の要約>" --k 3 2>/dev/null || true)
```

`condukt-interpreter` を `Task` で起動し、課題を Decomposition JSON に変換させます。

```json
{ "goal": "...", "tasks": [
  { "id": "t1", "title": "...", "touched_files": ["path/or/glob"],
    "deps": ["他タスクid"], "class": "parallel|serial|gated",
    "suggested_model": "sonnet|opus|haiku",
    "done_criteria": "検証で確認する合格条件",
    "confidence": "high|medium|low" }
]}
```

`open_questions` 相当が出た場合は、この時点で `AskUserQuestion` を 1 回使って解消します。

---

### Phase 2 — 検証 + ルーティング + スケジュール（決定論）

Decomposition JSON を一時ファイルに書き、以下を順に実行します。

```bash
condukt validate --file <json>       # 不正なら理由を提示しユーザーに差し戻し

# fugu-router があれば学習済み方策で suggested_model を上書き（soft 依存）
if command -v fugu-router >/dev/null 2>&1; then
  fugu-router route --file <json> --report <route.json> > <json.routed>
else
  cp <json> <json.routed>
fi

condukt schedule --file <json.routed>   # → {batches, serial, gated, warnings}
```

- `fugu-router route` は「似た過去タスクで検証を通った最安ティア」を選び `suggested_model` を確定します。
- `<route.json>` にはタスク id ごとの `verifier_model`・`basis`・`rationale` が入ります。Phase 6 の検証モデル選択に使います。
- `warnings`（shared_glob による serial 降格など）はユーザーに提示します。以降 `<json.routed>` を正とします。

---

### Phase 3 — 合意（main loop / AskUserQuestion）

`schedule` 結果（並列バッチ / serial / gated）を `AskUserQuestion` で提示して合意を取ります。
割り直しが出たら Decomposition を修正して Phase 2 へ戻ります。`--dry-run` ならここで停止します。

**confidence ゲート**: `confidence: low` または `confidence: medium` のタスクは、計画提示で
明示的に強調し、done_criteria や scope の確認を促します。

---

### Phase 3.5 — 競合チェック（conflict check）

`state init` の前に、同プロジェクトで実行中の他セッションとの衝突を確認します。

```bash
CONFLICT_JSON=$(condukt state conflict-check --file <json.routed> 2>/dev/null)
CONFLICT_EXIT=$?
```

コマンドが存在しないバージョンの場合（`exit 127` や "unknown subcommand"）はチェックをスキップします。

| exit | `auto_proceed` | 対応 |
|---|---|---|
| 0 | — | 衝突なし。そのまま Phase 4 へ |
| 1 | `true` | 衝突あり（全て inactive/paused）。警告を出して自動進行 |
| 1 | `false` | 衝突あり（active な run が存在）。AskUserQuestion でユーザーに確認 |

衝突種別には「ファイル競合（`conflicts`）」と「目的競合（`similar_goal_runs`）」があります。

---

### Phase 4 — run 初期化

```
RID=$(condukt state init --file <json>)   # tasks=pending で run を作成、run id を返す
```

`--label` を省略すると tty または `pid-<PID>` が自動填入されます。

---

### Phase 4.5 — ベースライン取得

実装開始前にテストスイートの現状を記録します。

```
condukt state test --run $RID > /tmp/condukt-baseline.txt 2>&1
BASELINE_EXIT=$?
```

- exit 0（全通過）: 以降 worker / verifier は「テストが新たに壊れた」ことを fail の根拠にできます。
- exit 非 0（既存失敗あり）: 失敗テスト一覧を `baseline_failures` として workers に渡します。verifier はこのリストに含まれる失敗を「実装前から壊れていた」として除外して合否を判定します。
- テストコマンドが未設定でエラーになる場合は無視して Phase 5 へ進みます。

---

### Phase 4.5.5 — Small-task fast path（省略可）

以下のいずれかを満たす場合、worktree 作成を省略して main で直接実装します。

- タスクが 1 つのみかつ `class: serial`
- 全タスクが serial で合計 2 つ以下

fast path 手順:

1. `condukt state set --run $RID --task <t.id> --status running`（worktree/branch なし）
2. main 上で直接実装・`git add && git commit`
3. `condukt state set --run $RID --task <t.id> --status done`
4. Phase 6（verifier）へ。Phase 7 の worktree merge/remove はスキップ。

parallel タスクが 1 つでも存在する場合、または serial タスクが 3 つ以上ある場合は通常フローになります。

---

### Phase 5 — 並列実装（worker agents）

`schedule.batches` を先頭から順に処理します（バッチ間は依存順、バッチ内は並列）。

バッチ内の各タスク `t` について:

1. `WP=$(condukt worktree create --topic <t.id> --branch condukt/<t.id>)`
2. `condukt state set --run $RID --task <t.id> --status running --worktree "$WP" --branch condukt/<t.id>`
3. `Task` で `condukt-worker` を起動（model=`t.suggested_model`）
4. worker の返却 status を確認する:
   - `done`: `condukt state set --run $RID --task <t.id> --status done` し、即座に Phase 6 の verifier を起動（パイプライン化）
   - `needs-serial`: 分類ミス。worktree を破棄してタスクを serial として main で実装し commit
   - `blocked`: AskUserQuestion でユーザーにエスカレーション

バッチ内は 1 メッセージで複数 `Task` を同時発行して並列化します。worker が完了するたびに
即 verifier を起動し、worker 完了の待ち合わせはしません（後続 worker が動いている間に先行タスクの
検証が進む）。`serial` タスクは worktree に出さず main で順に実装します。

**worker に渡すフィールド一覧**:

| フィールド | 必須/省略可 | 説明 |
|---|---|---|
| 作業ディレクトリ | 必須 | `condukt worktree create` の出力（`$WP`）|
| `touched_files` | 必須 | worker が触れてよいファイルのスコープ |
| `done_criteria` | 必須 | verifier が照合する合格条件 |
| `reproduction_tests` | 省略可 | TDD ループ起点。渡すと worker が red→green サイクルを回す |
| `target_symbols` | 省略可 | 編集対象の関数/クラス名 |
| `interface_context` | `target_symbols` あれば必須 | スコープ外シグネチャを main が事前収集して渡す |
| `knowledge_context` | 省略可 | プロジェクト固有の規約・落とし穴・推奨パターン |
| `peer_tasks` | 並列タスクがあれば必須 | スコープ衝突防止用の同バッチ他タスク情報 |
| `failure_context` | 再投入時のみ | `{reason, failed_tests, diff}` — 前回失敗情報 |

---

### Phase 6 — 検証（verifier agent）+ 実績の記録

**機械的 done_criteria の早期判定（verifier スキップ）**: `done_criteria` が観察可能な
事実の確認のみで構成される場合（grep / test -f / exit 0 確認など）、verifier agent を省略して
`Bash` で直接判定します。コマンド 1〜3 本で完結しない場合は通常の verifier フローを使います。

done の各タスクを `condukt-verifier` で done_criteria 照合します。検証モデルは
`<route.json>` の `verifier_model`（worker と別ティアの独立検証。無ければ既定 sonnet）を使います。

- pass: `condukt state set --run $RID --task <id> --status verified`
- fail: `condukt state set --run $RID --task <id> --status failed`（理由を控える）

**`kind: fix|feature` タスクの F→P（Fail→Pass）再現性ゲート**: `verified` への `state set` は、
対象タスクの RED→GREEN が有効な Fail→Pass オラクルであることを内部で確認し、満たさない場合は昇格を
拒否します。詳細は後述の「F→P（Fail→Pass）再現性ゲート」セクションを参照してください。

**confidence 再検証**: verifier が `pass` かつ `confidence: low` を返した場合は、
model を 1 ティア上げて同じタスクを再度 verifier に投げ、2 回 pass で verified に昇格します。

**カスケードエスカレーション（失敗タスクのリトライ）**:

verifier が fail したら同じターン内で以下を実行して Phase 5 へ再投入します。

1. タスクを `failed` に set
2. `failure_context` を組み立てる: `{ "reason": "...", "failed_tests": "...", "diff": "..." }`
3. `suggested_model` を 1 ティア上げる（haiku→sonnet→opus）
4. 新しい worktree を作成し、`failure_context` と escalated model で Phase 5 worker を再起動

リトライ上限: ティア数 = 最大 3 回。opus で失敗した場合は即ユーザーエスカレーション。

**実績の fugu-router への記録（soft 依存）**:

```
fugu-router record --title "<task.title>" --files "<task.touched_files>" \
  --class <task.class> --model <worker に使ったモデル> \
  --status verified|failed --cost <gauge から取れれば> \
  --done-criteria "<task.done_criteria>" \
  --notes "<worker サマリの要点>"
```

`--done-criteria` を渡すと、verified タスクの手順が `~/.fugu-router/playbooks.jsonl` に蓄積され、
次回 Phase 1 の playbook 検索に現れます（Devin Playbooks 相当）。

---

### Phase 7 — 完了ゲート + 統合

```
condukt state gate --run $RID    # exit 0 まで完了宣言しない
```

gate FAIL の場合、まず reconcile を試みます（branch がマージ済みのタスクを自動 verified に昇格）。

```
condukt state reconcile --run $RID
condukt state gate --run $RID    # 再チェック
```

reconcile 後も FAIL が残る場合の対処:

- `failed` タスク → Phase 6 のカスケードエスカレーションへ戻す
- worktree 残置 → `condukt worktree cleanup --remove` で掃除
- 未コミット → 該当 worktree 内で commit させる

各 verified タスクの worktree を自分の turn 内で閉じます。

```
condukt worktree merge --branch condukt/<id>
condukt worktree remove --path "$WP" --branch condukt/<id>
condukt worktree cleanup    # orphan が無いことを確認
```

**merge pre-flight 衝突への対処**:

1. 衝突している branch を特定する
2. 軽微で自動解消可能な場合: worktree 内で `git merge main` → コンフリクト解消 → commit → `condukt worktree merge` 再実行
3. 大きく再実装が必要な場合: タスクを `failed` に set し、Phase 6 経由で新しい worktree で再実装
4. 解消後に `condukt state gate --run $RID` で gate PASS を確認

gate PASS で統合完了を報告します（タスク表 / 変更ファイル / 検証結果 / GATED の残提案）。

---

### Phase 8 — クローズ

`commit` / `push` はユーザー指示時のみ実行します。GATED タスク（deploy 等）はユーザー承認を得てから別途実行します。

---

## エージェント役割

### condukt-interpreter

**目的**: 課題文を Decomposition JSON に変換する。

- 課題を分析し、独立実装可能な最小タスクに分割する
- 各タスクに `touched_files`・`deps`・`class`・`suggested_model`・`done_criteria`・`confidence` を付与する
- `open_questions` が生じた場合は main に返し、`AskUserQuestion` で解消させる
- `research_brief`（Phase 0.5）・`knowledge_context`（condukt knowledge）・`playbook_context`（fugu-router）を受け取って活用する

**出力**: Decomposition JSON（`condukt validate` と `condukt schedule` が消費する）

---

### condukt-researcher

**目的**: interpreter の前段で外部仕様・落とし穴を調査する（条件付き起動）。

- 外部ライブラリ / API の最新仕様を調査する
- breaking change や互換性問題を特定する
- 新しいアーキテクチャパターンの推奨事例を収集する

**出力**: `research_brief` JSON（interpreter プロンプトに含めて渡される）

---

### condukt-worker

**目的**: 割り当てられた 1 タスクを worktree 内で実装し commit する。

- 作業は割り当て worktree 内に限定する（`touched_files` スコープ外は触らない）
- スコープ外ファイルの変更が必要な場合は `needs-serial` を返す
- `reproduction_tests` があれば TDD ループ（red→green）で実装する
- `failure_context` があれば前回の失敗原因を分析し、別アプローチを取る
- `peer_tasks` の `touched_files` を確認してスコープ衝突を回避する
- 完了したら worktree 内で `git add -A && git commit`（merge はしない）

**返却ステータス**: `done` / `needs-serial` / `blocked`

---

### condukt-verifier

**目的**: タスクの `done_criteria` に対して実装が合格しているかを判定する。

- worktree 内の実装を `done_criteria` と照合する
- `touched_files` と `target_symbols`（あれば）をピンポイントで確認する
- worker と別ティアのモデルを使い独立した検証を行う（`route.json` の `verifier_model` を参照）
- `confidence: low` の pass は main が 1 ティア上のモデルで再検証する

**返却**: `pass` / `fail`（fail の場合は reason も返す）

---

## git worktree ライフサイクル

condukt は全ての並列タスクを専用の git worktree で実行します。worktree は main repo の外（`~/.condukt/worktrees/` 以下）に作成され、1 dir = 1 branch の原則を強制します。

### create（Phase 5 でタスク開始時）

```
WP=$(condukt worktree create --topic <t.id> --branch condukt/<t.id>)
```

- worktree のパスは `<worktree_base>/<project-key>/<branch>` 形式で作成される
- repo 外に作成することを強制する
- 同一 branch の重複作成を拒否する
- 作成後、`condukt state set --worktree "$WP" --branch condukt/<t.id>` で state に記録する

### merge（Phase 7 で verified タスクの統合時）

```
condukt worktree merge --branch condukt/<id>
```

- `condukt state gate` が PASS した後に実行する
- merge 前に pre-flight 衝突チェックを行う
- 衝突がある場合は詳細を返し、呼び出し側が解消する

### remove（Phase 7 で merge 後の後始末）

```
condukt worktree remove --path "$WP" --branch condukt/<id>
```

- merge が完了した worktree を削除する
- branch も削除する

### cleanup（Phase 7 の最終確認 / エラーリカバリー）

```
condukt worktree cleanup [--remove]
```

- orphan（state に記録されていない残置 worktree）を検出する
- `--remove` を付けると orphan を削除する
- `condukt state gate` が残置 worktree を検出した場合に使う

### list（現在の worktree 一覧の確認）

```
condukt worktree list
```

- 全 worktree の一覧と状態を表示する

---

## State Machine

### ステータス一覧

| ステータス | 意味 |
|---|---|
| `pending` | タスク作成済み、未着手 |
| `running` | worker が実装中（worktree / branch 情報が紐付く）|
| `done` | worker が実装完了・commit 済み（verifier 待ち）|
| `verified` | verifier が done_criteria を pass と判定（terminal）|
| `failed` | verifier が fail と判定（リトライ可、またはユーザーエスカレーション）|
| `cancelled` | ユーザーまたは skill によりキャンセル（terminal）|

### ステータス遷移図

```
pending
  |
  +--(worker 開始)--> running
                        |
                        +--(worker done)--> done
                        |                    |
                        |            +--(verifier pass)--> verified (terminal)
                        |            |
                        |            +--(verifier fail)--> failed
                        |                                    |
                        |                                    +--(escalate / 再投入)--> running
                        |                                    |   (model をティアアップ、max 3回)
                        |                                    |
                        |                                    +--(opus で fail / ユーザーエスカレーション)
                        |
                        +--(needs-serial / serial 降格)--> (main で直接実装)
                        |
                        +--(blocked / ユーザーエスカレーション)

cancelled (terminal) ← AskUserQuestion でユーザーがキャンセル
```

### condukt state サブコマンド一覧

| サブコマンド | 説明 |
|---|---|
| `condukt state init --file <json>` | run を作成し、全タスクを pending で登録。run id を返す |
| `condukt state set --run <rid> --task <tid> --status <s>` | タスクのステータスを更新（`--worktree` / `--branch` も同時に記録可）|
| `condukt state show --run <rid>` | run の詳細（タスク一覧 + ステータス）を表示 |
| `condukt state list` | 全 run の一覧を表示（open run の確認に使う）|
| `condukt state gate --run <rid>` | 全タスク verified かつ worktree 残置・未コミット無しを確認。満たさない場合 exit 非 0 |
| `condukt state stats` | 全 run の集計（完了率・タスク数・ステータス分布）|
| `condukt state reconcile --run <rid>` | branch がマージ済みまたは削除済みのタスクを自動 verified に昇格 |
| `condukt state resume-context --run <rid>` | pending / failed / done タスクを JSON で返す（再開用）|
| `condukt state test --run <rid>` | プロジェクトのテストスイートを実行（auto-detect: cargo/npm/pytest）|
| `condukt state abandon --run <rid> --all-stuck` | TTL 超過 **かつ** 進捗が `Known(Stalled)` と確定した running タスクを pending に戻す（multi-sample。`Progressing`/`Undetermined` は戻さない）|
| `condukt state conflict-check --file <json>` | 他セッションとのファイル競合 / 目的競合を確認 |
| `condukt state cancel --run <rid> --task <tid>` | タスクを cancelled（terminal）に設定 |
| `condukt state list-tasks` | キャンセル可能なタスク（pending/running/done）を一覧取得 |
| `condukt state check-oracle --run <rid> --task <tid>` | `kind: fix/feature` タスクの RED→GREEN が有効な Fail→Pass オラクルか判定（advisory・常に exit 0）。詳細は下記「F→P（Fail→Pass）再現性ゲート」参照 |

---

## F→P（Fail→Pass）再現性ゲート

`kind: fix|feature` タスク限定で、test-first（先にテストを書き、それが実装前に失敗し実装後に通る）の
証拠を決定論的に強制する仕組みです。`tdd` crate の `oracle` モジュールと、`condukt` の `oracle` モジュール
+ `enforce_fp_gate` が組んで実現しています。

### 判定の流れ

1. Decomposition JSON の `Task.kind`（省略可）が `fix` または `feature`（大小無視）のとき、
   `Task::requires_fp_oracle()` が true を返します。`chore`・その他の値・未指定はすべて false で対象外です。
2. `condukt state check-oracle --run <rid> --task <tid>` は `oracle::check_oracle` を呼び、対象タスクの
   worktree 内で `tdd oracle --task <tid>` を実行します。`tdd oracle` は RED（実装前）/ GREEN（実装後）の
   proof を 4 通りに分類します: `fail_to_pass`（唯一の有効なオラクル）/ `fail_to_fail` / `pass_to_pass` /
   `pass_to_fail`。
3. 出力 JSON の形:
   ```json
   { "required": true, "valid_fp_oracle": false, "fallback": false, "transition": "fail_to_fail", "reason": "..." }
   ```
   - `required`: `requires_fp_oracle()` が true かつタスクに `reproduction_tests` が設定されているときのみ true。
   - `fallback`: `tdd` が PATH に無い・spawn 失敗・stdout が空または JSON として壊れている・対象外タスク
     （`kind` が fix/feature でない、`reproduction_tests` が無い）のいずれかで true になります（fail-soft:
     パニックしない、非 0 終了しない）。
   - `valid_fp_oracle`: `fallback:false` のときのみ信頼できる、`tdd oracle` の実際の判定
     （`transition == "fail_to_pass"` のときだけ true）。

### `state set --status verified` での強制（`enforce_fp_gate`）

`condukt state set --run <rid> --task <tid> --status verified` は昇格の直前に上と同じ検証を内部で
再実行し、`state::enforce_fp_gate(&verdict)` に判定させます:

- `required && !fallback && !valid_fp_oracle` の場合のみ `FpGateDecision::Reject` — 昇格を拒否し
  非 0 終了します（理由付き）。
- それ以外（対象外・fallback・valid のいずれか）は `FpGateDecision::Allow(Option<bool>)`。`fallback` の
  ときは `None`（信頼できる判定が無いので永続化しない）、そうでなければ `Some(valid_fp_oracle)` を返します。

`Allow` で得た値は `TaskState.fp_oracle_valid: Option<bool>` として run state（`Status::Verified` への
遷移時のみ）に永続化されます。

### `gate_reasons` の defense-in-depth

`condukt state gate` が呼ぶ `gate_reasons` は、`state set` のゲートを何らかの理由（手動編集・古い
バイナリなど）で迂回して `verified` になったタスクも検出します。`status == Verified` かつ
`fp_oracle_valid == Some(false)` のタスクが見つかれば「test-first の再現なしに verified にされた」旨の
理由文字列を追加し、`state gate` を fail させます。

### fail-soft の設計

`oracle` モジュールは `tdd` バイナリの不在・worktree の消失・stdout の欠落や破損のいずれでもパニックせず、
必ず `fallback:true` の JSON を返します。この場合 `enforce_fp_gate` は常に `Allow` を返すため、Phase 6 の
従来の検証（`state check-criteria` → verifier agent）にすべて委ねられます。F→P ゲートは「あれば強くする」
追加の防御線であり、唯一の必須経路ではありません。

---

## fugu-router 連携

fugu-router は condukt のソフト依存です。存在しない場合でも condukt は動作しますが、
存在する場合はルーティングの精度と playbook の蓄積が向上します。

### Phase 1: playbook 検索（Devin Playbooks 相当）

```bash
if command -v fugu-router >/dev/null 2>&1; then
  PLAYBOOKS=$(fugu-router procedures search --query "<課題文の要約>" --k 3 2>/dev/null || true)
fi
```

過去に verified になったタスクの手順（playbook）を検索し、interpreter プロンプトに
`playbook_context` として渡します。interpreter はこれを参考に Decomposition を生成します。

### Phase 2: routing（モデル選択の最適化）

```bash
if command -v fugu-router >/dev/null 2>&1; then
  fugu-router route --file <json> --report <route.json> > <json.routed>
fi
```

- 「似た過去タスクで verified になった最安ティア」を学習済み方策で選択します
- `suggested_model` を決定論的に確定し、Decomposition JSON に反映します
- `<route.json>` には `verifier_model`（worker とは別ティアで独立検証するモデル）も含まれます
- fugu-router が無い場合は interpreter が出力した `suggested_model` のまま続行します

### Phase 6: recording（実績記録）

```bash
fugu-router record --title "<task.title>" \
  --files "<task.touched_files をカンマ区切り>" \
  --class <task.class> \
  --model <worker に使ったモデル> \
  --status verified|failed \
  --cost <gauge から取れれば> \
  --done-criteria "<task.done_criteria>" \
  --notes "<worker サマリの要点>"
```

verified タスクの実績（モデル・ファイル・クラス・cost・手順）が `~/.fugu-router/playbooks.jsonl`
に蓄積されます。次回の Phase 1 playbook 検索と Phase 2 routing で参照されます。
failed の場合は recording されません。

---

## 設計原則と不変条件

### 不変条件（外さない）

1. **合意は main loop のみ** — `AskUserQuestion` はこの skill（main）でしか使えません。合意未了のタスクを実装に渡しません。

2. **GATED は子に実行も承認もさせない** — `class: "gated"` のタスクは `condukt schedule` が `gated` に分離します。実装フェーズの対象外です。承認はユーザーから main で得ます。

3. **共有ファイルは直列** — `condukt schedule` が `shared_globs` 設定と file 衝突解析で `serial` に落とします。serial タスクは worktree に出さず main で順に実装します。

4. **並列実装の子は専用 worktree、1 dir = 1 branch** — worktree は `condukt worktree create` が作ります（repo 外・branch 重複拒否を強制）。各子は自分の turn 内で commit します。

5. **完了は `condukt state gate` が判定** — 「全タスク verified かつ worktree 残置・未コミット無し」を満たすまで完了宣言しません。

### 設計原則

- **LLM と決定論の分離**: 判断（解釈・実装・検証）は LLM、決定論（衝突解析・スケジュール・worktree・状態・完了ゲート）はバイナリ。
- **ソフト依存**: fugu-router は無くても動作する。あれば精度が上がる設計。
- **subscription-native**: `ANTHROPIC_API_KEY` 不要。Claude Code の skill + agent + SessionStart hook として動作する。
- **単一真実源**: `crates/condukt/` がソースオブトゥルース。install キャッシュ（`~/.claude/plugins/cache/`）は編集しない。
- **condukt 自身の改修**: workers が触れるファイルは git リポジトリ側のみ。統合後に `scripts/sync-plugin-assets.sh` でローカル install を更新する。
