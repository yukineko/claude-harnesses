> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# schemaguard 仕様

## 概要

`schemaguard` は LLM の構造化出力を **source→executor 境界** で検証するスキーマゲート CLI である。
名前付きで宣言済みのスキーマ（`registry` 内蔵の静的定義）に対して JSON 値を照合し、違反時には
`{path, problem}` を並べた構造化エラー（producer がモデルに 1 度だけ再依頼するための契約）を stdout へ
出力し、パース失敗・フィールド違反の双方を reject としてスキーマ別に計上する。これにより source→executor
境界での silent drop が観測可能になる。`main.rs` の doc-comment が明記するとおり、これは **lifecycle hook
ではなく素の CLI** であり、`run_hook` で包まず、呼び出し側が終了コード（`0`/`1`/`2`）で分岐する設計である。
判定はすべてバイナリ内の純関数 `schema::validate` が担い、外部 JSON-Schema クレートには依存しない。
`lib.rs` は同じ `metrics`/`registry`/`schema` モジュールを公開し、他クレート（例 `condukt`）が
デシリアライズ前に in-process で検証できるようにする。

## 不変条件

- **終了コードの意味は固定**（`main.rs` doc-comment）— `0`=JSON パース成功かつスキーマ妥当 / `1`=パース成功
  だが違反あり（→再依頼）/ `2`=JSON パース失敗、または未知スキーマ要求。`cmd_check` は schema 解決を最初に
  行い、未知スキーマは入力を読む前に fail-fast で `2` を返す。ファイル読み取り失敗・stdin 読み取り失敗も `2`。
- **validate は純関数**（`schema.rs`）— `validate` は副作用を持たず、short-circuit せず全違反を蓄積して返す。
  これにより呼び出し側は完全な error 集合を得て精密な再依頼プロンプトを組める。unknown な余剰フィールドは
  黙って許容する（`unknown_extra_fields_are_allowed`）。
- **型不一致は以降のチェックを打ち切る** — `validate` はフィールドが必須欠落なら violation を積んで continue、
  型不一致なら violation を積んだ後 enum/再帰チェックをスキップする（mistyped 値に更なる検査は無意味）。
- **メトリクスは fail-soft**（`metrics.rs`）— `record_reject` の書き込み IO エラーは stderr へ warning を
  出すのみでゲートの終了コードを変えない。`counts` はファイル欠落で空 map、malformed 行は黙ってスキップ
  （`parse_counts`）。パース失敗も違反も両方 reject として計上する（`cmd_check` は前者に `record_reject(name, 1)`、
  後者に `record_reject(name, error_count)`）。
- **append-only JSONL** — reject は `~/.schemaguard/rejects.jsonl`（`harness_core::config::base_dir("schemaguard")`
  ＝ `~/.schemaguard/`）へ 1 行 1 reject で追記される。`ts`（unix 秒, `SystemTime`）は optional field で、
  `skip_serializing_if` により欠落時はシリアライズされない。集計は schema 名で `BTreeMap` に和を取る（決定論）。
- **schema-required は消費側 struct と lockstep**（`registry.rs`）— `decomposition` の per-task 必須は `id` のみ。
  これは condukt の `model::Task` に合わせた意図的設計で、`title`/`class`/`done_criteria` を required にすると
  condukt の parser が常に受理してきた decomposition を precheck が reject し「valid input is byte-identical」
  保証を壊すため。コメントで明示されている。

## 振る舞い

サブコマンドは `clap` の `Command` enum で定義（`main.rs`）。

- **`check --schema <name> [--file <path>]`**（`cmd_check`）— `registry::get` で schema 解決（未知なら既知名を
  stderr へ列挙して `2`）。入力は `--file` 指定時はファイル、省略時は stdin から読む。`serde_json::from_str` で
  パース → 失敗なら `record_reject(name, 1)` して `{valid:false, error:"invalid JSON: …"}` を出し `2`。成功なら
  `schema::validate` を実行。違反 0 件なら `{valid:true, schema, errors:[]}` を出し `0`。違反ありなら
  `record_reject(name, error_count)` して `{valid:false, schema, errors:[{path, problem}]}` を出し `1`。
- **`metrics [--json]`**（`cmd_metrics`）— `metrics::counts` の schema 別 reject 件数を表示。`--json` は
  `serde_json::to_string_pretty` で機械可読出力、無指定は human-readable table（reject 無しなら
  `No rejects recorded yet.`）。常に `0`。
- **`list`**（`cmd_list`）— `registry::names` の既知スキーマ名を 1 行ずつ表示。常に `0`。

宣言済みスキーマ（`registry::names` の安定順）は **5 つ**: `decomposition` / `episode` / `playbook` /
`scout-measure` / `verdict`。

> 実装との差異（flag）: `README.ja.md` / `README.md` / `plugin.json` の description は 4 スキーマ
> （`decomposition`/`episode`/`playbook`/`scout-measure`）と記述し、`verdict` を列挙していない。
> `verdict`（`candidate`/`pass`/`group`, condukt の `consensus::Verdict` に対応）は実装済みだが README/manifest が
> 追随していない spec drift。`plugin.json` version は `0.1.2`。

各スキーマの形（`registry.rs` 静的 `Field` slice）:

- **`decomposition`** — top-level 必須 `goal`(string)/`tasks`(array)。`tasks` 各要素は `id`(必須 string)、
  optional `title`/`class`(enum `parallel|serial|gated`)/`done_criteria`/`suggested_model`(enum `haiku|sonnet|opus`)/
  `confidence`(enum `high|medium|low`)。
- **`episode`** — 必須 `title`(string)/`model`(string)/`pass`(bool)、optional `class`/`role`/`cost_usd`(number)。
- **`playbook`** — 必須 `title`(string)、optional `done_criteria`/`class`。
- **`scout-measure`** — 必須 `title`/`lens`(enum `L1..L5`)/`severity`(enum `high|medium|low`)/`effort`(enum
  `xs|s|m|l|xl`)/`evidence`。
- **`verdict`** — 必須 `candidate`(string)/`pass`(bool)、optional `group`。

## module 責務

- **`main`** — CLI 配線（clap の `Cli`/`Command`/`CheckArgs`/`MetricsArgs`）と 3 ハンドラ
  （`cmd_check`/`cmd_metrics`/`cmd_list`）、`exit(code)`。lifecycle hook ではない。
- **`lib`** — `metrics`/`registry`/`schema` を公開し、他クレートが in-process 検証できるライブラリ面。
- **`schema`** — 外部依存の無い小さな宣言的スキーマエンジン。`Ty`（`String`/`Number`/`Bool`/`Array`/
  `Object`/`Any`。後 2 者は `#[allow(dead_code)]` で予約＝現行 registry では未使用）、`Field`
  （`name`/`ty`/`required`/`enum_values`/`items`）、`Schema`、`Violation`（`path`/`problem`, serde 可シリアライズ）、
  純関数 `validate`（object 検査・必須・型・enum・array 要素の再帰、path prefix 付与）。
- **`registry`** — 名前付きスキーマの静的レジストリ。`names`（安定順の 5 名）と `get`（未知は `None`）。
  各スキーマの `Field` slice を保持。新スキーマはここに追加する。
- **`metrics`** — reject 観測。`record_reject`（fail-soft 追記）、`write_reject_line`、`counts`（JSONL 読取り集計）、
  純ヘルパ `parse_counts`（FS 非依存でテスト可能）、`RejectLine`（`schema`/`violations`/optional `ts`）、
  `rejects_path`（`~/.schemaguard/rejects.jsonl`）。
