---
name: continuous-audit
description: gate crates に対する敵対的レビュー1ラウンド(finder→refute ベース verifier→CONFIRMED subset)を回し、確認された指摘だけを overwatch review-queue に自動記録する Continuous-Audit driver。round metrics を audit ledger に残し収束(round 越しの new-findings 減少)を追跡する。opt-in・fail-soft。
argument-hint: [round-id] [--target crate1,crate2] [--dry-run]
allowed-tools: Task, Bash(scripts/continuous-audit.sh:*), Bash(overwatch:*), Bash(git:*), Read, Grep, Glob
---

# /continuous-audit — Continuous-Audit ループ 1 ラウンド駆動

gate crates への **敵対的レビュー 1 ラウンド**を回し、確認された指摘 (CONFIRMED findings) だけを
`overwatch review-queue` に自動供給する。決定論の記録は `scripts/continuous-audit.sh`(+ overwatch
バイナリ) が担い、この skill は **finder→verifier の意味判断**を担う。

```
finder (提案)  →  refute-verifier (反証で篩う)  →  CONFIRMED subset
                                                        │
                        scripts/continuous-audit.sh --finding …  ← 決定論 record
                                                        ▼
                        overwatch review-queue  (finding-id で dedup 済み)
```

**役割分担 (外さない)**: 指摘の発見・反証・確定という**意味判断は LLM (この skill)**、finding の永続化・
round ledger・収束メトリクスという**決定論は `scripts/continuous-audit.sh` と overwatch バイナリ**。
この skill は新しい状態を持たず、既存の決定論レコーダを駆動するだけ。

## いつ使うか

- gate crates (fleet 防御ゲート) を**定期的に**敵対レビューし、退行や見落としを掘り起こしたいとき。
- `docs/review-redesign-implementation-items.md`「継続運用の原則」の運用面 —
  「決定性はテストに固定化し、非決定性は発見エンジンとして分離する」— を 1 ラウンド回す入口。
- **opt-in**: どの always-on ゲートにも配線されておらず、condukt/rollout の挙動を変えない。呼んだときだけ走る。

## 前提

- `scripts/continuous-audit.sh` が存在する (リポジトリ同梱の決定論レコーダ)。無ければ導入を案内して停止。
- `overwatch` バイナリがビルド済み (`cargo build -p overwatch` か live cache)。スクリプトが自動解決する
  (`OVERWATCH_BIN` / PATH / `target/{release,debug}`)。見つからなければスクリプトが fail-soft で何も記録せず戻る。

## 対象 crate (既定)

既定の target は fleet の **GATE crates**: `blastguard,propguard,specguard,stuckguard,mutategate`
(`scripts/rollout-plugins.sh` の GATE_CRATES と同期)。`--target` で上書きできる。

## 手順

### Step 0 — 引数

- `round-id`: このラウンドの識別子 (例: `2026W28`、連番、日付)。省略時は `--dry-run` を促すか、`git rev-parse --short HEAD` 等で導く。
- `--target <csv>`: 対象 crate を上書き (既定は上記 GATE crates)。
- `--dry-run`: 記録せずに計画だけ表示 (finder/verifier は回してよいが record はしない)。

### Step 1 — finder (敵対的発見)

対象 crate ごとに **`Task` で finder subagent を起動**する (read-only。並列でよい)。各 finder に、
その crate の `src/` を敵対的に読み、**実在し根拠のある**バグ/退行/見落とし (境界条件・fail-open・
無音化・near-repeat の window 依存・polarity バイパス等) を **逐語引用 (file:line) つき**で列挙させる。

- finder は**修正しない** (read-only)。指摘は `{severity: high|med|low, summary, file:line}` の形で返させる。
- 「実在する問題だけ」を強制する (推測・スタイル論・LGTM の水増しを避ける)。
- 見つからなければ空を返させる (0 件は正常 = 収束の証拠)。
- **finder で使用するモデルを記録する** — Step 2 の model diversity チェックで必要。Task の実行結果から使用モデルを確認し、メモに記録しておく。

### Step 2 — refute-verifier (反証で篩う)

finder の各指摘を、**別の (finder とは独立した) `Task` verifier subagent**に渡し、
**反証を試みさせる** (adversarial verify)。verifier には「既定は REFUTED。コード上の根拠 (file:line) で
`CONFIRMED` を積極的に立証できたものだけ残す」と指示する。

- **CONFIRMED**: verifier がコードで再現/立証できた指摘。→ review-queue に載せる。
- **REFUTED / PLAUSIBLE**: 立証できない・誤検出・文脈で無害。→ 捨てる (載せない)。
- **finder と verifier は必ず別 subagent を使用し、かつ異なるモデルを指定すること (MUST)**。
  同一モデルペアだと生成と検証が同じ盲点を共有するため、必ず異なるモデルで実行する。
  - 具体的な model diversity ルール：
    - finder が `claude-3-5-sonnet` を使用した場合 → verifier は `claude-3-5-opus` または `claude-3-5-haiku` を指定。
    - finder が `claude-3-5-opus` を使用した場合 → verifier は `claude-3-5-sonnet` または `claude-3-5-haiku` を指定。
    - finder が `claude-3-5-haiku` を使用した場合 → verifier は `claude-3-5-sonnet` または `claude-3-5-opus` を指定。
  - verifier の Task 起動時に、**finder で記録したモデルと異なるモデルを明示的に指定する**。
    指定後、実際に同じモデルペアになっていないことを確認してから verifier を実行する。
  - 同一モデルペアの実行は防止する（分析品質低下・盲点共有のため）。
  - **この MUST は prose だけでなくコードで機械強制される**: Step 3 で finder/verifier のモデルを
    `--finder-model` / `--verifier-model` として渡すと、overwatch が決定論的に `same_model` 判定を行い、
    **同一モデルなら review-queue に high severity の警告 finding を1件記録する**（finding-id は round-id 由来で
    冪等）。ハード fail ではなく fail-soft（round は記録され続け、ループは止まらない＝never-break-a-turn）だが、
    review surface に MUST 違反が可視化される。**必ず両モデルを渡して自己申告を機械検証に晒すこと**。
- 高リスク指摘は verifier を複数立てて多数決にしてよい。その場合も複数の verifier は異なるモデルを指定する。

CONFIRMED subset を確定し、各件を `finding-id | severity | summary | file` に整形する。**finding-id は
安定なキー**にする (例: `CA-<crate>-<連番>` や rule id)。同じ指摘が次ラウンドでも CONFIRMED なら
**同じ finding-id を再利用**する — review-queue は finding-id で dedup するので、重複行にはならず最新状態に畳まれる。

### Step 3 — 決定論レコーダで記録

CONFIRMED subset と round metrics を `scripts/continuous-audit.sh` に渡す。**引数契約は scaffold と厳密に一致**:

```sh
# 記録 (実書き込み):
scripts/continuous-audit.sh --round <round-id> --target <csv> \
  --new-findings <このラウンドで新規に出た件数> \
  --confirmed <CONFIRMED 件数> \
  --regression-tests-added <確定指摘を回帰テスト化した件数> \
  --finder-model <finder が使ったモデル> --verifier-model <verifier が使ったモデル> \
  --finding '<id>|<severity>|<summary>|<file>'   # CONFIRMED ごとに1回。--finding は繰り返し可

# プレビュー (何も書かない):
scripts/continuous-audit.sh --round <round-id> --dry-run
```

- `--finding` の書式は `id|severity|summary|file` (severity と file は省略可)。CONFIRMED の数だけ繰り返す。
- `--finder-model` / `--verifier-model` は Step 2 の model diversity MUST の**機械強制**入力。両方渡すと
  overwatch が `same_model` で決定論判定し、同一なら review-queue に high の警告 finding を冪等記録する
  (fail-soft・round は記録継続)。両省略時は従来どおりチェックしない（後方互換）が、**MUST を実効化するため
  常に両モデルを渡すこと**。
- `--new-findings` / `--confirmed` / `--regression-tests-added` は **round ledger にそのまま記録される件数**。
  `--finding` エントリは review-queue に流す CONFIRMED subset (件数入力とは独立)。
- スクリプトは各 CONFIRMED を `overwatch record-finding --source continuous-audit …` で review-queue へ、
  ラウンドを `overwatch audit-round record …` で収束 ledger へ追記し、最後に `overwatch audit-metrics` を印字する。
- **fail-soft**: overwatch 呼び出しが失敗してもループは止まらない (never-break-a-turn 不変)。

### Step 4 — 回帰テスト化 (継続運用の原則)

CONFIRMED のうち**挙動バグ**は、対象 crate に**回帰テストを追加して固定**する (決定性はテストに固定化する)。
これは別タスク (backlog / condukt) に委譲してよいが、追加できた件数を Step 3 の
`--regression-tests-added` に反映する。commit `38f613c` (re-review finding 1-3 の ignored 回帰テスト昇格) が
「CONFIRMED → 回帰テスト」の POC。

### Step 5 — 結果確認と収束

```sh
overwatch review-queue            # 記録した CONFIRMED が [ai-finding] タグで最新順に並ぶ (finding-id で dedup 済み)
overwatch audit-metrics           # round 越しの new-findings 推移・converging フラグ
```

- CONFIRMED は `overwatch review-queue` に `[ai-finding]` 行として現れる (systemic / rollback と統合表示)。
- **収束の読み方**: ラウンドを重ねて `new-findings` が下降トレンドなら健全 (掘り尽くしに近づく)。
  下がらない/増える場合はゲートに構造的欠陥が残っている合図。
- この ONE は「1 ラウンドの実接続と自動供給」まで。収束の longitudinal 実証はラウンドを蓄積してからの
  measure step (データが要る)。

## 自動化テンプレート (opt-in・この skill は何も自動インストールしない)

`scripts/continuous-audit.sh` 冒頭と `scripts/continuous-audit.cron.example` に、週次 cron / git pre-push
advisory のテンプレートがある。導入は手動 (コピー) で、いずれも fail-soft (push を止めない)。

## 失敗モード

- `scripts/continuous-audit.sh` 不在 → リポジトリ同梱物。パスを確認し導入を案内。
- `overwatch` バイナリ不在 → `cargo build -p overwatch`。スクリプトは fail-soft で何も記録せず戻る。
- finder が 0 件 → 正常 (収束の証拠)。`--new-findings 0 --confirmed 0` で round だけ記録してよい。
- 同じ指摘が毎ラウンド出る → finding-id を固定していれば review-queue は 1 行に dedup する (重複ノイズにならない)。
