> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# fugu-router 仕様

## 概要

`fugu-router` は condukt が生成したタスク分解 JSON に対し、各タスクの `suggested_model` を「過去実績から
学習した方策」で決定論的に上書きするモデルルーティング層である。Sakana AI の fugu が**訓練済み
コーディネータ**でリクエストを役割ごとのモデルへ振り分けるのに対し、Claude の重みは学習できないので、
本 crate は fugu の**形（独立した決定論コンポーネント）だけ**を残し、*学習された判断*を**実績の検索
（k-NN）**に置き換える（`main.rs` doc-comment）。record された episode（`store::Episode`：どのモデルが
検証を通ったか・コスト）を retrieval store とし、`route` が近傍を引いて「歴史的にしきい値を満たす最安
ティア」を選ぶ。判断（解釈・実装・検証）は LLM、ルーティング（実績検索・ティア選択）は本バイナリ、と
割り切る。結合は**ソフト**で、バイナリが無ければ condukt は interpreter 自身の `suggested_model` に
フォールバックし何も壊れない。

## 不変条件

- **API キー・埋め込みサービス非依存（決定論）** — 類似度は語尾ステミング＋ドメイン概念辞書
  （`semantic.rs`：login↔auth↔session を橋渡し）による字句 Jaccard（`rag::similarity`）のみで、
  外部 API も embedding も呼ばない。同じストア状態＋同じ入力に対し `confidence`/`code-index search`/
  `lessons search` はバイト同一の出力を返す（各コマンド doc-comment が明記）。唯一の非決定要素は
  探索（bandit）用の PRNG シードで、`seed_rng` が wall-clock nanos ⊕ store size から採る。
- **ソフト依存・fail-soft** — `budget::under_pressure` は budgetguard を欠いても偽を返す（`budget.rs`）。
  空ストア・空クエリ・重複なしは `procedures search`/`lessons search`/`code-index search` すべて空
  JSON 配列＋exit 0 を返す。`code-index build` は読めない tracked file を fatal にせずスキップする。
- **gated は自動ルーティングしない** — `class == "gated"` のタスクは worker/verifier とも `opus` 固定・
  basis=`gated` で、`cmd_route` は `basis != "gated"` のときだけ `suggested_model` を書き換える。
  interpreter が選んだ値を保存する（人間承認の対象）。
- **独立 verifier** — `policy::verifier_model` は worker と**別ティア**を選ぶ（自身の盲点を共有させない）。
  opus worker→sonnet 検証、低stakes sonnet→haiku、haiku worker→独立性のため一段上の sonnet、
  serial/design（高stakes）→opus。この助言は condukt スキーマに無いため stdout の分解 JSON には載らず、
  `--report` ファイルにタスク id ごとに `verifier_model` として出力する。
- **人間ラベルが verifier の自己合格を上書き** — 方策集計（`policy::aggregate`）は `Episode::effective_pass`
  （`human_label.unwrap_or(pass)`）を学習信号とし、`label` の人間判定が verifier の自己判定を de-bias する。
- **パス正規化** — `record` は `--files` の絶対パスを `pathutil::normalise_paths` で repo 相対へ正規化し、
  マシン固有パスがストアに混入して k-NN 精度を落とすのを防ぐ。
- **コスト最安化** — `policy::decide` はしきい値・最小サンプルを満たすティアのうち cost-per-success
  （`total_cost_usd / passes`）が最小のものを選ぶ。コスト全ゼロ時は TIERS（haiku→sonnet→opus）順で
  最初に適格なティア＝最安に退化する。

## 振る舞い

サブコマンドは `clap` の `Command` enum（`main.rs`）で定義。設定は `~/.fugu-router/config.toml`
（`config::Config`：`k`=6, `sim_threshold`=0.15, `pass_threshold`=0.6, `min_samples`=1, `explore`=true）。

- **`route --file <json> [--report <path>]`（主用途）** — 分解 JSON（`decomp::Decomposition`）を読み、各
  タスクを `route_decision` でルーティング（`cfg.explore` で bandit/threshold を切替）。日次予算逼迫時は
  `policy::downgrade_for_budget` で一段安くする。stdout は `suggested_model` を更新した分解 JSON
  （`#[serde(flatten)] extra` で未知フィールドを round-trip 保持）、`--report` は worker/verifier/basis/
  confidence/neighbors/rationale の per-task マップ。
- **`record --title --model --status …`** — 1 タスクの検証結果を episode store へ append（学習信号）。
  `--status` が合格語（`verified|pass|passed|ok|true`）以外は非合格。pass かつ `--done_criteria` があれば
  playbook store にも書く。`--skill-fingerprint` で SKILL.md コーパス版を刻印できる。condukt 併用時は
  condukt の Stop hook が発火するので手書き不要。任意フィールド2つは計測専用でルーティングに一切影響
  しない: `--duration <secs>`（wall-clock 実測値）、`--delegation <fork|inline>`（`--class` と同じく
  無検証の自由記述。fork/inline のコスト・所要時間比較用。`docs/design-delegation-strategy-measurement.md`
  参照。condukt の `shadow-run`（モデル比較）とは別軸で、こちらは呼び出し側の delegation 戦略選択を
  手動記録する）。
- **`code-index build [--root] [--if-stale]`** — code-RAG slice-1。`git ls-files` で列挙した tracked `.rs`
  からシンボルを抽出し per-repo JSONL（`<root>/.fugu/code-index.jsonl`）を再構築。`--if-stale` は
  path+size+mtime の安価な fingerprint（内容は読まない）を sidecar meta と比較し、不変なら no-op
  （`rebuilt:false`）。実体スキャナ/ストアは `harness_core::code_index`。
- **`code-index search --query [--root] [--k]`** — 構築済み index への決定論的字句 top-K 検索。JSON 配列を
  返し、index 欠落/空は `[]`＋exit 0。
- **`suggest` / `confidence`** — 単発でモデルの当たり（worker/verifier/basis）／較正済み合格確率 [0,1] を
  出す。`confidence` は近傍が `min_samples` 未満なら中立 prior 0.5 に退化（`confidence::calibrated_confidence`）。
- **`procedures search`（別名 `playbook`）** — 似た検証済みタスクの解き方を k-NN で引き interpreter を seed。
- **`lessons add|search`** — プロジェクト非依存の cross-project lessons store（`harness_core::lessons`）。
  content 派生 id で冪等 append。
- **`label` / `stats` / `import [--dedup]` / `sync` / `fingerprint` / `init` / `install` / `uninstall`** —
  それぞれ人間による実績訂正、モデル別 pass率/コスト表示、別マシン store のマージ（content-hash 重複排除）、
  git リモート同期、SKILL.md コーパス版スタンプ、設定生成、UserPromptSubmit フックの settings.json 統合。
- **`prompt`（UserPromptSubmit hook）** — プロンプトがコーディングに見える（`inject::looks_actionable`）とき
  ルーティングメモリ要約を 1 ブロック注入。**advisory のみ**で決定的ではない。`FUGU_ROUTER_DISABLED=1` や
  `enabled=false` で no-op。

### module 責務

- **`policy`** — ティア選択の中核。`Decision`（worker/verifier/basis/confidence/neighbors/rationale）、
  `decide`（threshold）/`decide_bandit`（Beta 事後の Thompson サンプリング＋安ティアへの探索ボーナス
  `cheap_bonus`）、`prior_model`（コールドスタート：design→opus・trivial→cheap・それ以外は blast radius で
  scale）、`verifier_model`、`downgrade_for_budget`、`aggregate`。TIERS=`[haiku,sonnet,opus]`。
- **`rag`** — 字句 k-NN。`tokenize`/`file_tokens`/`similarity`（token Jaccard、file token があれば 0.5:0.5
  で混合）/`knn`（threshold 以上を上位 k）/`Neighbor`。
- **`store`** — episode/playbook の JSONL 永続化（append-only、temp+rename）。`Episode`/`Playbook`、
  `effective_pass`、`content_hash_*`（重複排除）、`import_*`/`dedup_*`/`save_all`/`now_secs`。
- **`config`** — `Config` を TOML から読み、`store_path`/`playbook_path`/`sync_dir_path` を解決
  （`sync_repo` 設定時は `<sync_dir>/{episodes,playbooks}.jsonl` を既定に）。
- **`decomp`** — condukt 分解 JSON の parse/rewrite/round-trip（`Decomposition`/`Task`、未知フィールドは
  flatten で保持）。
- **`semantic`** — 埋め込みサービス無しの正規化（suffix stemming＋概念辞書）。`stem`/`expand_into`。
- **`confidence`** — 較正済み合格確率と `brier_score`。
- **`budget`** — budgetguard への read-only 逼迫問い合わせ（ソフト依存、fail-soft）。
- **`inject`** — UserPromptSubmit 要約の生成（`looks_actionable`/`summary`）。advisory のみ。
- **`pathutil`** — repo 相対パス正規化。`find_repo_root`/`normalise_path(s)`。
- **`fingerprint`** — SKILL.md コーパスの決定論 fingerprint。
- **`rng`** — 探索用 xorshift PRNG（`normal` で Beta 事後の Gaussian 近似サンプリング）。
- **`install`** — settings.json への hook 統合／除去。
