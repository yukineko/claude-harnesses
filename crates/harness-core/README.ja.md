# harness-core

> 🌐 [English](README.md) ・ **日本語**

harness の各 Claude Code プラグインが共有する「不変のインフラ」の単一の正典 (single source of truth)。

## 目的

harness-core は、harness のすべてのプラグインで**同一でなければならない**土台を一箇所にまとめた、ビルド時の**ライブラリ crate** である。並列セッション安全なノートストア、hook の入出力ラッパー（判定を持たない observability hook 用の exit 0 ラッパーと、Stop ゲート用の fail-closed ラッパー）、三値の `Verdict`/`Determination` 型、`~/.claude/settings.json` のインストール機構、プロジェクトごとのアドレッシング、メトリクスのシンク——こうした「全プラグインで挙動が一致していなければならないもの」をここで一度だけ実装し、各プラグインはそれを再実装せずに合成する（そして相互にドリフトしない）。

これは**プラグインではなくライブラリ crate** であり、`plugin.json`・hook・バイナリのいずれも持たない。各プラグインがこれを自分の自己完結バイナリへ静的にリンクするため、配布される `crates/<plugin>/bin/` は実行時に `../harness-core` を参照しない。プラグイン固有のドメインロジックや config/metrics の*フィールド*は、各プラグイン crate 側に残る。

提供するモジュールは次のとおり。

| モジュール | 共有する内容 |
|---|---|
| `store` | 永続的で Obsidian 互換のノートストア。プロジェクトごと、並列セッション安全なフォールバック付き（harness の不変条件） |
| `hook` | Hook の stdin ペイロード構造体と `run_hook` ラッパー（パニックを捕捉して stderr に記録し、常に exit 0）。**判定を持たない observability hook 専用**——判定を返す hook は `gate::run::run_guarded` を使うこと（決定なしの exit 0 は「許可」と区別できないため） |
| `hook_latency` | 中央集約された追記専用の Stop-hook レイテンシ台帳（`<base_dir>/state/hook-latency.jsonl`）——共有の 1 ファイルなので集計は 1 回の読み込みで済む。best-effort（IO エラーは握りつぶす。判定ではなく observability 用） |
| `install` | `~/.claude/settings.json` の読み込み / タイムスタンプ付きバックアップ / 書き込みと、command marker による所有権検出 |
| `hash` | FNV-1a（32/64-bit）——オンディスクのアドレッシングを支える非暗号ハッシュの単一の正典 |
| `projkey` | プロジェクトキー `<basename>-<fnv1a32-hex>`——run-state ファイルのアドレッシングの単一の正典 |
| `config` | home/base-dir の解決、チルダ展開、環境変数パースのプリミティブ |
| `gate` | 共有の Stop ゲート機構：`runner`（タイムアウト付き・出力末尾のみ保持する subprocess ランナー）、`state`（セッションごとの `max_attempts` カウンタ）、`run`（`run_guarded` パニック防壁と一度きりの skip）、`protection`（全ブロッキングゲートが宣言する `Protection` 文）。`run_guarded` は **fail closed**：ゲート本体がパニックすると最初の stop では `{"decision":"block"}` を出し、連続 2 回目のパニック（`stop_hook_active`）だけを通す。interactive 実行では exit 1 |
| `spans` | Span モデルと防御的な JSONL ローダー（`~/.tracekit/<run_id>/spans.jsonl` のオンディスク契約） |
| `session` | セッションごとの正典レコード（`<state_dir>/sessions/<id>.json`） |
| `usage` / `transcript` | ストリーミング JSONL transcript リーダーと、モデルごとのトークン/使用量集計（transcript 全体を読み込まない） |
| `estimate` | transcript → (usage, cost) の唯一の同梱推定器（`estimate_transcript_cost`。crate ルートに re-export）。`usage::aggregate` と `pricing::session_cost` を1回の呼び出しに束ねる——`gauge` / `budgetguard` / `session-insights` はいずれも独自の推定ロジックを持たず、これを共有利用する |
| `metrics` | 追記専用 JSONL のメトリクス SINK、並列安全 |
| `pricing` | モデル→USD のコスト表（cache read/write の倍率を含む） |
| `ledger` | 日次の支出を永続化する台帳（`~/.budgetguard/state/ledger.json`） |
| `daily` | カレンダー日ごとに一度だけのガード |
| `inject` | context-injection hook（`playbook`・`runbook`）の共有基盤 |
| `inject_metrics` | ハーネス横断の UserPromptSubmit インジェクションサイズ台帳。`turn_key = hash(session + prompt)` をキーにするので、5 つのインジェクタがプロセス間協調なしで同一ターン分を合算できる |
| `interrogate` | ドメイン非依存の、ゲート単位の interrogation 制御構造 |
| `shell` | クロスプラットフォームなシェル起動の単一の正典 |
| `trust` | プロジェクトローカル config のコマンド文字列を尊重するための workspace-trust ゲート |
| `scorer` | 決定論的な優先度スコアラー（`score`。crate ルートに re-export）：`severity_weight × clamp(goal_proximity, 0.0, 1.0) ÷ effort_factor × lens_multiplier`。純粋関数で参照透過——clock も RNG も I/O も無い。`L2`/`L5`（scout の security/safety レンズ）は 1.0 超の乗数を持つ（「壊さない・安全側」） |
| `verdict` | 唯一の三値ゲート判定（`Verdict`：`Clean` / `Violation` / `Undetermined`）と汎用の `Determination<T>`。`Verdict` は `Default` / `From<bool>` を持たず `#[must_use]` で、`Clean` は private witness 無しには作れないため、fail-open はレビュー指摘ではなくコンパイルエラーになる |
| `boundary` | 失敗しうる入力境界（ディレクトリ列挙・ファイル読み込み・subprocess）を `Determination` で返し、「観測できなかった」が空 `Vec` や無視された終了コードへ潰れないようにする |
| `undetermined` | 「判定不能」分岐のテレメトリ。`Verdict::undetermined` / `Determination::undetermined` ごとに 1 行（crate・ファイル・行・理由）を記録し、`overwatch undetermined-metrics` が集計する |
| `degrade` | 証拠の劣化と判定単調性プロパティ（ゲートが見られるものを劣化させても、判定がより許可側へ動いてはならない） |
| `git_probe` | ゲート crate 共有の「git リポジトリか？」プローブ。「git を実行できなかった」を「リポジトリではない」に潰さない |
| `primary_tree` | 「このパスは PRIMARY working tree の中か？」（git-dir と git-common-dir の比較）を三値で返す。`refuse_if_primary` は main ツリーへの state-store 書き込みを拒否し、`Undetermined` も拒否として扱う |
| `attribution` | 共有 working tree の変更ファイルのうち、ゲートが見なくてよいものを session transcript を識別子として判定する |
| `repeat` | セッションスコープの repeat 台帳：ゲートは同じユーザー指示を 2 度ブロックしてはならず、2 度目は ask する |
| `fleet` | fleet の GATE crate 名の正典集合（と `BLOCKING_GATES`）。condukt/tdd に手書きコピーされていたリテラルを置き換える |
| `autonomy` | 全プラグインが直接読む唯一の autonomy スイッチ（condukt/ctxrot/autoflow が個別に持っていたスイッチを置き換える） |
| `parallel` | セッション全体の並行数上限。強制するのは `parallelguard` の PreToolUse hook だけ |
| `plugin_bin` | hook プロセスの `$PATH` を信用せずに、兄弟プラグインの実行ファイルを見つける |
| `progress` | 「保持者は進捗しているか、生きているだけか？」の判定エンジン。セッション横断の staleness reaper（backlog のロック、condukt の claim）が共有する |
| `append` | JSONL シンク向けのアトミックな単一 `write()` 追記（本文と改行を 1 バッファで書くので、並列の `O_APPEND` writer が交錯しない） |
| `discovery` | マシンスコープの追記専用「discovery record」ストア。並行する compass/scout セッションのタスク重複を避ける（fail-soft） |
| `lessons` / `retrieval` | マシン全体で共有するプロジェクト横断の lessons ストア（`~/.lessons/lessons.jsonl`、id で冪等）と、その読み出し側の retrieval イベント台帳 |
| `code_index` / `text_index` / `callgraph` / `index_store` | 決定論的で純粋に字句的なコードインデックス（シンボル・全文・caller→callee）と、その 3 つをまとめるプロセス内ファサード——embedding も外部 API も使わない |

## どうして必要か

harness は多数のプラグインから成り、それらは同じ土台——ノートストア、hook の入出力、settings.json の配線、メトリクスの記録——の上に立っている。この土台を各プラグインが個別に実装すると、次の問題が起きる。

- **ドリフト。** 同じはずの挙動が、プラグインごとに少しずつ違う実装になり、時間とともに食い違っていく。harness-core は「全プラグインで同一でなければならないもの」を一度だけ定義し、各プラグインに合成させることで、この食い違いを構造的に防ぐ。
- **不変条件の崩れ。** 並列セッション安全なノートストアや「判定不能は制限側に倒す」のような harness の不変条件は、各所で再実装するたびに守り損ねるリスクがある。`store`・`verdict`・`gate::run::run_guarded`（パニック時は fail closed で block）として一箇所に置くことで、すべてのプラグインが同じ保証を共有する。なお `hook::run_hook`（あらゆるパニックでも exit 0）は判定を持たない observability hook 専用であり、判定を持つ hook には適用しない。
- **実装の重複。** transcript のストリーミング読み込み、pricing 表、settings.json のバックアップと所有権検出といった土台コードを各 crate が書き直すのは無駄が多く、バグの温床になる。

つまり harness-core は、harness 全体の「動作の一貫性」と「不変条件の保証」を担うレイヤーである。

## どう使うか

harness-core は依存ライブラリであり、プラグインではない。**インストールする hook も配線するものも無い**。各プラグインが自分の `Cargo.toml` でこの crate に依存し、必要なモジュールをリンクするだけで使える。

ビルドとテストは次のとおり。

```sh
cargo test
```

ワークスペースの一部としてビルドされる（`cargo build --workspace --release`）。harness-core 自身は committed な `bin/` を持たず、各プラグインのバイナリへコンパイルされて取り込まれる。
