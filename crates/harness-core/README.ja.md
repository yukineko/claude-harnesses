# harness-core

> 🌐 [English](README.md) ・ **日本語**

harness の各 Claude Code プラグインが共有する「不変のインフラ」の単一の正典 (single source of truth)。

## 目的

harness-core は、harness のすべてのプラグインで**同一でなければならない**土台を一箇所にまとめた、ビルド時の**ライブラリ crate** である。並列セッション安全なノートストア、ターンを決して壊さない hook ラッパー、`~/.claude/settings.json` のインストール機構、プロジェクトごとのアドレッシング、メトリクスのシンク——こうした「全プラグインで挙動が一致していなければならないもの」をここで一度だけ実装し、各プラグインはそれを再実装せずに合成する（そして相互にドリフトしない）。

これは**プラグインではなくライブラリ crate** であり、`plugin.json`・hook・バイナリのいずれも持たない。各プラグインがこれを自分の自己完結バイナリへ静的にリンクするため、配布される `crates/<plugin>/bin/` は実行時に `../harness-core` を参照しない。プラグイン固有のドメインロジックや config/metrics の*フィールド*は、各プラグイン crate 側に残る。

提供するモジュールは次のとおり。

| モジュール | 共有する内容 |
|---|---|
| `store` | 永続的で Obsidian 互換のノートストア。プロジェクトごと、並列セッション安全なフォールバック付き（harness の不変条件） |
| `hook` | Hook の stdin ペイロード構造体と、ターンを決して壊さない `run_hook` ラッパー（あらゆるエラー/パニックでも exit 0） |
| `hook_latency` | 中央集約された追記専用の Stop-hook レイテンシ台帳（`<base_dir>/state/hook-latency.jsonl`）——共有の 1 ファイルなので集計は 1 回の読み込みで済む。best-effort でターンを決して壊さない |
| `install` | `~/.claude/settings.json` の読み込み / タイムスタンプ付きバックアップ / 書き込みと、command marker による所有権検出 |
| `hash` | FNV-1a（32/64-bit）——オンディスクのアドレッシングを支える非暗号ハッシュの単一の正典 |
| `projkey` | プロジェクトキー `<basename>-<fnv1a32-hex>`——run-state ファイルのアドレッシングの単一の正典 |
| `config` | home/base-dir の解決、チルダ展開、環境変数パースのプリミティブ |
| `gate` | 共有の run/runner/state ゲート機構 |
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

## どうして必要か

harness は多数のプラグインから成り、それらは同じ土台——ノートストア、hook の入出力、settings.json の配線、メトリクスの記録——の上に立っている。この土台を各プラグインが個別に実装すると、次の問題が起きる。

- **ドリフト。** 同じはずの挙動が、プラグインごとに少しずつ違う実装になり、時間とともに食い違っていく。harness-core は「全プラグインで同一でなければならないもの」を一度だけ定義し、各プラグインに合成させることで、この食い違いを構造的に防ぐ。
- **不変条件の崩れ。** 並列セッション安全なノートストアや「ターンを決して壊さない hook」のような harness の不変条件は、各所で再実装するたびに守り損ねるリスクがある。`store` や `hook::run_hook`（あらゆるエラー/パニックでも exit 0）として一箇所に置くことで、すべてのプラグインが同じ保証を共有する。
- **実装の重複。** transcript のストリーミング読み込み、pricing 表、settings.json のバックアップと所有権検出といった土台コードを各 crate が書き直すのは無駄が多く、バグの温床になる。

つまり harness-core は、harness 全体の「動作の一貫性」と「不変条件の保証」を担うレイヤーである。

## どう使うか

harness-core は依存ライブラリであり、プラグインではない。**インストールする hook も配線するものも無い**。各プラグインが自分の `Cargo.toml` でこの crate に依存し、必要なモジュールをリンクするだけで使える。

ビルドとテストは次のとおり。

```sh
cargo test
```

ワークスペースの一部としてビルドされる（`cargo build --workspace --release`）。harness-core 自身は committed な `bin/` を持たず、各プラグインのバイナリへコンパイルされて取り込まれる。
