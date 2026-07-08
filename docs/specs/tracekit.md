> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# tracekit 仕様

## 概要

`tracekit` は condukt の 1 ラン（実行）を、フェーズ（interpreter→worker→verifier）ごとの
**親リンク付き span ツリー**として記録し、可視化し、OpenTelemetry GenAI-semconv JSON として書き出す
スタンドアロン CLI である（`main.rs` の doc-comment）。各 span は `phase`/`model`/`ms`/`cost_usd`/`status`
を保持し、フェーズ完了のたびに 1 件ずつ追記される。`gauge` がコストを agent の *kind* 単位で
バケット化する（run/task/span の連結を持たない）のに対し、tracekit は run ごとの因果ツリーを補い、
失敗ランの「どのフェーズが遅い・高い・壊れた」かを指し示す。ライフサイクル hook ではなく素の CLI で、
呼び出し側（condukt の state-set 遷移、または人間）が `tracekit record` を直接叩く（`run_hook` を
介さない）。**ファイルのみ・ネットワークなし・API キー不要**。

## 不変条件

- **File-only, no network** — 全サブコマンドはローカル FS しか触らない（`span::append`/`load`,
  `otlp::to_otlp` は JSON を組むだけ）。OTLP エクスポートは file-only で、backend への live push は未実装
  （README 記載の後続作業）。
- **append-only 永続化** — span は `~/.tracekit/<run_id>/spans.jsonl`（`span::spans_path`）へ JSONL で
  **追記専用**に書かれる（`OpenOptions::create(true).append(true)`）。並列ワーカーの完了が互いを
  上書きせず、部分ラン（partial run）も読める。unix では file mode `0o600`（owner-only）で作成される
  （`append_creates_store_file_with_owner_only_mode` テスト）。
- **run_id sanitize** — run_id は CLI/condukt RID から流入するため、`span::sanitize`（および `main::sanitize`）が
  英数・`-`・`_` 以外を `_` に写像し path-safe に保つ。両者は同一ロジックの mirror。
- **defensive loader** — `span::load`（`harness_core::spans::load_from` に委譲）は破損した 1 行で
  トレース全体を沈めない。malformed 行は skip して `skipped` に計上し、blank 行は無計上で skip する。
  file 欠落は I/O エラー。`trace`/`export` は skip 件数を warn 表示する。
- **dangling parent は root 扱い** — `parent_id` が span 集合外を指す（未記録の親＝truncated run）span も
  root として描画される（`trace::render` の `known.contains_key` 分岐、`dangling_parent_renders_as_root`
  テスト）。interpreter span が着地しなくてもトレースは必ず描画される。
- **決定論的タイムライン** — `start_unix_ms = end_unix_ms.saturating_sub(ms)`（`harness_core::spans::Span`）。
  `--end-unix-ms` で記録時刻を上書きでき決定論的リプレイに使える。OTLP hex id は
  `DefaultHasher` ベースで安定（`span_hex`=16 hex/8byte、`trace_hex`=32 hex/16byte、salted 2 ハッシュ）で、
  同じ id は同じ hex に解決し parent 链が一致する（`parent_link_resolves_to_same_hex` テスト）。
- **on-disk contract の共有** — `Span` の serde 形状は tracekit（writer）と replaykit（reader）間の
  on-disk 契約で byte-stable。schema と loader は `harness_core::spans` が single source of truth。
  フィールド順は canonical serialize 順（struct 順）で、無断で並べ替えない。
- **exit code** — `record` 成功 0/失敗 1、`trace`/`export` の load 失敗 2、`export` の書き込み失敗 1、
  `list` は常に 0（base_dir 欠落でも 0）。

## 振る舞い

サブコマンドは `clap` の `Command` enum で定義（`record`/`trace`/`export`/`list`）。

- **`record`** — 1 span を run の store へ追記する（`cmd_record`→`span::append`）。`--run`/`--span`/`--name`/
  `--phase` が必須、`--parent`（root は省略）/`--model`/`--task`/`--ms`（既定 0）/`--cost`/`--status`（既定 `ok`）/
  `--end-unix-ms`（既定 now＝`now_unix_ms`）は任意。成功時に span_id と path を stderr に出す。
- **`trace <run_id>`** — run の span を読み、インデント済みツリー＋1 行ロールアップを stdout へ描画
  （`cmd_trace`→`span::load`→`trace::render`）。ロールアップは span 数・wall（`wall_ms`＝最遅 end − 最早 start,
  saturating）・合計 cost・最遅 span・エラー数を含む。error span は `✗`、それ以外は `·` で印付け
  （`is_error`＝`ok`/`verified` 以外を case-insensitive でエラー扱い）。空なら "no spans recorded"。
- **`export <run_id> [--service] [--out]`** — span を OTLP/JSON（`TracesData` 形状、
  `resourceSpans→scopeSpans→spans`）で書き出す（`cmd_export`→`otlp::to_otlp`）。`--service`（既定 `condukt`）は
  `service.name` resource 属性。`--out -` は stdout、省略時は `~/.tracekit/<RID>/otlp-<RID>.json`。
  各 span は `gen_ai.operation.name`（`tool`/`execute_tool` フェーズは `execute_tool`、他は `invoke_agent`）・
  `harness.phase`・`gen_ai.request.model`・`harness.task_id`・`gen_ai.usage.cost_usd`・`harness.status` 属性、
  `SPAN_KIND_INTERNAL`(1)、nanos タイムスタンプ、`status.code`（error=2/ok=1）を持つ。skip 件数を warn。
- **`list`** — `spans.jsonl` を持つ run dir 名を sort して列挙（`cmd_list`）。base_dir 欠落なら stderr に
  注記して 0 を返す。

## module 責務

- **`main`** — clap CLI 定義と 4 サブコマンドの dispatch（`cmd_record`/`cmd_trace`/`cmd_export`/`cmd_list`）。
  `now_unix_ms`（record 既定終了時刻）と `sanitize`（default export ファイル名用、`span::sanitize` の mirror）を持つ。
- **`span`** — tracekit 固有の storage 層。`run_dir`/`spans_path`（`~/.tracekit/<run_id>/spans.jsonl`）・
  `sanitize`・`append`（append-only writer, unix 0o600）・`load`（`load_from` 委譲）。`Span` 型と `load_from` は
  `harness_core::spans` を re-export し replaykit と schema を共有する。
- **`trace`** — flat span list から span ツリーを組み立て端末描画する純関数群（IO なしで unit-testable）。
  `render`/`render_node`/`summary`/`wall_ms`。child→parent を id で連結し、dangling parent を root 扱いにする。
- **`otlp`** — run の span を OTel GenAI-semconv OTLP/JSON（file-only、network なし）に変換する。
  `to_otlp`/`span_to_otlp`/`operation_name`/`str_attr`/`double_attr`/`span_hex`/`trace_hex`/`hash64`。
  hex id は `DefaultHasher` 由来で決定論的、trace_hex は salt 付き 2 ハッシュ。

> **参考**: `Span` schema・`start_unix_ms`/`is_error`・defensive `load_from` は本 crate ではなく
> `harness_core::spans` に定義され、tracekit（writer）と replaykit（reader）で共有される。
