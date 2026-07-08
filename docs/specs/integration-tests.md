> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# integration-tests 仕様

## 概要

`integration-tests` は **テスト専用クレート**である。出荷するバイナリもプラグインも持たない
（`Cargo.toml` は `publish = false`、`src/lib.rs` は doc-comment のみの content-free lib）。
ワークスペースの root `members = ["crates/*"]` グロブに自動包含される。実体は `tests/e2e.rs` の
クロスクレート E2E 統合テスト群で、**実際にビルド済みのワークスペースバイナリ**（`flow` /
`backlog` / `fugu-router` / `condukt`）を `std::process::Command` で spawn し、in-process リンクや
mock を一切使わずに flow パイプラインの「integration contract（結合契約）」を pin する。個々の
クレートの単体テストが単一バイナリの内部を守るのに対し、本クレートは **バイナリ間の I/O 形状の
合意**（どのフィールドを渡すか・どのバケットに何を置くか・exit code の意味）を守る。

## 不変条件

- **fail-soft バイナリ探索** — `bin(name)` は `CARGO_TARGET_DIR`（未設定なら manifest dir 相対の
  `../../target`）配下の `release/<name>` → `debug/<name>` の順で最初に存在するものを返し、未ビルドなら
  `None`。各テストは `None` を **skip（`eprintln!` して return）** として扱い、決して failure にしない。
  よってバイナリ未コンパイルのマシンでも suite は green を保つ。実際に契約を行使させるには先に
  `cargo build -p flow -p backlog -p fugu-router -p condukt` が要る。
- **完全隔離（開発者環境非汚染）** — 各テストは project dir に `tempfile::TempDir` を使い、`$HOME`
  配下に状態を書くバイナリ（`backlog` の `~/.backlog`、`condukt` の `~/.condukt`）には fresh な
  `$HOME` を割り当てる。開発者の実 state を触らない。ネットワークも使わない。
- **`flow` は turn を壊さない** — `flow propose` は pending が 0 でも backlog 欠落/エラーでも常に
  exit 0（`contract_a_*` が `status.success()` を要求）。
- **routing はタスク集合を保存する** — `fugu-router route` は全 task id を保存し、各 task に
  haiku/sonnet/opus のいずれかの `suggested_model` を割り当て、`--report` に valid JSON を書く。
- **class ごとの配置規約** — `condukt schedule` は `gated` task を必ず `gated` バケットへ隔離し、
  `serial` task を **決して parallel batch に入れない**（`serial` バケットか `warnings` のどちらかは可）、
  `parallel` task を `batches[].parallel` に置く。
- **完了ゲートの意味** — `condukt state gate` は全 task が `verified` のときのみ exit 0。1件でも未検証なら
  非 0。人間向けの「gate PASS/complete」文言は stderr に出るが、**機械契約は exit code**。
- **決定論** — decomposition fixture は固定 3-task（`tpar`/`tser`/`tgate`、各 class 1件）で、routing/
  schedule は CWD=fresh tempdir で machine-specific な学習メモリに依存しない結果を出す。

## 振る舞い

3 つの契約グループに分かれる。全テストは `#[test]` で、bins 未ビルド時は冒頭で skip する。

- **Contract A — directive injection（`flow propose` → `backlog`）**: `flow propose` が PATH 上の
  `backlog` へ `backlog list --project <cwd> --status pending --json` をシェルアウトする hop を pin。
  - `contract_a_empty_project_is_silent` — 空プロジェクト（backlog 空）では stdout を **無出力**にし、
    exit 0 であること。
  - `contract_a_pending_item_is_announced` — `backlog add --title ZZTASK --priority p1` で1件 seed
    （seed と propose で `$HOME` を共有）し、propose の出力に件数 `1 件` と title `ZZTASK` が含まれること。
    `$TMPDIR` の symlink 差異を避けるため `--project` は canonicalize したパスを使う。
- **Contract B — routing + schedule + state（各 hop を単独で pin）**:
  - `contract_b_route_preserves_ids_and_models` — `route --file <d.json> --report <r.json>`。routed
    stdout が valid JSON で全3 id が生存、各 `suggested_model` が haiku/sonnet/opus、`--report` が
    存在し JSON として parse できること。
  - `contract_b_schedule_routes_classes` — route の出力を `condukt schedule --file` に流し、`tgate` が
    `gated`、`tser` が `serial` か `warnings`（かつ parallel batch には入らない）、`tpar` が `batches`
    に現れること。
  - `contract_b_state_roundtrip_gate_passes_when_all_verified` — `state init`（stdout 末尾の `run-...`
    行から run id を抽出）→ **negative control**（全 task 未検証で gate が FAIL すること）→ 3 task を
    `state set ... --status verified` → `state gate` が exit 0、stderr に PASS/complete を出すこと。
- **Contract C — 連結 3-binary chain（route → schedule → state → gate）**:
  - `contract_c_connected_chain_route_schedule_state_gate` — route の stdout を `routed.json` として
    **一度だけ**永続化し、それを schedule と state init の**両方**が消費する。route→schedule で task 集合が
    厳密一致（`scheduled_ids` = routed ids、drop/extra なし）、同じ routed artifact が state input として
    valid、そして **scheduler が出した id のみ**を verified にして gate が pass すること。単独 hop テストが
    見落とす「全ステージが同一 task 集合に合意しているか」（落ちた id・改名フィールド・黙って task を失う
    バケット）を pin する。

### 構成

- `Cargo.toml` — `publish = false`、依存は無し、dev-dependencies に `serde_json` / `tempfile`
  （いずれも workspace 継承）。real bins は依存として宣言せず、実行時に `bin()` で探索する。
- `src/lib.rs` — 意図的に空（doc-comment のみ）。ライブラリコードは持たない。
- `tests/e2e.rs` — 全契約の実体。ヘルパ `bin`（バイナリ解決）/ `bin_dir`（`flow` の親 dir、child の
  PATH 前置用）/ `path_with`（PATH 合成）/ `decomposition_json`（固定 3-task fixture）/ `scheduled_ids`
  （schedule 全バケットの id 和集合）。テスト: `contract_a_empty_project_is_silent`,
  `contract_a_pending_item_is_announced`, `contract_b_route_preserves_ids_and_models`,
  `contract_b_schedule_routes_classes`, `contract_b_state_roundtrip_gate_passes_when_all_verified`,
  `contract_c_connected_chain_route_schedule_state_gate`。
