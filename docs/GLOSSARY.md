# GLOSSARY — claude-harnesses 用語・クレート早見表

> 目的: セッション開始時にこの1枚を読めば全体像がつかめる。各クレートの詳細は該当 `crates/<name>/README.ja.md` を必要時だけ読む（context 節約）。

## ドメイン用語

- **harness（ハーネス）** — Claude Code の挙動を型にはめる Rust クレート群の総称。この repo は「yukineko のハーネス一家」を単一ワークスペースで管理する。
- **plugin（プラグイン）** — `.claude-plugin/plugin.json` + `hooks/` + 同梱 `bin/` を持つ配布単位。1 クレート = 1 プラグイン。
- **hook（フック）** — `hooks/hooks.json` で宣言する Claude Code ライフサイクルイベント（SessionStart / UserPromptSubmit / PreToolUse / PostToolUse / Stop / SubagentStop / PreCompact / SessionEnd / Notification）へのフック。プラグインの発火面。
- **SKILL / スキル** — スラッシュコマンド（`/condukt` 等）として起動される LLM 手順。フックを持たず skill だけのプラグインは「manual」。
- **worktree** — condukt が並列タスクごとに切る git worktree。衝突解析で並列/直列にスケジュールし、実装後にマージする。
- **gate（ゲート）** — 条件を満たすまで Stop（完了宣言）を阻止する決定論チェック。donegate / tdd / propguard / reviewgate / precommit-audit など。
- **source↔executor** — タスクを供給する側（compass の一手・backlog キュー・scout 施策・hypothesis）と実行する側（condukt）を分ける設計軸。flow がこの2層を1ループで束ねる。
- **driver（ドライバ）** — source→executor を回す統合ループ層。`/flow` が現行のドライバ。
- **PDO（Parallel Development Orchestration）** — 仮説駆動で並列に開発施策を回す枠組み。hypothesis がそのライフサイクル（作成・検証・棄却・出荷≠検証の追跡）を担う。
- **PDO session anchor（セッションアンカー）** — 各セッションが「今どの PDO 単位を担当し（scope＝触るファイル/glob・done_criteria＝完了定義）」を保持する仕組み。overwatch の `Lease` に scope/done_criteria を持たせ、flow が pick 時に `overwatch begin --scope/--done-criteria` で登録（`overwatch status` に可視化）、ctxrot が guard で再注入（記憶喪失対策）、stuckguard が scope 逸脱を検知（drift 対策）＋heartbeat 便乗（誤 reap・奪取防止）、overwatch begin が scope_overlap/possible_duplicate を早期警告、condukt reconcile が二重完了を検知（exit 2）。設計は `docs/DESIGN-pdo-session-anchor.md`。
- **HOTL（Human On The Loop）** — 人間が随時点検・介入する運用モデル。harness-status / taskprog がハンドオフを支援する。
- **autonomy gate / switch** — 自律運転時に人間ゲートを縮退させる仕組み。condukt の `state autonomy-check` / env `CONDUKT_AUTONOMOUS` / config で切替。
- **fail-soft（フェイルソフト）／ fail-closed** — 前提ツール不在時に安全側へ縮退して継続するのが fail-soft、閾値未満で確実に阻止するのが fail-closed。propguard は fail-closed、oracle は tdd 不在時に fail-soft。
- **`.githooks/`** — `git config core.hooksPath .githooks` で有効化する versioned な git hooks。`pre-commit` は injectguard の advisory 事前チェック、`pre-push` は GATE_CRATES（blastguard/propguard/specguard/stuckguard/mutategate/overwatch）への変更を検知して Continuous-Audit ラウンド実行（`scripts/continuous-audit.sh --dry-run`）を勧める。両方とも fail-soft（常に exit 0、push/commit を止めない）。
- **F→P オラクル（再現性オラクル）** — condukt の完了ゲートが要求する有効な Fail→Pass 遷移（`condukt state check-oracle`）。これを伴わない fix/feature の verified 昇格を拒否する。
- **subscription-native** — API キー不要で Claude Code サブスクリプション内で完結する設計（compass / ship / ctxrot / tdd 等）。
- **activation scope（発火スコープ）** — プラグインをフック頻度で分類する軸。**always-on**（毎ターン級のイベントを持つ）/ **event-scoped**（低頻度イベントのみ）/ **manual**（フックなし・skill か CLI 起動）。`harness-status plugins` が自動分類する。
- **golden eval** — `*.jsonl` の期待値でオフライン回帰検証する仕組み。evalkit が実行、curate（playbook→golden）と replaykit（trace→golden）が供給する。
- **span / trace** — condukt run の interpreter→worker→verifier を親子リンクした span 木（phase/model/ms/cost/status）。tracekit が記録・描画する。

## クレート一覧（プラグイン）

| クレート | 分類 | 一言 |
|---|---|---|
| autoflow | always-on | Stop で `/record`→`/condukt` を回すセッション終了オートフローゲート |
| backlog | manual | クロスプロジェクト・タスクキュー（`/backlog` skill + SessionStart 通知 + binary） |
| beacon | always-on | ターン完了・入力待ちをデスクトップ/Slack/webhook 通知する |
| benchkit | manual | ベンチ回帰＋SWE-bench ランナー。auto-gate 通過変更を無作為抽出し厳格監査へ回す事後サンプリング較正ループ（auditsample）を含む |
| blastguard | always-on | Bash/ファイル操作の破壊的コマンドを PreToolUse で deny する（機微パス glob／公開シンボル変更の diff リスクも RiskAssessment に合流） |
| budgetguard | always-on | Stop でセッション/日次のコスト上限を監視し超過ターンを阻止する |
| compass | always-on | condukt 上流のゴール再接地＋次の一手導出（subscription-native） |
| condukt | always-on | interpreter/researcher/worker/verifier + Rust binary の決定論オーケストレーションエンジン（cross-task lessons ライフサイクル＋`learning-signal` 計測集計を内蔵） |
| context-governor | always-on | 組込みコンパクションの薄い制御層（pin + lossless-recall + retrieval + tool-hygiene） |
| ctxrot | always-on | context 劣化ガード（検出・救済・復元・蒸留＋load/pin/drop 制御） |
| curate | manual | fugu-router playbook を evalkit の versioned golden データセットへ昇格する |
| daily | always-on | SessionStart で登録シェルタスクを 1 日 1 回だけ実行し所見を注入する |
| daily-report | manual | git ログ＋session-insights の record ノートを日次1枚の日報に合成し Obsidian へ書き戻す skill（`daily` とは別物：あちらはタスク実行、こちらは日報生成） |
| deepwiki | manual | リポ構造マップから `.deepwiki/*.md` アーキテクチャ wiki を自動生成する |
| difflog | always-on | SessionStart で HEAD をスナップショット、SessionEnd で git diff サマリを記録する |
| donegate | always-on | Stop で受け入れコマンドを実行し全 green まで完了を阻止する |
| evalkit | manual | golden `*.jsonl` を実行し契約劣化で非ゼロ終了するオフライン回帰評価ハーネス |
| flow | always-on | source（compass/backlog）→executor（condukt）を 1 ループで束ねる統合 driver |
| fugu-router | always-on | 検証実績から cheap-first でモデル選択する per-model ルーター |
| gauge | always-on | Stop でトークン/コスト/ツール呼び出し/レイテンシをローカル計測する LLMOps テレメトリ |
| harness-status | always-on (実質サイレント) | HOTL 手動点検の統合ダッシュボード（CLI 専用）＋hook binary 欠損時のみ警告する軽量 SessionStart hook 1本（健全時は無出力） |
| hypothesis | always-on | PDO 仮説のライフサイクル管理（作成・検証・棄却・compass 紐づけ） |
| overwatch | always-on | project-global 実行台帳＋cross-session 重複ガード（begin 同一 key で live 他 session を skip）＋PDO 進行管理ビュー（各session/backlog/hypothesis/condukt run/compass gap を fail-soft 集約）＋操作(pause/resume/reassign/reap)＋PDO session anchor（Lease に scope/done_criteria、`begin --scope/--done-criteria`、`lease --session`、begin が scope_overlap/possible_duplicate を早期警告） |
| playbook | always-on | UserPromptSubmit で関連アトミックノートを予算内でコンテキスト注入する |
| precommit-audit | always-on | Stop で汎用＋プロジェクトルールを diff に照合し clean まで完了を阻止する |
| propguard | always-on | done_criteria から 3–5 個の意味的不変条件を導出し fail-closed で Stop を検証する |
| replaykit | manual | tracekit の run trace を evalkit golden へ再生する回帰ハーネス |
| reviewgate | always-on | Stop で diff をレビューし合格まで完了を阻止するコードレビューゲート |
| runbook | always-on | UserPromptSubmit で `!name` マクロを repo 手順（`.runbook/<name>.md`）に展開する（plugin 名 runbook） |
| schemaguard | manual | source→executor 境界で LLM 構造化出力を宣言 schema 検証し 1 回 re-ask するゲート |
| scout | manual | 5レンズ並列監査で施策を生成し backlog へ積んで /flow へ渡す SOURCE |
| session-insights | always-on | セッション単位でツール/ターン/ファイル/サイズ・カテゴリを集計、Obsidian 記録も可 |
| ship | event-scoped | commit・merge・push・plugin-update の出荷儀式を促す（未出荷状態を検出・subscription-native） |
| specguard | always-on | 仕様↔実装の整合を監査する read-only ハーネス（subscription-native）＋ polarity synonym guard の段階的 ratification ＋ spec-map ストア |
| stuckguard | always-on | PostToolUse で反復操作・編集スラッシュを検知しエスカレーションする（Jaccard near-repeat ＋ progress-score 3信号 stall advisory ＋ escalation 時の lesson write/retrieve） |
| taskprog | always-on | `.claude/progress.md` をセッション間で同期し HOTL ハンドオフを支援する |
| tdd | always-on | Stop でテストなし実装を阻止するテストファースト・ゲート（RED before GREEN） |
| tracekit | manual | condukt run を span 木として記録・描画し OTel GenAI-semconv JSON を export するトレーサ |
| trajectoryeval | manual | worker が辿った tool-call 経路を期待軌跡と照合する trajectory-match verifier。`tier` サブコマンドはリスク階層化 e2e 検証（core allowlist に載る core フローは毎回 structured-data/fuzzy diff、非 core は existence/seeded sampling）を持ち、fuzzy 閾値超過ドリフトは needs-human（exit3）にエスカレートする |

## 非プラグイン

- **harness-core** — 全プラグインが共有する不変インフラの単一ソース（ビルド時依存。各バイナリに静的に焼き込まれる）。
- **mutategate** — このワークスペース用の mutation-testing kill-rate ゲート（内製ツール・配布しない）。
- **integration-tests** — ワークスペース横断の結合テスト専用クレート（プラグイン面なし）。

## さらに読む（docs/）

- [OVERVIEW.md](OVERVIEW.md) — 全体設計・プラグイン一覧・フック早見表
- [USAGE.md](USAGE.md) — セッションを開いてから打つ典型パターン集
- [AGENTIC-CODING-GUIDE.md](AGENTIC-CODING-GUIDE.md) — condukt を背骨にプロジェクトを回すガイド
- [plugin-activation-scopes.md](plugin-activation-scopes.md) — 各プラグインの発火スコープ分類（always-on / event-scoped / manual）と分類ルール
- [plugin-dependency-graph.md](plugin-dependency-graph.md) — プラグイン間の依存グラフ
- [stop-gate-latency.md](stop-gate-latency.md) — Stop ゲートの遅延測定
- [e2e-autonomy.md](e2e-autonomy.md) — end-to-end 自律ループ
- [condukt-context-flow.md](condukt-context-flow.md) — condukt のコンテキストフロー
- [context-optimization.md](context-optimization.md) / [context-optimization-flow.md](context-optimization-flow.md) — コンテキスト最適化の設計と流れ
- [fork-subagent-type.md](fork-subagent-type.md) — fork（subagent_type）の定義・context rot・監査独立性
- [design-delegation-strategy-measurement.md](design-delegation-strategy-measurement.md) — fork/inline 選択のコスト最適性を計測する設計（`fork-subagent-type.md` の後続）
