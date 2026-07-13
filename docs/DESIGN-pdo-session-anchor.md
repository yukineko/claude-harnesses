# DESIGN: PDO Session Anchor — セッションの記憶保持・スコープ逸脱防止・並行セッション分離

> ステータス: **草案（未実装・未レビュー）**。[DESIGN-pdo-space.md](DESIGN-pdo-space.md)（git-native
> チーム共有）の優先度を下げた上での再設計。DLC（AI-DLC の stage/scope ワークフロー構造そのものの
> 採用）は本仕様では**採否を問わない**——効きそうな要素（Intent 的な単位分離）だけを個人運用の文脈で
> 借用し、DLC 全体を輸入するかどうかは別問題として扱う。実装前に人間レビューを要する。

**個人開発で PDO を回すときの実際の悩みは2つ: (1) セッションが自分は何をやっていたか忘れる、
(2) セッションが本来の課題からじわじわ逸れていく。この2つへの最も効く処方箋は、チーム共有ではなく
「1セッション＝1 PDO 単位」を機械的に強制し、その単位の記憶（scope・done_criteria）を持続的に
再注入し、逸脱をその場で検知することである。**

---

## 1. 動機

### 1.1 実際の悩み（ユーザー提示）

> 今の悩みは、各session がなにをやっているか忘れること。また session が本来の課題からどんどん
> それていること。

これは2つの異なる失敗モードだが、根は同じ: **セッションが「自分が今どの PDO 単位（仮説／backlog
item／compass の一手）に紐づいているか」を保持し続けられていない。**

- **記憶喪失**: 長いセッション・compaction を経るうちに、最初に合意した done_criteria やタスクの
  輪郭が context から薄れる。
- **スコープ逸脱**: 輪郭が薄れた結果、目についた別の問題やついでの改善に手を伸ばし、当初のタスクから
  静かに離れていく。

### 1.2 なぜ団体（DLC/Space）より先に個人の分離が要るか

前回提案した [PDO Space](DESIGN-pdo-space.md) は「チームの他の人が既に検証した仮説を知る」という
チーム間の情報共有を扱うもので、**セッション自身の記憶やスコープ管理には一切効かない**。単一開発者が
複数セッションを並行させる今の運用では、他人と情報を共有できることより、**自分の各セッションが
「今何をしていて、どこまでが自分の担当範囲か」を見失わないこと**の方が直接効く。

さらに、複数の PDO セッションを並行させる場合（例: 1つは仮説Aの実験、もう1つは backlog の別item）、
**互いの実装が衝突しないこと**も同じ根から来る要求である——各セッションが自分のスコープを正確に
保持していれば、他セッションのスコープと重ならないことを機械的に確認できる。「記憶」「スコープ」
「並行分離」は、**同じ1つのデータ（そのセッションが今担当している PDO 単位とその scope）**を
異なる角度から使っているだけの、単一の問題である。

---

## 2. 現状の再点検（証拠）— 車輪の再発明をしない

この harness には関連機構が既にかなりある。再設計はこれらを**繋ぎ直す・継続的にする・
condukt run の外にも広げる**ことに絞り、新しい検出ロジックは極力足さない。

| 機構 | 何をするか | 発火タイミング | 本仕様への示唆 |
|---|---|---|---|
| `condukt` Phase 3.5 `state conflict-check` | `state init` 前に、同プロジェクトの他 active/paused run とのファイル競合（`conflicts`）・目的競合（`similar_goal_runs`）を検出 | condukt run 開始直前のみ | **既に「並行分離」の核心を実装済み**。ただし condukt run の外（後述）には効かない |
| `condukt schedule` の `touched_files` 競合分析 | 同一 run 内のタスク同士がファイル競合しないバッチだけを並列化 | run 内のスケジューリング時 | run 内の話であり、run 間の話は Phase 3.5 が担当（上記） |
| `condukt state claim-task --hashkey` | backlog item 単位のクロスセッション占有（多重着手防止） | flow のピック時・着手直前（TOCTOU ガード） | 「同じ item を2セッションが取らない」は担保済み。「違う item だが scope が重なる」は対象外 |
| `overwatch` lease（`begin`/`heartbeat`/`end`） | project-wide の key 単位 exclusive claim + liveness | 任意のタイミング | key は opaque な文字列のみ。**scope（touched files）を持たない** |
| `overwatch status` | project-wide の進行中作業の集約ビュー（PDO progress view） | SessionStart / Stop | 「今何が進行中か」の一覧は出せるが、**個々のセッションが自分の担当を忘れないための再注入はしない** |
| `ctxrot` re-anchor | 既知の決定事項をウィンドウ末尾付近で再注入（lost-in-the-middle 対策）、band≥2 かつ 8 プロンプトに1回 | UserPromptSubmit（`guard`） | **再注入の仕組みは既にある**。対象が「決定事項」全般であり、「このセッションが担当する PDO 単位」に特化していない |
| `taskprog` | `.claude/progress.md` をセッション間で同期 | SessionStart / SessionEnd | 複数セッション**間**の引き継ぎ用。単一セッション**内**の記憶保持には非対応 |
| `compass` breadcrumb | Stop 時に「次の物理的一歩」を charter へ書き戻す | Stop | 次に何をするかの記録であり、今のセッションが今何をしているかの継続的な保持ではない |
| `stuckguard` PostToolUse | ツール呼び出し列から repeat/oscillation を検出しナッジ。**編集ファイル名は既に signature に含めて追跡している** | PostToolUse（毎ツール呼び出し） | **スコープ逸脱検出に転用できる下地が既にある**（後述 §4.3） |

結論: **「並行セッションの衝突防止」は condukt run の内側では既にかなり強い。弱いのは
(a) 記憶の持続的な再注入、(b) condukt run の外側（測定ステップ・調査・雑談的な作業）での
scope 登録・衝突検知、(c) 逸脱の即時検知**の3点。

---

## 3. 設計原則

1. **新しいデータを増やさない、既存の overwatch lease を拡張する** — 「このセッションは何を
   担当しているか」を表すデータは overwatch の `Lease` に既にある骨格（key/title/session_id/
   run_id）の自然な拡張として持たせる。新しいストアを作らない。
2. **新しい検出ロジックを増やさない、既存の stuckguard PostToolUse を拡張する** — スコープ逸脱の
   検知は「編集ファイルの追跡」という stuckguard が既にやっていることの上に1検出器足すだけにする。
3. **condukt run の外まで anchor を届ける** — condukt の conflict-check/claim-task は run の
   lifecycle にしか効かない。flow が PDO 単位（仮説・backlog item・compass 一手）をピックした
   **その瞬間**に、run を起こす起こさないに関係なく anchor を登録する。
4. **強制ではなく advisory から始める** — スコープ逸脱の検知は stuckguard の既存方針
   （ブロックしない・余計な1行のコンテキストで済む）を踏襲する。fail-closed にするかどうかは
   運用実績を見てから判断する（§9）。
5. **fail-soft** — anchor が無い・壊れている・overwatch が未導入でも、セッションは今まで通り
   動く（劣化するのは「記憶の補助」だけ）。

---

## 4. アーキテクチャ

### 4.1 anchor データモデル（overwatch::Lease の拡張）

```rust
// 既存フィールドはそのまま、2つ追加（#[serde(default)] で後方互換）
struct Lease {
    key: String,
    title: String,
    session_id: String,
    run_id: String,
    claimed_at: i64,
    heartbeat_at: i64,
    // ── 新規 ──
    scope: Vec<String>,          // touched files / glob（例: ["crates/hypothesis/src/**"]）
    done_criteria: Option<String>, // このセッションの「完了」の定義（compass/condukt と同じ語彙）
}
```

- `scope` は condukt の `touched_files` と同じ語彙（glob 可）。condukt run 経由なら
  Decomposition の `touched_files` をそのまま流用でき、二重管理にならない。
- `scope` が空 = 「まだ scope が確定していない」（調査・compass の carve ループ中など）を意味し、
  衝突検知の対象からは除外する（false positive を避ける）。

### 4.2 登録: flow のピック時に必ず anchor を立てる

flow の Step 3-1（次のタスクを優先度順にピック）で、どの source（compass 主筋 / measure step /
backlog / open 仮説）を選んだ場合も、**課題文を組み立てた直後**に:

```bash
overwatch begin --key "<pdo-unit-id>" --title "<task title>" \
  --scope "<touched_files をカンマ区切り、不明なら省略>" \
  --done-criteria "<done_criteria>"
```

を呼ぶ（新規 CLI 引数 `--scope`/`--done-criteria`、既存の `begin` はこれらを省略しても動く）。
これにより、condukt run を起こす起こさないに関わらず——measure step のような「condukt を起動しない
軽い PDO 作業」でも——このセッションが何を担当しているかが project-wide レジストリに乗る。

condukt run を実際に起こす経路（backlog の通常タスク）では、Phase 3.5 の `conflict-check` が
今まで通り condukt 自身の `claims.json` で二重チェックする。**2つの claim 機構が競合するわけでは
なく、overwatch 側は「condukt run の外まで届く広い anchor」、condukt 側は「run 内の詳細な
scheduling 用の厳密な claim」という役割分担のまま**でよい（重複は許容——両方が同じ結論を出すのは
むしろ健全性の確認になる）。

### 4.3 継続的な再注入: 記憶喪失対策

`ctxrot` の re-anchor 機構（`guard` フック、band≥2 かつ 8 プロンプトに1回）に、**現在の
session_id が保持している live lease** を読んで再注入する経路を追加する:

```
UserPromptSubmit → ctxrot guard
  → (既存) band チェック・大ファイル警告
  → (新規) overwatch lease --session $CLAUDE_CODE_SESSION_ID があれば、
    「あなたは今 <title> を担当中。done_criteria: <...>。scope: <...>」を
    band≥1 から低頻度（例: 12 プロンプトに1回）で re-anchor する
```

- 「決定事項」の re-anchor（既存）とは**別トラック**にする——決定事項は増減するプロジェクト知識、
  anchor は「今このセッションの1つの仕事」という単一の不変な事実であり、性質が違う。
- 頻度は決定事項の re-anchor より疎でよい（anchor はセッション中ほぼ変化しないため、8プロンプトに
  1回は過剰）。既定 12 プロンプトに1回、`ctxrot.toml` で調整可能にする。
- lease が無い（PDO 単位に紐づかない自由なセッション）場合は何もしない（fail-soft・無音）。

### 4.4 スコープ逸脱検知: stuckguard の既存 PostToolUse を拡張

stuckguard は既に「編集したファイル名」をシグネチャの一部としてウィンドウに記録している
（oscillation 検出のため）。ここに3つ目の検出器を足す:

```
PostToolUse → stuckguard watch
  → (既存) repeat 検出
  → (既存) oscillation 検出
  → (新規) scope drift 検出:
    現在の session_id の live lease（overwatch 経由）から scope を読み、
    直近ウィンドウで編集されたファイルが scope に**1件もマッチしない**状態が
    drift_threshold 回（既定 3）続いたら advisory ナッジ:
    「このセッションは <scope> の担当のはずですが、直近の編集は <touched> でした。
      scope を広げる意図なら anchor を更新してください（`overwatch begin --key ... --scope ...`
      の再実行）。そうでなければ元のタスクに戻ってください。」
```

- **lease が無い / scope が空のセッションには発火しない**（fail-soft、false positive 回避）。
- repeat/oscillation と同じ「advisory のみ・ブロックしない」方針を継承。stuckguard の既存
  クールダウン・エスカレーション機構（`escalate_after`）もそのまま使い回せる。
- overwatch バイナリが無い環境では黙ってスキップ（既存の `condukt`/`fugu-router` 欠落時の
  fail-soft パターンと同じ）。

### 4.5 並行セッションの衝突防止（新規、cross-run）

`overwatch begin` 時、既存の「同一 key の live lease があれば reject」（dedup 契約）に加えて、
**scope の重なりチェック**を追加する:

```
overwatch begin --key K --scope S
  → (既存) key=K の live lease があれば exit 1（今まで通り）
  → (新規) key≠K だが scope が S と重なる live lease があれば、
    exit 0 はするが warning フィールドを JSON に含める
    （blocking にはしない——scope の重なりは「同じファイルを触る2つの別タスク」であって
      必ずしも悪ではない。condukt に渡せば Phase 3.5 の conflict-check がより厳密に判定する）
```

blocking にしない理由: overwatch 単体では「本当に衝突するか（同じ関数を触るのか、単に同じ
ファイルの別セクションか）」まで判定できない。ここは condukt の conflict-check（Phase 3.5）に
判定を譲り、overwatch は**早期警告**（「あなたが着手しようとしている scope は、別セッションが
既に触っている可能性があります」）に徹する。二層防御: overwatch = 早期・粗い警告、
condukt conflict-check = run 開始直前の厳密な判定。

### 4.6 誤投入・奪取・多重実行への対応

既存機構の棚卸し（§2）で判明した3つの残存ギャップへの対応。いずれも新規ストア・新規フックを
足さず、既存の一箇所（`overwatch begin`／既存 PostToolUse／既存 `reconcile`）に機能を足すだけに
とどめる。

#### (a) ユーザーが誤って処理中タスクを再投入する

`backlog add` は既に content hashkey（title 正規化＋project の FNV-1a）で完全一致の重複投入を
拒否し、`condukt state is-claimed --hashkey` で他セッションの live claim も検出している
（[backlog README](../crates/backlog/README.ja.md) 「重複タスクの拒否」節）。**この経路は
本仕様の変更なしで既に機能する。**

残るギャップは2つ: ① 言い回しが違う近似重複（hashkey が正規化後も完全一致しないと素通りする）、
② `backlog add` を経由しない自由文での直接指示（このチェック自体が発動しない）。

**対応**: §4.2 で「flow がどの source を選んでも `overwatch begin` を必ず呼ぶ」設計にした
ことを利用し、**この一箇所**に fuzzy 近似重複チェックを足す。`harness_core::lessons` が既に
持つ Jaccard トークン類似度（`tokenize`/`jaccard`、§2 で言及した stuckguard のレッスン検索と
同じ実装）を再利用し、新規 lease の `title`/`done_criteria` を他の live lease 全件と比較する。
閾値（既定 0.6）を超える類似があれば、`begin` の戻り値 JSON に `possible_duplicate:
[{key, title, similarity}]` を追加する（**advisory、blocking しない**——類似度だけで別タスクを
拒否するのは false positive リスクが高い）。呼び出し元（flow skill）はこれを見てユーザーに
一言確認する。backlog 経由・自由文経由のどちらでも同じ一箇所を通るため、経路を問わず効く。

#### (b) 実行中に他セッションのタスクを奪ってしまう

`condukt state claim-task --hashkey` は他 run の live claim があれば hard-skip する。しかし
liveness 判定の heartbeat は **flow の外側ループが次のタスクをピックする直前
（Step 3-4）にしか呼ばれない**（[flow SKILL.md](../crates/flow/skills/flow/SKILL.md) 305 行目
付近）。**1つのタスクの実行中（長いビルド・テストなど）は heartbeat が更新されない**ため、
TTL（30分、`condukt state abandon --all-stuck` と overwatch の `LEASE_TTL_SECS` が共通して
使う値）を超える作業を1タスク内で行っていると、まだ生きているセッションの claim が「死んだ」と
誤判定され、`reap`/`abandon --all-stuck` で解放→**別セッションに奪われうる**。ハートビート頻度が
実際の作業粒度と合っていない、典型的な false-death 検出のバグパターン。

**対応**: heartbeat を「flow の外側ループのたび」だけでなく「**PostToolUse のたびに**」も
発行する。新しいフックは足さず、stuckguard が既に PostToolUse で毎ツール呼び出しごとに
session_id を解決している処理に便乗させる（`stuckguard watch` の既存ステップに
`condukt state heartbeat --run <resolved-run-id>` / `overwatch heartbeat --key <resolved-key>`
の呼び出しを1行足すだけ）。これでタスク実行中も claim が生き続け、TTL 超過による誤 reap が
実質的に起きなくなる。`condukt`/`overwatch` いずれかが無い環境では fail-soft にスキップ
（stuckguard の既存方針を継承）。

#### (c) 奪われたタスクを重複して実行してしまう

(b) を修正すれば理論上の発生頻度はほぼゼロに近づくが、レース条件（クロック skew・ユーザーが
`--force` で意図的に二重着手・reap と再 claim が僅差で競合）の最後の砦として、検出だけは持たせる。

現状 `condukt state reconcile --run <rid>` は「branch がマージ済み/削除済みのタスクを自動
verified に昇格」するだけで、**同一 hashkey を複数 run_id が両方 done/verified にした状態は
検出しない**。

**対応**: `reconcile` に、実行対象 run とは別に「同一 hashkey を持つ他の run で、この run の
`claimed_at` より後に done/verified になったタスクが無いか」を横断的に確認するステップを足す。
見つかった場合は自動マージ・自動破棄のどちらもせず、`{"duplicate_completion": [{hashkey, runs:
[run_id...]}]}` を出力して **exit code で異常系（例: exit 3）を返し、人間の選択（HOTL）を
要求する**——`condukt`/`propguard` が既に持つ「fail-closed でブロックし人間に選ばせる」設計
方針をそのまま流用する。自動解決しない理由: どちらの実装を残すべきかは実行結果（テスト・diff の
質）を見ないと判断できず、機械的な優先順位付け（先着順など）は往々にして間違った方を残す。

---

## 5. クレート別の変更内容

### 5.1 overwatch

- `Lease` に `scope: Vec<String>`（既定 `[]`）・`done_criteria: Option<String>`（既定 `None`）を
  `#[serde(default)]` で追加。
- `begin` に `--scope <csv>` `--done-criteria <text>` オプションを追加（両方省略可、既存呼び出しは
  無変更で動く）。
- 新規 `overwatch lease --session <id> [--json]` — 指定 session の live lease を1件返す
  （§4.3 の re-anchor 読み取り用）。
- `begin` の戻り値 JSON に `scope_overlap: [{key, title, scope}]`（重なった live lease の一覧、
  空配列が既定）を追加（§4.5）。既存の exit code 契約（0=成功/1=同key保持中）は変更しない。
- `begin` の戻り値 JSON に `possible_duplicate: [{key, title, similarity}]`（既定空配列）を追加
  （§4.6a）。`harness_core::lessons::{tokenize, jaccard}` を再利用し、新規 lease の
  `title`/`done_criteria` と他の live lease 全件を比較。閾値は `possible_duplicate_threshold`
  （既定 0.6、config で調整可）。exit code は変更しない（advisory）。

### 新規: heartbeat の PostToolUse 便乗（§4.6b、実装は stuckguard 側）

overwatch 自体の変更は無し。`overwatch heartbeat --key <k>` は既存コマンドをそのまま stuckguard
から呼ぶだけ（§5.3）。

### 5.2 flow

- Step 3-1（ピック）の直後、選んだ source 種別によらず `overwatch begin` を呼ぶよう
  `skills/flow/SKILL.md` を更新（§4.2）。`overwatch` バイナリが無ければ今まで通り skip
  （fail-soft、既存方針を継承）。
- Step 4（ロック解放・sink）で対応する `overwatch end --key <k> --status <s>` を呼び、
  anchor のライフサイクルを閉じる。

### 5.3 stuckguard

- 検出器を1つ追加（§4.4）。`stuckguard.toml` に `scope_drift_enabled`（既定 false——まずは
  opt-in で様子を見る）・`drift_threshold`（既定 3）を追加。
- overwatch 呼び出しは既存の「他バイナリ欠落時 fail-soft」パターンを踏襲（新規依存だが
  ハードな依存にはしない）。
- **新規（§4.6b）**: `watch` の既存ステップ（signature 作成→リングバッファ追記→検出器実行）の
  直後に、`condukt state heartbeat --run <resolved-run-id>` と `overwatch heartbeat --key
  <resolved-key>` を1回ずつ呼ぶ（`run-id`/`key` は overwatch lease から現在 session の
  ものを解決。無ければ何もしない）。`heartbeat_piggyback_enabled`（既定 true——これは
  「奪取防止」という安全側の機能なので scope drift とは逆に既定 on にする）で無効化可能。
  `condukt`/`overwatch` 欠落時は fail-soft でスキップ（ナッジ自体はブロックされない）。

### 5.4 ctxrot

- `guard` フックに anchor re-injection 経路を追加（§4.3）。`ctxrot.toml` に
  `anchor_reinject_every`（既定 12）を追加。

### 5.5 condukt

- **新規（§4.6c）**: `state reconcile --run <rid>` に、指定 run の hashkey 群それぞれについて
  「他の run_id が同じ hashkey を、この run の `claimed_at` より後に done/verified にしていないか」
  を横断確認するステップを追加。見つかったら `{"duplicate_completion": [{hashkey, runs:
  [run_id...]}]}` を出力し `exit 3`（既存の needs-human エスカレーション exit code の慣例
  ——`trajectoryeval tier` の fuzzy 閾値超過ドリフトと同じ扱い——を踏襲）。自動マージ・自動破棄は
  行わない。既存の「branch merge/削除済み → 自動 verified 昇格」パスは変更しない（重複が無い
  通常ケースは今まで通り）。

---

## 6. 非目標

- **チーム間共有（git-native Space）。** [DESIGN-pdo-space.md](DESIGN-pdo-space.md) に記述済みで、
  優先度を下げて凍結。本仕様が実装され運用が安定した後、必要になれば再検討する。
- **AI-DLC の stage/scope ワークフロー構造そのものの採用。** 32 ステージ×9 スコープのような
  重い枠組みを輸入するかどうかは未決（「どちらでもいい」）。本仕様はその枠組みに依存しない
  ——anchor は condukt/flow/hypothesis の既存語彙（`touched_files`/`done_criteria`）だけで表現する。
- **scope drift 検知を fail-closed（Stop ブロック）にすること。** advisory から始め、
  誤検知率を見てから判断する（§9）。
- **overwatch の scope 重なりチェックを厳密な衝突判定にすること。** それは condukt の
  conflict-check の役割のまま（§4.5）。overwatch は早期警告のみ。

---

## 7. 受け入れ基準（done_criteria）

- [ ] `overwatch begin --scope`/`--done-criteria` を省略した既存呼び出しが、既存のテスト・
      既存の JSON 出力形状のまま**一切変わらず**動く（回帰テスト）。
- [ ] `flow` が measure step（condukt run を起こさない経路）をピックしたときも、
      `overwatch begin` が呼ばれ、`overwatch status` にその作業が反映される
      （condukt run 無しでも anchor が立つことの確認）。
- [ ] 12 プロンプト分の会話をシミュレートし、live lease がある session では ctxrot の
      re-anchor に anchor テキストが含まれる（band 条件を満たす回で）。
- [ ] scope drift シナリオ: `scope=["src/a.rs"]` の lease を持つセッションで `src/b.rs` を
      3回連続編集すると、`scope_drift_enabled=true` のとき stuckguard が advisory ナッジを出す。
      `scope_drift_enabled=false`（既定）では出ない。
- [ ] `overwatch begin` で scope が重なる別 key の live lease がある場合、exit code は
      0 のまま（blocking しない）で `scope_overlap` に該当 lease が列挙される。
- [ ] overwatch/stuckguard/ctxrot いずれかのバイナリが欠落していても、他は今まで通り動作し
      turn を壊さない。
- [ ] （§4.6a）title/done_criteria が類似（Jaccard ≥ 0.6）する2つ目の `overwatch begin` を、
      1つ目が live な間に呼ぶと `possible_duplicate` にヒットが載る。exit code は 0 のまま
      （blocking しない）。閾値未満の非類似タスクではヒットしない。
- [ ] （§4.6b）1タスクの実行を模した長時間シナリオ（flow の外側ループを回さずツール呼び出しだけ
      TTL 超過分繰り返す）で、stuckguard 経由の heartbeat 便乗により claim/lease が stale
      判定されない（`heartbeat_piggyback_enabled=true` のとき）。無効化時は从来どおり TTL 超過で
      stale になることも合わせて確認する（回帰）。
- [ ] （§4.6c）同一 hashkey を2つの異なる run_id が done/verified にした状態を用意し、
      `condukt state reconcile` が `duplicate_completion` を報告して exit 3 で終わる（どちらの
      run も自動では変更されない）。重複が無い通常ケースでは従来どおり exit 0 で自動昇格する
      （回帰）。
- [ ] 変更した各クレート（overwatch / flow / stuckguard / ctxrot / condukt）の version を
      lockstep bump し、`check-plugin-versions.py` / `check-version-bumped.py` を green にする。

---

## 8. 段階的ロールアウト

1. **Phase 1 — overwatch の scope/done_criteria 拡張のみ**（後方互換フィールド追加、CLI
   オプション追加）。他クレートはまだ呼ばない。
2. **Phase 2 — flow が anchor を登録**（§4.2）。この時点で `overwatch status` を見れば
   「今どのセッションが何を担当しているか」が可視化される（記憶喪失対策の土台が動き出す）。
3. **Phase 3 — ctxrot re-anchor 統合**（§4.3）。記憶喪失対策が実際にセッション内で効き始める。
4. **Phase 4 — stuckguard scope drift 検出**（§4.4、既定 opt-in）。しばらく運用し誤検知率を
   観測してから既定 on への切り替えを検討する。
5. **Phase 5（任意）— overwatch scope overlap 早期警告**（§4.5）。
6. **Phase 6 — stuckguard heartbeat 便乗**（§4.6b、既定 on）。奪取防止は安全側の機能なので
   他の advisory 機能より先に有効化してよい。優先度としては Phase 2 の直後に前倒ししても構わない
   （anchor の登録が無いと heartbeat 対象の key/run-id が解決できないため Phase 2 には依存する）。
7. **Phase 7 — overwatch fuzzy 近似重複検知**（§4.6a）。
8. **Phase 8 — condukt reconcile の多重完了検出**（§4.6c）。単独で実装・テスト可能なため、
   他 phase と独立に着手してよい。

各 phase は独立に PR 化。GATE_CRATES ではないため `--canary` 不要。

---

## 9. リスク・トレードオフ

- **anchor re-injection が context 予算を圧迫する** — `inject_limit` 相当の上限を設け、
  anchor テキストは title + done_criteria の要約1〜2行に絞る（scope の生 glob 列は注入しない）。
- **scope drift の誤検知（正当なスコープ拡大を drift と誤認）** — advisory のみに留め、
  「scope を更新してください」という逃げ道を常に提示することで実害を「1行の注意」に限定する
  （§4.4）。既定 opt-in（`scope_drift_enabled=false`）でまず運用実績を積む。
- **overwatch への新規依存が増えるクレート（stuckguard/ctxrot/flow）が増える** — いずれも
  「overwatch 欠落時は fail-soft でスキップ」という既存パターン（flow の `backlog`/`condukt`
  欠落時と同じ）を踏襲するため、overwatch 単体プラグインとして未導入の環境でも他機能は壊れない。
