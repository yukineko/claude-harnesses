# harness

[![coverage](https://github.com/yukineko/claude-harnesses/actions/workflows/coverage.yml/badge.svg)](https://github.com/yukineko/claude-harnesses/actions/workflows/coverage.yml)

<!-- coverage バッジはトークン不要の GitHub Actions ステータスバッジ。`coverage`
     ワークフロー（`cargo llvm-cov` + 閾値ゲート）の成否 = ワークスペースの
     line coverage が下限を満たしているかを PR チェックに表示する。数値そのものは
     各 run の Job Summary と lcov アーティファクトで確認できる。
     トレンドグラフが欲しい場合は `coverage.yml` の Codecov opt-in を有効化し、
     下のバッジに差し替える:
     [![codecov](https://codecov.io/gh/yukineko/claude-harnesses/branch/main/graph/badge.svg)](https://codecov.io/gh/yukineko/claude-harnesses) -->

Cargo ワークスペース・モノレポ。`yukineko` の Claude Code ハーネス一家を単一ソースで管理する。

- 共通基盤: `crates/harness-core`（ビルド時依存。各プラグインのバイナリに静的に焼き込まれる）
- 各プラグイン: `crates/<plugin>/` — Rust クレートかつ Claude Code プラグイン（`.claude-plugin/plugin.json` + `hooks/` + 同梱 `bin/`）
- 内製ツール: `crates/mutategate`（cargo-mutants kill-rate ゲート。`plugin.json` を持たず配布はしないワークスペース内製ゲート）

計 **37 クレート**: **35 プラグイン**（`harness-status plugins` の分類で always-on 23 / event-scoped 2 / manual 10）+ ビルド時ライブラリ `harness-core` + 内製ゲート `mutategate`（プラグインではない）。各クレートには日本語版 `README.ja.md` も同梱する。

配布はこの repo 自身で完結する。リポ root の `.claude-plugin/marketplace.json` が marketplace カタログで、各プラグインを `git-subdir`（`yukineko/claude-harnesses.git` の `crates/<plugin>`、`ref=main`）で指す。利用側は `/plugin marketplace add yukineko/claude-harnesses` → `/plugin install <plugin>@yukineko`。別リポへの切り出しは行わない。

## プラグイン一覧

| プラグイン | 説明 |
|---|---|
| autoflow | Stop フックで `/record` と `/condukt` を自動ループするセッション終了オートフローゲート |
| backlog | SessionStart フックで未完了タスクを通知するクロスプロジェクト・タスクキュー |
| beacon | Stop/Notification フックでデスクトップ・Slack 通知を送る |
| blastguard | PreToolUse フックで Bash/ファイル操作中の破壊的操作（再帰 rm・git reset --hard・全消し上書き等）を deny する。リポの設定ファイルは除外 |
| budgetguard | Stop フックでセッション/日次コスト上限を監視し超過時に制御する |
| compass | 北極星ゴール設定・次の一手導出を行う condukt の上流エージェント |
| condukt | インタープリタ/ワーカー/ベリファイア/リサーチャーの 4 エージェント + Rust バイナリによる決定論オーケストレーションエンジン。`consensus`（multi-sample 自己整合投票・opt-in）と autonomy switch（`condukt state autonomy-check` / env `CONDUKT_AUTONOMOUS` / config）で自律時に人間ゲートを縮退する。完了ゲートは F→P 再現性オラクル（`condukt state check-oracle`）を強制し、有効な Fail→Pass 遷移を伴わない `fix`/`feature` タスクの verified 昇格を拒否する（tdd 不在時はフェイルソフト縮退） |
| context-governor | Claude Code 組込みコンパクションの薄い制御層。pin + lossless-recall + retrieval + tool-hygiene を単一 hook-dispatch バイナリに束ね、size/cost/correctness の 3 軸を分離する |
| ctxrot | 注入ルール・救済・蒸留・キャリーオーバー制御でコンテキスト劣化をガードする |
| curate | fugu-router playbook を evalkit の versioned golden 評価データセットへ昇格する。機械的な done_criteria（`cargo test` 等）は実行可能な golden に、それ以外は人手補完用 draft に変換し `evals/curated/*.jsonl` に固定。オフライン評価ループの供給側 |
| daily | SessionStart フックで `cargo deny` セキュリティ監査を 1 日 1 回だけ実行し所見を注入する |
| deepwiki | リポジトリ構造マップから `.deepwiki/*.md` アーキテクチャ wiki を自動生成する |
| difflog | SessionStart でスナップショットを取り SessionEnd で git diff サマリを記録する |
| donegate | Stop フックで受け入れコマンドを実行し全 green まで完了を阻止する |
| evalkit | golden `*.jsonl`（ファイル内容 / CLI 出力アサーション）を実行し、プロンプト改変や CLI 契約の劣化で非ゼロ終了するオフライン回帰評価ハーネス。condukt のオンライン verifier のオフライン姉妹で、CI ゲート（`eval.yml`）として動く |
| flow | source（compass の一手 / backlog キュー / hypothesis の計測待ち仮説）→ executor（condukt）を 1 本のループで束ねる統合 driver。出荷済み仮説を計測して validate/reject する measure step を含む。SessionStart で開いている仕事があれば `/flow` を提案する |
| fugu-router | 過去タスクの検証実績から cheap-first（最安 Claude ティア優先）でルーティングし、cascade を安全網に据える per-model ルーター |
| gauge | Stop フックでトークン使用・コスト・ツール呼び出し数をローカルに計測する |
| harness-status | HOTL 手動点検ダッシュボード（CLI 専用・hook なし）。`budget` / `sessions` / `progress` / `hooks`（Stop ゲート遅延）/ `inject`（UserPromptSubmit 注入予算）/ `plugins`（activation-scope 分類）のサブコマンドを持つ |
| hypothesis | PDO 仮説ライフサイクル管理。仮説の作成・検証・棄却・compass ゴール紐づけと、出荷済み・未計測（awaiting-measurement）の追跡を行う（出荷 ≠ 検証） |
| playbook | UserPromptSubmit フックで関連アトミックノートをコンテキストに注入する |
| precommit-audit | Stop フックで設定ルールと diff を照合し問題がなくなるまで完了を阻止する |
| propguard | プロパティゲート。`done_criteria` から 3-5 個の意味的不変条件を導出し、閾値未満なら fail-closed で Stop をブロックする（tdd の「具体テスト」に対する「不変条件」の相補） |
| replaykit | tracekit が記録した condukt run trace を evalkit golden へ再生する回帰ハーネス。`extract` が run の spans.jsonl を可搬な trajectory summary（順序付き phase/model/status + 期待値 expect）へ蒸留、`promote` が fixture を `evals/replay/fixtures` に commit し `replaykit verify` を叩く golden を append、`verify` が steps から aggregate を再計算して expect と照合し 0/1/2 ゲート終了する。curate の playbook→golden に対する trace→golden の姉妹 |
| reviewgate | Stop フックで diff をセルフレビュー/独立レビューし合格まで完了を阻止する |
| runbook | UserPromptSubmit フックでプロンプト中の `!name` マクロを手順に展開する |
| schemaguard | source→executor 境界で LLM の構造化出力（condukt 分解 / fugu episode・playbook / scout 施策）を宣言 schema で検証し、違反時は構造化エラーで 1 回 re-ask、reject 件数をメトリクス計上して silent drop を観測可能にするゲート |
| scout | プロジェクトを5レンズ（課題/セキュリティ/業界標準/不足施策/安全性）で並列監査し施策を生成、backlog に積んで /flow へ引き渡す SOURCE |
| session-insights | ツール呼び出し・ターン数・ファイル数を集計し Obsidian vault に記録。クロスセッション backlog 管理も担う |
| specguard | 仕様と実装の整合を監査する read-only ハーネス |
| stuckguard | PostToolUse フックで繰り返し操作・編集スラッシュを検知しエスカレーションする |
| taskprog | `.claude/progress.md` をセッション間で同期し HOTL ハンドオフを支援する |
| tdd | Stop フックでテストなし実装を阻止するテストファースト・ゲート |
| tracekit | condukt run の interpreter→worker→verifier を親子リンクした span 木（phase/model/ms/cost/status）として記録し、`tracekit trace <RID>` で描画、OTel GenAI semconv JSON を export するトレーサ。失敗 run のどの段が遅い/高い/落ちたかを可視化（file-only・ネットワーク無し） |
| trajectoryeval | エージェントの tool-call 経路を検証する trajectory-match verifier。condukt の verifier が出力（done_criteria）を見るのに対し、worker が辿った経路を見る。`check` が実 tool 列を期待軌跡（strict/unordered/subsequence）と照合し {pass, missing, unexpected, out_of_order} を 0/1/2 ゲート終了で返し、`extract` が transcript をストリームして順序付き tool_use 名に変換。done-criteria 検証の経路面の姉妹（agentevals 流） |

## ドキュメント

- [docs/OVERVIEW.md](docs/OVERVIEW.md) — 全体設計・プラグイン一覧・フック早見表
- [docs/USAGE.md](docs/USAGE.md) — セッションを開いてから打つ典型パターン集
- [docs/AGENTIC-CODING-GUIDE.md](docs/AGENTIC-CODING-GUIDE.md) — condukt を背骨にプロジェクトを回すガイド

アーキテクチャ / 内部設計:

- [docs/plugin-activation-scopes.md](docs/plugin-activation-scopes.md) — 各プラグインの発火スコープ分類（always-on / event-scoped / manual）
- [docs/plugin-dependency-graph.md](docs/plugin-dependency-graph.md) — プラグイン間の依存グラフ
- [docs/stop-gate-latency.md](docs/stop-gate-latency.md) — Stop ゲートの遅延測定
- [docs/e2e-autonomy.md](docs/e2e-autonomy.md) — end-to-end 自律ループ
- [docs/condukt-context-flow.md](docs/condukt-context-flow.md) — condukt のコンテキストフロー
- [docs/context-optimization.md](docs/context-optimization.md) / [docs/context-optimization-flow.md](docs/context-optimization-flow.md) — コンテキスト最適化

各クレートには日本語 `README.ja.md` も同梱している。

## 開発

```sh
cargo build --workspace
cargo test  --workspace
```

各プラグインの検証:

```sh
claude plugin validate crates/<plugin>
```

## カバレッジ

ワークスペースの line coverage は CI の [`coverage.yml`](.github/workflows/coverage.yml) が [`cargo llvm-cov`](https://github.com/taiki-e/cargo-llvm-cov) で計測する（既存の smoke gate とは独立した別ジョブ）。coverage が下限（`COVERAGE_MIN_LINES`、初期値 65%。計測時点のワークスペース line coverage 約 72% から数ポイントの余裕を引いた「床」で、目標値ではなく上げていくための下限）を下回ると CI が非ゼロ終了する。各 run は lcov レポートをアーティファクトに、サマリ表を Job Summary に出力するので、外部サービスやトークンなしで数値を確認できる。ローカルでの計測:

```sh
cargo install cargo-llvm-cov --locked   # 初回のみ
rustup component add llvm-tools-preview  # 初回のみ
cargo llvm-cov --workspace --summary-only
```

トレンドの可視化（Codecov 連携）は `coverage.yml` にトークン設定後に有効化する opt-in として残している。

## バージョニング

git-subdir 配布では `version` 省略時に「コミット SHA = バージョン」となり、モノレポでは無関係なコミットで全プラグインが新バージョン扱いになりうる。
**各 `crates/<plugin>/.claude-plugin/plugin.json` に明示 `version` を置き、そのプラグインが変わった時だけ bump する。**

## 供給網の来歴 (SLSA build provenance)

各プラグインが同梱する配布バイナリ (`crates/<plugin>/bin/<name>-<os>-<arch>`) は、
`build-binaries.yml` の `commit` ジョブが [`actions/attest-build-provenance`](https://github.com/actions/attest-build-provenance)
で **SLSA build provenance** を生成してからコミットする。来歴は各バイナリの sha256 ダイジェストを
このリポジトリのワークフロー実行 (ビルダー) に結び付けるので、利用者は配布物の出所を検証できる:

```sh
# GitHub CLI で来歴を検証
gh attestation verify crates/<plugin>/bin/<name>-linux-x86_64 --repo <owner>/<repo>

# もしくは slsa-verifier で
slsa-verifier verify-artifact crates/<plugin>/bin/<name>-linux-x86_64 \
  --source-uri github.com/<owner>/<repo>
```

来歴の生成は `main` への push 時 (= 実際に landing したバイナリ) のみで、PR では走らない。
