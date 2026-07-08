> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# hypothesis 仕様

## 概要

`hypothesis` は PDO（product discovery / hypothesis-driven development）の「仮説」ライフサイクルを
管理する project-global なハーネスである。各仮説（`hypothesis::Hypothesis`）は text・status・任意の
`linked_goal`（compass ゴールキーワード）・pre-registered な success/kill criterion・依拠する assumption 列・
discovery confidence を持ち、`store::Store` が TOML（`config::Config::hypotheses_path()` =
`<store_dir>/hypotheses.toml`）へ atomic に永続化する。CLI（`main.rs` の `clap` `Command` enum）で
add/list/validate/reject/assume/rat/tested/confidence/await-measurement を回し、SessionStart hook
（`hooks::session_start`）で open / awaiting-measurement な仮説を context へ注入する。中核の姿勢は
**「ビルドは検証ではない（build != validation）」** — コードが出荷されただけでは validated に遷移させず、
測定された学びを要求する。判定（何が仮説か・測定結果の解釈）は人間/上位フローに置き、本バイナリは
falsifiability ゲートと決定論的な永続化・順序付けに徹する。

## 不変条件

- **build != validation（測定必須ゲート）** — `store::validate_with_measurements` は evidence も
  measurement も無ければ拒否する（"validate requires measured evidence"）。`AwaitingMeasurement` は
  「出荷済みだが未測定」の独立 status で、`mark_awaiting_measurement` は evidence を要求せず validated へは
  進めない（人間が測定後に validate/reject する）。`reject` も空でない `--reason` を要求する。
- **pre-registered success gate（事後 goalpost 移動の禁止）** — `add` 時に `success_criterion` を登録した
  仮説は、その metric の measurement を渡し、かつ登録済みの bar（`hypothesis::Criterion::satisfied_by`）を
  クリアしないと validated にならない。measurement 欠落・bar 未達はいずれも拒否し、値が `kill_criterion` に
  一致する場合はエラーが `reject` を指し示す。criterion 無しの仮説は従来どおり非空 evidence だけで validated。
- **confidence 降順が load-bearing** — `store::list` は confidence 降順、同点は `created_at` 昇順（ISO-8601
  文字列の辞書順＝時系列）で決定論的に並べる（`f64::total_cmp` で NaN-safe）。open リストは挿入順ではなく
  スコア順になり、最高 confidence の bet が最初に浮上する。confidence 未指定・legacy レコードは
  `hypothesis::default_confidence()`（中立の 0.5）。
- **RAT = 最もリスキーな未検証 assumption** — `Assumption::is_leap_of_faith` は「未 tested かつ risk=High かつ
  evidence≠Strong」を leap of faith とし、`Hypothesis::riskiest_assumption` は `leap_score`
  （risk weight + evidence weakness）最大の leap を選ぶ。フル build 前に最小実験で de-risk すべき対象を示す。
- **決定論的 ID** — `hypothesis::new_id` は text を FNV-1a 64bit（`harness_core::hash::fnv1a64`）でハッシュし
  下位 32bit を 8桁 hex にする。同一 text は同一 ID（tests `new_id_is_deterministic`）。
- **atomic 永続化** — `Store::save` は temp ファイルへ書いてから rename する。store は `BTreeMap` 相当の
  安定順序ではなく Vec 挿入順で serialize されるが、`list` の表示順は上記ソートで決定論。
- **後方互換（legacy load）** — `evidence`/`linked_goal`/`condukt_run`/`success_criterion`/`kill_criterion`/
  `assumptions`/`confidence` はすべて `#[serde(default)]` で、旧レコードもエラーなく load する。
- **fail-soft hook** — `hooks::session_start::run` は `HYPOTHESIS_DISABLE`（`Config::disabled_env`）や
  `enabled=false`、config/store の読み取り失敗、open/awaiting が空のとき `None`（無注入）を返す。
  `run_hook` 経由で常に exit 0。注入は `inject_limit`（既定 2000 byte）で char boundary 切り詰め。

## 振る舞い

サブコマンドは `main.rs` の `Command` enum。すべて `store::Store::load(&cfg)` を土台にする。

- **`add <text> [--goal] [--success] [--kill] [--confidence]`** — 仮説を追加し ID を stdout へ。`--success`/
  `--kill` は `Criterion::parse`（`>=`/`<=`/`>`/`<`/`==`）で登録、`--confidence` 省略時は 0.5。`add_with_criteria`。
- **`list [--status]`** — confidence 降順で列挙。`--status open|awaiting-measurement|validated|rejected` で
  絞り込み（未知値は何にも一致せず空）。各行に status・confidence・criterion・RAT・condukt_run を付す。
- **`validate <id> [--evidence…] [--measurement…] [--run]`** — 測定ゲート＋success ゲートを通してから
  `Status::Validated` へ。measurement は `metric=value`（`parse_measurement`）で evidence として保存される。
- **`reject <id> [--reason] [--run]`** — `--reason` 必須。`Status::Rejected` へ遷移し reason を evidence に追加。
- **`await-measurement <id> [--run]`** — `Status::AwaitingMeasurement` へ（出荷済み・未測定。validate/reject は後）。
- **`assume <id> --text --risk --evidence`** — assumption を添付（`Risk::parse` low|medium|high、
  `Evidence::parse` strong|weak|none）。RAT de-risking の入力。
- **`rat <id>`** — 最もリスキーな未検証 leap of faith を `<index>\t<assumption>` で1行出力（de-risk 済みなら
  無出力 exit 0）。フローが index を狙って後で tested にできる。
- **`tested <id> <index>`** — assumption[index] を `tested=true` に（範囲外は "out of range" エラー）。RAT 後に
  呼ぶとその leap が RAT 対象から外れ、次点が繰り上がる。
- **`confidence <id> <value>`** — discovery confidence を更新し `list` 順序を決定論的に再編（`set_confidence`）。
- **`install [--dry-run]` / `uninstall`** — `~/.claude/settings.json` に SessionStart hook を冪等に登録/除去
  （`install.rs`、`harness_core::install`）。
- **`session-start`** — SessionStart hook 本体（内部用）。open/awaiting-measurement な仮説を Markdown で注入し、
  compass charter（`.compass/charter.md`）に紐付かない仮説へ `[unlinked]` マーカーと警告を付す。

### module 責務

- **`main`** — `clap` CLI 定義・ディスパッチ・`parse_measurement`（`metric=value` パース）。エラーは stderr へ
  出し exit 1。
- **`hypothesis`** — ドメイン型。`Hypothesis`（`new`/`riskiest_assumption`）、`Status`（Open/AwaitingMeasurement/
  Validated/Rejected、snake_case serde、`is_*` 述語・Display）、`Criterion`+`Comparator`（`parse`/`satisfied_by`/
  Display）、`Assumption`（`leap_score`/`is_leap_of_faith`）+ `Risk`/`Evidence`、`new_id`（FNV-1a）、
  `default_confidence`。
- **`store`** — `Store` の load/save（atomic rename）と全遷移: `add_with_criteria`/`validate_with_measurements`
  （2段ゲート）/`mark_awaiting_measurement`/`reject`/`add_assumption`/`mark_assumption_tested`/`set_confidence`/
  `list`（confidence 降順ソート）/`all`。id 不明は "hypothesis not found" エラー。
- **`config`** — `Config`（`enabled`/`store_dir`/`inject_limit`）を `<base_dir>/config.toml` から load
  （`harness_core::config::base_dir("hypothesis")`、`~` 展開）。`hypotheses_path`/`disabled_env`。
- **`goal_link`** — compass charter 連携。`find_charter`（上方向 5 階層探索）・`parse_charter`（north_star /
  definition_of_done 抽出）・`check_goal_link`（`linked_goal` が charter に部分一致するか判定、charter 不在なら
  全 unlinked）。純粋読み取り。
- **`hooks::session_start`** — hook 入口 `run`（disable/enabled/空を fail-soft で処理）とテスト可能な `run_with`。
  open + awaiting-measurement を注入、unlinked 警告付与、`inject_limit` で char-boundary 切り詰め。
- **`install`** — SessionStart hook の登録/除去（`harness_core::install` 委譲）。
