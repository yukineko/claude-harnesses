# harness — 全体概要

Claude Code 上で動く自作プラグイン群のモノレポ。「判断は LLM、決定論的な作業は Rust バイナリ」という分業を軸に、長いエージェントセッションを安全・継続的に動かすための基盤を提供する。

---

## なぜこのハーネスが存在するか

素の Claude Code は 1 セッション完結で動く。長いセッションでは:

- コンテキストが膨張して初期の指示や決定が埋もれる（context rot）
- 並列実装でファイルが競合する
- LLM が同じ失敗を繰り返す
- コストが見えない
- セッションをまたいで作業が引き継がれない

harness はこれらを **Claude Code の hooks と Rust バイナリと skill** の組み合わせで解決する。外部 API キー不要、Claude subscription の範囲内で完結する。

---

## 3 つの設計原則

**1. LLM と決定論の分離**  
LLM は「解釈・実装・判断」だけを担う。スケジューリング・状態管理・ゲート・worktree 管理は Rust バイナリが決定論的に処理する。LLM の非決定性が副作用を持つ範囲を最小化する。

**2. フック不変条件（never break a turn）**  
`harness-core::hook::run_hook` がパニックを捕捉して常に exit 0 で返す。どのプラグインのバグも Claude Code のターンを壊さない、という不変条件がコアライブラリに焼き込まれている。

**3. Subscription-native**  
外部 API キーを不要とし、既存の transcript / git diff / ローカルファイルを読む。メモリ上限を守るためトランスクリプトのフル読み込みは禁止し、常にストリーミング処理する。

---

## 共通基盤 — harness-core

全プラグインがビルド時に依存する共有ライブラリ。バイナリに静的リンクされるためランタイム依存はない。

| モジュール | 役割 |
|---|---|
| `hook.rs` | `HookInput` パース、`run_hook` 不変ラッパー |
| `store.rs` | 並列セッション安全な note ストア（session-tag 付きファイル名、fallback ロジック、prune/GC） |
| `transcript.rs` | JSONL トランスクリプトのストリーミング読み込み（全ロード禁止） |
| `pricing.rs` | Opus/Sonnet/Haiku/Fable の USD コスト推定テーブル |
| `install.rs` | `~/.claude/settings.json` の load/backup/write |
| `interrogate.rs` | ゴール/仕様精緻化の問答ループ（純粋関数） |
| `inject.rs` | コンテキスト注入プラグイン（playbook / run-book）の共有基盤 |

---

## プラグイン一覧

### オーケストレーション

#### condukt
課題を「解釈→タスク分割→合意→並列実装→検証→完了ゲート」の 1 サイクルで回す。

- **LLM が担う**: 解釈（interpreter agent）・実装（worker agent）・検証（verifier agent）
- **バイナリが担う**: ファイル衝突解析・parallel/serial スケジュール・git worktree 管理・state 追跡・完了ゲート
- **スキル**: `/condukt <課題>`
- **フック**: SessionStart（`condukt restore` — 未完 run/orphan worktree の警告）、Stop
- **consensus**: `condukt consensus`（multi-sample 自己整合投票。opt-in の fan-out 計画、または 1 タスクの N 個 verifier 判定を多数決＋Opus エスカレーション判断に集約）
- **autonomy switch**: `condukt state autonomy-check`（config `autonomous` + env `CONDUKT_AUTONOMOUS`）が自律モードを判定し、自律時は `/condukt` スキルが Phase 3 の合意など人間ゲートを縮退（exit code で分岐）する
- **correctness spine（実行オラクルで正しさを担保）**: condukt が生成する patch の「正しさ」を、プロセス指標（self-consistency 投票・coverage）ではなく**実行オラクル**で裏づける。keystone = **F→P 再現ゲート**（fix/feature は buggy→fixed の状態遷移をテストで証明。有効な Fail→Pass が無ければ `verified` 昇格を拒否）、その上に **edit-time compile ゲート**（壊れた編集を write 時に弾く）、さらに **best-of-N patch 選択**（parked）。「候補が合意した」ではなく「実行が緑」を done の条件にする。すべて fail-soft（判定不能時は従来の done_criteria ゲートに縮退し、ターンを壊さない）

#### fugu-router
過去のタスク実績（episodes.jsonl）から k-NN で「最安で検証を通過できるモデル」を選び、condukt の `suggested_model` を上書きする。haiku < sonnet < opus のティア順で cheap-first（最安優先）にバイアスし、cascade を安全網（失敗時に上位ティアへ段階的に引き上げる）として据える。Thompson sampling で未実績ティアも探索する。

- **スキル**: `/fugu-router`（condukt 内から自動呼び出し）
- **フック**: UserPromptSubmit（ルーティング記憶のサマリー注入）

#### compass
「今何をすべきか」を決める上流コンポーネント。北極星ゴールを彫り直し、現状との gap から condukt へ渡す「右サイズの一手」を選ぶ。

- **スキル**: `/compass`
- **フック**: SessionStart（目標リマインド注入）、Stop（breadcrumb 更新）

#### flow
**source → executor を 1 本のループで束ねる統合 driver（autopilot 層）**。課題の供給（compass の次の一手 / backlog のキュー / 直渡しの課題文）から解決手段の実行（condukt、fugu-router がモデル選択）までを貫く。判定（どの source を引くか・止め時）は LLM、状態維持・ロック・モデル選択は既存バイナリ（compass / backlog / condukt / fugu-router）が担い、flow 自身は新しい状態を持たない。`/backlog` の上位互換（compass ゲート＋複数 source を足したもの）で、backlog ロックを共有して直列化する。

- **スキル**: `/flow [課題文]`（引数があれば source 選択を飛ばして condukt に直行）
- **フック**: SessionStart（`flow propose` — 開いている仕事があれば `/flow` を propose-then-confirm で提案）
- **バイナリ**: 提案ディレクティブを注入するだけの薄いスキャフォールド。タスク数は再計算せず、compass / backlog / condukt が注入済みの state を束ねる

#### scout
**広域監査による施策生成 SOURCE**。プロジェクトを5レンズ（現在の課題 / セキュリティ / 業界・他プロジェクト標準 / 不足施策 / 安全性）で **read-only sub-agent を並列起動**して偵察し、逐語引用つきの施策候補を統合・重複排除・スコアリングして backlog に積み、`/flow` に実行を引き渡す。compass が「一手に絞る単一ゴールの勾配」なら、scout は「広く挙げて backlog に積む」相補的 source。判断（監査・施策選別）は LLM + sub-agent、保存は backlog、実行は flow/condukt。scout 自身は read-only で実装しない。

- **スキル**: `/scout [スコープ/レンズ絞り込み]`（`--dry-run` で提示のみ）
- **フック**: なし（スキルから呼ぶ）
- **バイナリ**: なし（状態は backlog が持つため skill のみ。業界標準レンズは WebSearch で裏付け）

---

### コンテキスト管理

#### ctxrot
長いセッションで context が膨張・腐敗するのを防ぐ。複数のサブコマンドが異なるタイミングで動く。

| サブコマンド | フック | 役割 |
|---|---|---|
| `guard` | UserPromptSubmit | 大きなファイル参照・context 使用率バンドを検出し警告注入 |
| `rescue` | PreCompact | `/compact` 直前に決定事項・残課題・触ったファイルを markdown ノートに保存 |
| `restore` | SessionStart | 前セッションのノートから決定事項・残課題を抽出して注入 |
| `preguard` | PreToolUse | 1MB 超の Read を禁止（sub-agent 経由を促す） |
| `toolguard` | PostToolUse | 巨大なツール出力を検知して警告 |
| `stop` | Stop | ctxrot 自前の budget メーターを基準に、閾値超過時に auto-compact を促すナッジを注入 |

- **スキル**: `/distill`（能動蒸留）、`/ctx`（load/pin/unload/use-note/list）
- ノートは `~/.ctxrot/store/` に Obsidian 互換 markdown として保存。`store_dir` を vault に向ければそのまま閲覧できる

#### context-governor
Claude Code 組込みコンパクションの薄い制御層。pin + lossless-recall + retrieval + tool-hygiene を単一 hook-dispatch バイナリに束ね、size（ウィンドウ占有）/ cost（再計算・キャッシュ）/ correctness（規範保存）の 3 軸を分離する。自前のログは書かず、既存の Claude Code コンパクションに介入する。

- **フック**: SessionStart、UserPromptSubmit、PreToolUse、PostToolUse、PreCompact、Stop、SubagentStop

#### playbook
プロジェクト固有の知識（`.playbook/*.md`）をプロンプトごとにキーワードスコアリングで関連ノートを自動注入する。

- **フック**: UserPromptSubmit

#### run-book
プロンプト中の `!name` を `.runbook/<name>.md` の内容に展開して注入する。繰り返し使う手順をマクロ化する。

- **フック**: UserPromptSubmit

#### taskprog
`.claude/progress.md` をセッション間でつなぎ、複数セッション跨ぎの進捗を管理する。

- **フック**: SessionStart（progress.md 注入）、SessionEnd（更新）
- **スキル**: `/taskprog`

---

### 検証ゲート（Stop フック群）

Stop フックはエージェントがターンを終えようとする前に実行される。FAIL なら停止を延長して修正を促す。

#### donegate
`build / test / lint` コマンドが全 green になるまで Stop をブロックする。設定したコマンドが失敗すると Claude に修正を求めて再試行させる。

#### reviewgate
コードレビューゲート。inject モード（Claude が自己レビュー）または subprocess モード（独立レビュアーを別プロセスで起動）で差分を評価し、基準を満たさなければ Stop をブロック。

#### precommit-audit
差分に対して静的監査を実行する。シークレット混入・例外の握りつぶし・禁止 API・巨大ファイル追加などを検査。

#### tdd
テストファースト強制ゲート。実装変更にテストが伴わない Stop をブロックする。`red/green/verify` のサブコマンドで TDD ライフサイクル（RED → GREEN → VERIFY）を証明させる。

- **スキル**: `/tdd`

#### propguard
プロパティゲート。Stop ごとにタスクの `done_criteria` から 3-5 個の**意味的不変条件（プロパティ）**を決定論的に導出し、成立数が `threshold` 未満なら **fail-closed** で Stop をブロックする。tdd が「具体テストが通るか」を見るのに対し、propguard は「不変条件を保つか」を見る相補ゲート（PGS / arXiv:2506.18315）。プロパティの導出と count→threshold ブロックはバイナリが担い、各プロパティの成否判定は inject モード（走行中のエージェント）または subprocess モードのチェッカーが行う。

#### mutategate
mutation-testing の kill-rate ゲート（cargo-mutants の `outcomes.json` を parse→kill-rate 算出→閾値未満で非ゼロ終了）。テストが「実際に fault を捕捉できるか」を測る。**プラグインではなくワークスペース内製ツール**（`plugin.json` なし・hook なし・CLI 専用）。

GATE_CRATES（blastguard / propguard / specguard / stuckguard / mutategate）を敵対的にレビューする
Continuous-Audit ラウンド（overwatch の `/continuous-audit`）は opt-in の別ループで、常時ゲートでは
ない。`git config core.hooksPath .githooks` を有効化していれば `.githooks/pre-push` が GATE_CRATES
配下の変更 push を検知し `scripts/continuous-audit.sh --dry-run` を advisory で勧める（fail-soft、
push は止めない）。cron 定期実行の雛形は `scripts/continuous-audit.cron.example`。

#### budgetguard
セッション・日次のコスト上限を設定し、超過すると Stop をブロックする。gauge の記録を読んでコストを計算する。

---

### 仕様管理

#### specguard
仕様（正典ドキュメント）と実装のドリフトを監査する。shard に分割して read-only subagent が逐語引用つきで差異を報告する。

- **スキル**: `/specguard:run`（監査実行）、`/specguard:brief`（着手前ブリーフィング）、`/specguard:decide`（ADR 記録）など
- **フック**: SessionStart（`specguard pending` — 未処理 sentinel をセッション開始時に提示）
- **condukt 連携**: condukt の Phase 8 (gate PASS 後) が自動で `specguard ingest` を呼ぶ。結果が condukt の完了を阻害しない非ブロッキング設計 (Human-on-the-loop)

---

### 観測・通知

#### gauge
ローカル LLMOps テレメトリ。トークン・キャッシュ率・ツールコール・コストを JSONL に記録する。他プラグイン（budgetguard・harness-status・session-insights）の数値源。

- **フック**: SessionEnd（セッション記録）

#### session-insights
セッション作業メトリクス（ターン数・ツールコール・ファイル数・サイズクラス・カテゴリ）を集計し、Obsidian vault へ書き出す。

- **フック**: PostToolUse（逐次カウント）、SessionEnd（Obsidian 書き出し・record note 更新）
- **スキル**: `/session-insights:record`

#### difflog
SessionStart で HEAD をスナップショットし、SessionEnd で `git diff` の構造化ログを書き出す。

- **スキル**: `/difflog`

#### beacon
Notification と SessionEnd フックでデスクトップ通知・Slack・webhook に通知する。長いエージェント実行が終わったときや Claude が入力を求めているときに外から気付ける。

- **フック**: Notification、SessionEnd

#### stuckguard
同一ツールコールの反復や編集のオシレーションを PostToolUse で監視し、スタックを検知したら Claude にエスカレーション注入する。

- **フック**: PostToolUse

#### deepwiki
Rust スキャナーがリポジトリ構造をマップし、`.deepwiki/*.md` にアーキテクチャ wiki を生成・鮮度追跡する。

- **スキル**: `/deepwiki`

#### harness-status
HOTL 手動点検ダッシュボード。budgetguard の支出台帳 + gauge のセッション記録 + taskprog の progress.md を 1 画面に集約表示するほか、サブコマンドで観測面を切り出せる。意図的に **CLI 専用（hook なし）・read-only**。

| サブコマンド | 表示 |
|---|---|
| `budget` | 今日のコスト（budgetguard） |
| `sessions` | 直近セッション（gauge） |
| `progress` | 進捗ファイル（taskprog） |
| `hooks` | Stop ゲートの遅延集計 |
| `inject` | UserPromptSubmit 注入サイズ集計 |
| `plugins` | 全プラグインの activation-scope 分類（always-on / event-scoped / manual） |

- **スキル**: `/harness-status:status`（引数なしは budget/sessions/progress を集約）

#### daily
「1 日 1 回だけ」走らせたいタスクを SessionStart で実行する daily-once ランナー。現状の唯一のタスクはセキュリティ監査（`cargo deny check advisories bans sources licenses`）。所見があれば非ブロッキングで `additionalContext` に注入し、クリーン／cargo-deny 未導入なら沈黙する。同日に既に走っていればスキップ（状態は `~/.daily/state/` に保存）。

- **フック**: SessionStart（`daily session-start`）
- **状態 / 設定**: `~/.daily/state/<task>-daily.txt`（DailyGuard の実行済みマーク）、`~/.daily/config.toml` に `enabled = false` で全タスク無効化
- **共通基盤**: `harness-core::daily::DailyGuard`（カレンダー日単位の once ゲート）

---

## プラグイン間の連携図

```
[source]
  compass（単一ゴールの勾配＝一手）  scout（5レンズ広域監査＝複数施策）
      └──────────────┬──────────────┘
                     ↓ backlog（確定キュー）に積む
[driver]
  flow（source→executor を束ねるループ。SessionStart で /flow を提案）
      ↓ 課題文
  condukt（実行の背骨）
      ├─ [前段] playbook / run-book / ctxrot / taskprog が context を整備
      ├─ [Phase 5 並列実装] fugu-router がモデルを選ぶ
      │    stuckguard / ctxrot / budgetguard / gauge が並走監視
      ├─ [Phase 6 検証] donegate / tdd / precommit-audit / reviewgate
      ├─ [Phase 7 完了] beacon で通知
      └─ [Phase 8 クローズ] specguard（drift 監査・non-blocking）
[横断]
  gauge → budgetguard, harness-status, session-insights が数値を読む
  harness-status → budgetguard + gauge + taskprog の集約ビュー
```

---

## フック発火タイミング早見表

| Hook | 発火タイミング | 主な登録プラグイン |
|---|---|---|
| SessionStart | セッション開始時 | autoflow, compass, condukt(restore), context-governor, ctxrot(restore), daily, difflog, flow(propose), hypothesis, specguard, taskprog |
| UserPromptSubmit | プロンプト送信前 | context-governor, ctxrot(guard), fugu-router, playbook, run-book |
| PreToolUse | ツール実行前 | blastguard, ctxrot(preguard) |
| PostToolUse | ツール実行後 | context-governor, ctxrot(toolguard), session-insights, stuckguard |
| PreCompact | /compact 直前 | context-governor, ctxrot(rescue) |
| Stop | ターン終了前 | autoflow, budgetguard, condukt, context-governor, ctxrot(stop), donegate, precommit-audit, propguard, reviewgate, tdd |
| SubagentStop | サブエージェント終了時 | context-governor |
| SessionEnd | セッション終了時 | beacon, compass, difflog, gauge, session-insights, taskprog |
| Notification | 外部通知時 | beacon |

---

## 関連ドキュメント

- **使い方**: [USAGE.md](USAGE.md) — 典型パターン集 / [AGENTIC-CODING-GUIDE.md](AGENTIC-CODING-GUIDE.md) — condukt を背骨に回すガイド
- **アーキテクチャ / 内部設計**:
  - [plugin-activation-scopes.md](plugin-activation-scopes.md) — 発火スコープ分類（always-on / event-scoped / manual）
  - [plugin-dependency-graph.md](plugin-dependency-graph.md) — プラグイン間の依存グラフ
  - [stop-gate-latency.md](stop-gate-latency.md) — Stop ゲートの遅延測定
  - [e2e-autonomy.md](e2e-autonomy.md) — end-to-end 自律ループ
  - [condukt-context-flow.md](condukt-context-flow.md) — condukt のコンテキストフロー
  - [context-optimization.md](context-optimization.md) / [context-optimization-flow.md](context-optimization-flow.md) — コンテキスト最適化

---

## インストール方法

```
/plugin marketplace add yukineko/claude-harnesses
/plugin install <plugin>@yukineko   # 必要なプラグインを個別に
```

各プラグインは `crates/<plugin>/` にあり、`.claude-plugin/plugin.json` で管理されている。バージョンが変わったプラグインだけ bump する（全プラグイン一斉更新は不要）。

---

## 開発

```sh
cargo build --workspace
cargo test  --workspace
claude plugin validate crates/<plugin>
```

各プラグインの詳細は `crates/<plugin>/README.md` を参照。
典型的な使い方パターン（compass・backlog・condukt 直接指定・再開など）は **[USAGE.md](USAGE.md)** を参照。
