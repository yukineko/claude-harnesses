---
name: continuous-audit
description: gate crates に対する敵対的レビュー1ラウンド(finder→refute ベース verifier→CONFIRMED/REFUTED/UNVERIFIED の三値判定)を回し、CONFIRMED を overwatch review-queue と backlog へ、UNVERIFIED を再検証待ちとして queue にだけ記録する Continuous-Audit driver。round metrics を audit ledger に残し収束(round 越しの new-findings 減少)を追跡する。opt-in・fail-soft。
argument-hint: [round-id] [--target crate1,crate2] [--dry-run]
allowed-tools: Task, Bash(scripts/continuous-audit.sh:*), Bash(overwatch:*), Bash(git:*), Read, Grep, Glob
---

# /continuous-audit — Continuous-Audit ループ 1 ラウンド駆動

gate crates への **敵対的レビュー 1 ラウンド**を回し、確認された指摘 (CONFIRMED findings) を
`overwatch review-queue` と backlog に、**判定不能な指摘 (UNVERIFIED)** を review-queue に
「再検証待ち」として自動供給する。決定論の記録は `scripts/continuous-audit.sh`(+ overwatch
バイナリ) が担い、この skill は **finder→verifier の意味判断**を担う。

```
finder (提案)  →  refute-verifier (反証で篩う)  →  verdict 三値
                                                    ├─ CONFIRMED  → --finding             → queue + backlog
                                                    ├─ UNVERIFIED → --unverified-finding  → queue のみ (pending)
                                                    └─ REFUTED    → 記録しない (立証責任を満たした場合のみ)
                                                        │
                        scripts/continuous-audit.sh …  ← 決定論 record
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

既定の target は fleet の **GATE crates** (blastguard/propguard/specguard/stuckguard/mutategate/overwatch =
`scripts/rollout-plugins.sh` の GATE_CRATES と同期。overwatch はこのループ自身が依存するバイナリであり、
canary health-gate もこれに依存するため、保護対象のクレートと同じ監査対象に含まれる) に加えて **backlog**
(audit 対象のみの追加。backlog は積みっぱなしのタスクが腐らないよう定期監査したいが、危険な操作を
gate/block しないため GATE crate ではない＝canary 必須にはならない)。既定 target の完全な一覧:
`blastguard,propguard,specguard,stuckguard,mutategate,overwatch,backlog`
(`scripts/continuous-audit.sh` の DEFAULT_TARGETS と同期)。`--target` で上書きできる。

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
**反証を試みさせる** (adversarial verify)。

> **既定は REFUTED ではない。既定は UNVERIFIED。**
> 二値 (CONFIRMED/REFUTED) + 「既定 REFUTED」は、**「permissive な経路を辿れなかった」を
> 「経路が無い」に潰す**。これは監査対象のゲートが持つ fail-open と全く同じ形であり、実測で
> 実在する fail-open を誤って棄却した (2026-07-21: specguard `forge/gather.rs` の指摘を REFUTED と
> したが、検証者は shortfall→sentinel の 1 経路しか辿っておらず、件数閾値を満たす部分的な束が
> clean として下流へ渡る経路を見ていなかった。後に `EXIT_INTAKE_INCOMPLETE=8` を追加する修正が
> 入り、指摘が実在したことが示された)。**判定不能は制限側 = UNVERIFIED に倒す。**

**verdict は三値 (CONFIRMED / REFUTED / UNVERIFIED)**。verifier には次を指示する:

- **CONFIRMED**: verifier がコードで再現/立証できた指摘。→ review-queue に載せ、backlog へ流す。
  - 立証には該当箇所の **file:line と逐語引用**、および「その状態が下流でどう permissive に作用するか」の
    経路を 1 本以上示すこと。
- **REFUTED**: 指摘が実在しないことを**積極的に立証できた**ときだけ。**立証責任は反証側にもある**:
  - **その値/状態の消費者 (consumption path) を全列挙**すること。「grep した」ではなく、
    **各消費経路について file:line + 逐語引用**を出す。1 本でも辿れていない経路があれば REFUTED にしてはならない。
  - 各経路について「この経路では permissive にならない」根拠を、**引用したコードだけから**述べること
    (「〜のはず」「設計上そうなっていない」は不可)。
  - 「permissive な到達路を示せなかった」は REFUTED **ではない** (それは UNVERIFIED)。
  - 消費者が別 binary/crate にコンパイルされる、呼び出しが動的、という理由で追跡を打ち切った場合も UNVERIFIED。
- **UNVERIFIED**: 上記いずれも満たさない = **判定不能**。立証も反証もできなかった。
  - **捨てない**。項目は pending のまま残す (done にも失敗にもしない)。
  - CONFIRMED と同じ扱い (backlog への自動起票 = 対応済みの作業として流す) にもしない。
  - 記録先は Step 3 の `--unverified-finding`。review-queue には `[UNVERIFIED]` マークつきで並び、
    `--to-backlog` では**流れない** (再検証待ちとして可視化されたまま残る)。
  - verifier には「何が確認できて、何が確認できなかったか (辿れなかった経路の名前)」を rationale に書かせる。
- **PLAUSIBLE** という語は使わない (UNVERIFIED に統合された)。
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
- verifier が **CONFIRMED** と判定した際は、その根拠 (file:line 引用込み・反証を退けた理由) を
  `rationale` として保持しておく。Step 3 で `overwatch record-finding` の `--rationale` オプション
  (省略可) にそのまま渡す。省略しても記録自体は成功する (後方互換) が、渡せる場合は必ず渡す。
- **UNVERIFIED** と判定した際も同様に `rationale` を残す。内容は「辿れた経路」と**「辿れなかった経路」**の
  両方 (何が未確認のまま残っているか) を書く。次ラウンドの再検証はここから再開する。
- **REFUTED** と判定した際は、消費経路の全列挙と各経路の逐語引用を verifier の出力に残す。列挙できない/
  引用できない場合は REFUTED を名乗らせない (UNVERIFIED に落とす)。

CONFIRMED subset と UNVERIFIED subset をそれぞれ確定し、各件を `finding-id | severity | summary | file` に
整形する。**finding-id は安定なキー**にする (例: `CA-<crate>-<連番>` や rule id)。同じ指摘が次ラウンドでも
CONFIRMED なら**同じ finding-id を再利用**する — review-queue は finding-id で dedup するので、重複行には
ならず最新状態に畳まれる。UNVERIFIED も同じ id を使い回すこと (次ラウンドで CONFIRMED に昇格したとき、
同じ id で上書きされ 1 行に畳まれる)。

### Step 3 — 決定論レコーダで記録

CONFIRMED subset と round metrics を `scripts/continuous-audit.sh` に渡す。**引数契約は scaffold と厳密に一致**:

```sh
# 記録 (実書き込み):
scripts/continuous-audit.sh --round <round-id> --target <csv> \
  --new-findings <このラウンドで新規に出た件数> \
  --confirmed <CONFIRMED 件数> \
  --unverified <UNVERIFIED 件数> \
  --regression-tests-added <確定指摘を回帰テスト化した件数> \
  --finder-model <finder が使ったモデル> --verifier-model <verifier が使ったモデル> \
  --finding '<id>|<severity>|<summary>|<file>|<rationale>' \            # CONFIRMED ごとに1回 (繰り返し可)
  --unverified-finding '<id>|<severity>|<summary>|<file>|<rationale>'   # UNVERIFIED ごとに1回 (繰り返し可)

# プレビュー (何も書かない):
scripts/continuous-audit.sh --round <round-id> --dry-run
```

- `--finding` / `--unverified-finding` の書式は同じ `id|severity|summary|file|rationale`
  (severity / file / rationale は省略可)。それぞれ CONFIRMED / UNVERIFIED の数だけ繰り返す。
- **verdict は record 時に記録される**: `--finding` は `overwatch record-finding --verdict confirmed`、
  `--unverified-finding` は `--verdict unverified` として書かれる。overwatch 側では
  **未知の verdict 値は `unverified` に倒れる** (判定不能は制限側。silently confirmed にはならず、
  行が捨てられることもない)。**REFUTED はスクリプトに渡さない** (載せない)。
- **UNVERIFIED の下流での扱い** (ここが二値との差):
  - `overwatch review-queue` に `[UNVERIFIED]` マークつきで並ぶ = **捨てられない**。
  - `overwatch review-queue --to-backlog` では**流れない** = CONFIRMED のように「対応中の作業」に
    昇格しない。pending のまま再検証を待つ。
  - `--confirmed` 件数には含めない (closure-rate の分母は CONFIRMED のみ)。
- `--finder-model` / `--verifier-model` は Step 2 の model diversity MUST の**機械強制**入力。両方渡すと
  overwatch が `same_model` で決定論判定し、同一なら review-queue に high の警告 finding を冪等記録する
  (fail-soft・round は記録継続)。両省略時は従来どおりチェックしない（後方互換）が、**MUST を実効化するため
  常に両モデルを渡すこと**。
- `--new-findings` / `--confirmed` / `--unverified` / `--regression-tests-added` は
  **round ledger にそのまま記録される件数**。`--finding` / `--unverified-finding` エントリは
  review-queue に流す subset (件数入力とは独立)。`--unverified` を別枠で持つのは、
  `new_findings - confirmed` を「残りは全部 refuted」と読ませないため (未決は未決として残す)。
- スクリプトは各 finding を `overwatch record-finding --source continuous-audit --verdict … ` で review-queue へ、
  ラウンドを `overwatch audit-round record …` で収束 ledger へ追記し、最後に `overwatch audit-metrics` を印字する。
- **fail-soft**: overwatch 呼び出しが失敗してもループは止まらない (never-break-a-turn 不変)。

### Step 4 — 回帰テスト化 (継続運用の原則)

CONFIRMED のうち**挙動バグ**は、対象 crate に**回帰テストを追加して固定**する (決定性はテストに固定化する)。
これは別タスク (backlog / condukt) に委譲してよいが、追加できた件数を記録する。commit `38f613c`
(re-review finding 1-3 の ignored 回帰テスト昇格) が「CONFIRMED → 回帰テスト」の POC。

- **`#[ignore]` の理由文字列は必ず `<finding-id>: ` で始める** (構造化規約)。例:
  `#[ignore = "CA-overwatch-001: re-review finding 1, see docs/..."]`。
  従来は自由文言 (例: "...re-review finding 2") で運用していたが、finding-id と回帰テストを機械的に
  逆引きできるようにするため、今後はこの規約に統一する。

> **回帰テストは通常ラウンド記録より後に landed する** (修正は backlog/condukt へ委譲される別タスク)。
> Step 3 の `--regression-tests-added` は「そのラウンドと同時にテストまで締めた」件数だけを入れ、
> **後から締めた分は Step 4.5 の closure で round に還元する** (record 時は 0 のままにしてよい)。

### Step 4.5 — closure フィードバック (fix 側を収束シグナルに還元する)

CONFIRMED を回帰テストで締めたら、その件数を**元のラウンドへ closure として書き戻す**。これをやらないと
`audit-metrics` の `closure-rate` と `converging` は fix 側を見ないまま `0.00 / false` に張り付き、
「fleet は硬化しているか?」というループ本来の問いに答えられない (build ≠ validate)。

```sh
# ラウンド <id> の confirmed findings を締めた回帰テスト件数 <N> を還元する。
overwatch audit-round close --round <round-id> --tests <N>
```

- **SET(加算ではない)**: 同じ `<N>` で二度 close しても二重計上しない (冪等なので backfill を安全に再実行できる)。
- **closure ≤ 1.0 に保つ**: `<N>` は「回帰テストで固定した confirmed findings の数」であって raw なテスト関数の
  総数ではない (closure-rate = tests ÷ confirmed なので confirmed を超えると 1.0 を超えて不正になる)。
- **未知 round-id は fail-soft**: ledger を変えずに `closed:false` を返す (turn を壊さない)。
- 同じ round-id が重複記録されている場合は**最後に記録されたラウンド**が closure 対象になる。

### Step 5 — 結果確認と収束

```sh
overwatch review-queue            # 記録した finding が [ai-finding] タグで最新順に並ぶ (finding-id で dedup 済み)
                                  # UNVERIFIED は summary 先頭に [UNVERIFIED] が付き、--to-backlog では流れない
overwatch audit-metrics           # round 越しの new-findings 推移・converging フラグ
```

- CONFIRMED は `overwatch review-queue` に `[ai-finding]` 行として現れる (systemic / rollback と統合表示)。
- UNVERIFIED も同じく `[ai-finding]` 行として現れるが `[UNVERIFIED]` マークが付き、backlog には流れない。
  **UNVERIFIED が積み上がるのは異常ではなく情報**: 次ラウンドで優先的に再検証する対象を指している
  (`audit-metrics` の `cumulative unverified` で追える)。
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
