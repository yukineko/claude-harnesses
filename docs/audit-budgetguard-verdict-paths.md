# budgetguard の verdict 経路 監査（per-gate、逐語引用つき）

- **対象**: `crates/budgetguard/`（`config.rs` / `gate.rs` / `lock.rs` / `main.rs` / `install.rs`）
- **監査日**: 2026-07-31
- **監査時点**: main `9c29fc22`（作業 branch `audit/budgetguard-verdict-paths`）
- **版**: budgetguard 0.1.15 から 0.1.16
- **位置づけ**: compass charter DoD9「各 gate crate の verdict 経路が三値で表現され、silence・空集合・
  panic・IO・parse・subprocess 失敗を restrictive へ解決する」の per-gate 監査。分子を **2 から 3**
  （schemaguard, autoflow に budgetguard を追加）へ進める。

この文書の完了条件は **「budgetguard の未監査 verdict 経路 0本」**。以下は判定を持つ関数と、その
判定を消費する call site の**全列挙**である。分類は次の 3 つ:

- **P（permissive）** — 判定不能を「問題なし（許可）」へ潰していた。**本監査で是正した。**
- **R（restrictive）** — 判定不能を制限側へ解決している。**変更しない**（うち数件は見た目が
  permissive なので、誤って「直される」のを防ぐため明記する）。
- **D（deliberate）** — 意図的に許可へ解決する**宣言された仕様**。permissive とは別物。

---

## 0. 発見の起点 — 二値の形が原因である

`Config::load` は `Config` を返す**二値の形**だった。「設定を読んで、こう設定されていた」と
「設定を読めなかった」が**同じ型の同じ値**に写り、下流から区別不能だった。
`crates/blastguard/src/model.rs:5`「Three answers, not two.」が名指ししている構造そのもの。

この構造が実害になるのは、**欠けた第三の答えの行き先が `Config::default()` だから**である:

```rust
// crates/budgetguard/src/config.rs:63-68（監査前・現在も同じ）
Config {
    enabled: true,
    session_warn_usd: 0.0,
    session_block_usd: 0.0,
    daily_warn_usd: 0.0,
    daily_block_usd: 0.0,
```

そして 0.0 は「上限なし＝全許可」を意味する。これは推測ではなく、**既存テストが仕様として固定**している:

```rust
// crates/budgetguard/src/gate.rs（既存テスト zero_threshold_means_disabled）
assert!(matches!(verdict(&Config::default(), 999.0, 999.0), Verdict::Allow));
```

### 実測（判断ではない）

監査前のコードに、`block_usd = 1.0` を宣言しつつ構文エラーを 1 個含む `budgetguard.toml` を与えて
実行した結果:

```
PROBE session_block_usd=0 daily_block_usd=0 enabled=true
PROBE verdict(999,999) = ALLOW
```

**タイポ 1 個でゲートが黙って無効化される。** operator にはエラーも警告も出ない。
`budgetguard status` は `session.block_usd: 0.00` と表示し、「設定されていない」と読める。

---

## 1. P — 是正した permissive 経路（4件）

### P1. 設定ファイルの read 失敗が「未設定」に写る

```rust
// 監査前 crates/budgetguard/src/config.rs:93
if let Ok(text) = std::fs::read_to_string(&path) {
```

`Err` に else が無い。read できなければ `cfg` は `Config::default()` のまま返る。
**是正**: `crates/budgetguard/src/config.rs:106-114` で `Determination::undetermined` を返す。

### P2. 設定ファイルの parse 失敗が「未設定」に写る

```rust
// 監査前 crates/budgetguard/src/config.rs:94
if let Ok(fc) = toml::from_str::<FileConfig>(&text) {
```

同上。**是正**: `crates/budgetguard/src/config.rs:115-123`。

> **なぜ「存在しない」と別扱いなのか（重要な区別）**: 設定ファイルが**そもそも無い**のは
> 判定不能ではなく **KNOWN な答え**（operator は何も設定していない）であり、
> `crates/budgetguard/src/config.rs:1-5` が宣言する「installing it without configuring any limits is a no-op」という
> **意図的な仕様**である。したがって不在は `Known(default)` のまま残した。
> 是正したのは「**ファイルは在るのに読めない/解釈できない**」場合だけ。
> この線引きは `an_absent_config_is_known_default_not_undetermined` が固定しており、
> 修正が「全ての未設定インストールを block する」側へ**行き過ぎない**ことを保証する。

### P3. `exists()` が「問い自体に答えられなかった」を「無い」に写す

```rust
// 監査前 crates/budgetguard/src/config.rs:80,84
if p.exists() {
...
if h.exists() {
```

`Path::exists()` は**存在しない場合と、権限等で問い自体に答えられなかった場合の両方に `false`**
を返す。**是正**: `crates/budgetguard/src/config.rs:161-182` の `locate` が `try_exists()` を使い、`Err` を
`Undetermined` へ解決する。

### P4. ledger ロックを取れなくても read-modify-write を実行する

`lock.rs` の module header は、監査前の時点で**自分が招く障害を自分で説明していた**:

```rust
// 監査前 crates/budgetguard/src/lock.rs:4-7
//! The daily ledger is a single shared file updated on every Stop. Without
//! serialization, two sessions that Stop at the same moment each load → record →
//! save and the last writer clobbers the other's entry (lost update), silently
//! under-counting the day total and letting the daily block fail open.
```

そして直後にこう書いてあった:

```rust
// 監査前 crates/budgetguard/src/lock.rs:16-18
//! This is best-effort by design: if the lock can't be acquired within the
//! timeout we proceed anyway rather than ever blocking a turn — correctness under
//! contention is improved, and the hook never hangs.
```

**「letting the daily block fail open」と書いた 9 行あとに、そこへ進むことを正当化している。**
正当化の語は「**rather than ever blocking a turn**」＝ CLAUDE.md 第1節が
「**この語を verdict 経路の docstring・コメント・コミットメッセージに書いた時点で赤信号**」と
名指ししている当のものである。第1節はこの語が判定を持つコードに適用されないことを既に宣言しているが、
**このモジュールは移行から取り残されていた**。

呼び出し側は guard の `held` を**一度も見ていなかった**:

```rust
// 監査前 crates/budgetguard/src/gate.rs
let _guard = crate::lock::LedgerLock::acquire(&cfg.state_dir);
let day_usd = match Ledger::load_checked(&cfg.state_dir) {
```

**是正**: `LedgerLock::held()` を公開し（`crates/budgetguard/src/lock.rs:60-62`）、`crates/budgetguard/src/gate.rs:100-115` で
「直列化できなかった＝本日の合計を測定できていない」として **write を行わず**
`day_undetermined_verdict` へ解決する。境界は精密にした:

- session の費用は ledger と独立に測定済みなので**そのまま厳密に enforce**する
  （`crates/budgetguard/src/gate.rs:155-157`。session block 超過は `session-budget-exceeded` の具体的理由を維持）。
- **daily 上限が armed のときだけ** day 側を restrictive に倒す（`crates/budgetguard/src/gate.rs:162-165`）。
- **daily 上限が未設定なら**、未知の day total が跨ぎうる閾値が存在しないので**何もゲートしない**
  （`crates/budgetguard/src/gate.rs:166-170`）。これが無いと修正は「競合したら常に block」へ行き過ぎる。

---

## 2. P — 付随して是正した経路（3件）

### P5. 読めないロックを「消えた」と見なして live なロックを奪う

```rust
// 監査前 crates/budgetguard/src/lock.rs:76-79
let Ok(meta) = std::fs::metadata(path) else {
    // Vanished between attempts — treat as acquirable.
    return true;
};
```

コメントは "Vanished" と言っているが、`let Ok(..) else` は **NotFound 以外の全エラー**
（権限、経路途中の非ディレクトリ）も捕らえる。それらは「消えた」証拠ではなく「**確かめられなかった**」
であり、`true` を返すと**生きている別セッションのロックを奪う**＝ P4 が防ごうとしている lost update を
自分で再現する。**是正**: `crates/budgetguard/src/lock.rs:98-108` で `NotFound` だけを acquirable とし、他は `false`。

### P6. `status` の cwd 解決失敗が別プロジェクトの設定を読む

```rust
// 監査前 crates/budgetguard/src/main.rs:185
let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
```

**是正**: `crates/budgetguard/src/main.rs:214-220` で報告して exit 2。

### P7. `status --json` が判定不能を `pressure:false` として下流に配る

`status --json` は fugu-router が読む（`crates/fugu-router/src/budget.rs`）。設定が読めないとき、
監査前は `daily_warn_usd: 0.0` から `pressure: false`（＝余裕あり）を**断定して出力**していた。
**是正**: `crates/budgetguard/src/main.rs:224-236` で thresholds も `pressure` キーも出さず exit 2。

---

## 3. R — 判定不能を制限側へ解決している経路（変更しない）

**見た目が permissive なので、grep ベースの一括修正で誤って「直される」危険がある。**
autoflow 監査の `crates/autoflow/src/lock.rs:81` と同じ、**grep の見た目と意味が逆転する**類。

| 位置 | 逐語 | なぜ restrictive か |
|---|---|---|
| `crates/budgetguard/src/config.rs:220` | `env_bool("BUDGETGUARD_DISABLE").unwrap_or(false)` | `false` は「無効化**されていない**」＝ゲートは**armed のまま**。未設定・不正値でゲートは切れない |
| `crates/budgetguard/src/gate.rs:326-337` | `record_is_fresh` が `.ok()` 失敗時に `return false` | `false` は「gauge の記録は信用しない」＝**より正確な transcript 再計算へ**フォールバック。過少計上を防ぐ |
| `crates/budgetguard/src/gate.rs:92-93` | `Determination::Undetermined(why) => ... undetermined_verdict(cfg, ...)` | 既に三値化済み（先行作業）。session 費用の測定不能は armed な上限に応じて Block/Warn |
| `crates/budgetguard/src/gate.rs:122-132` | ledger 破損時に**上書きせず** `session_usd` を day total とする | 上書きは当日の累積を消す＝fail open。保存して自セッション分を下限とするのは保守的 |
| `crates/budgetguard/src/lock.rs:113-114` | `mtime.elapsed().map(...).unwrap_or(false)` / `Err(_) => false` | `false` は「stale ではない」＝**奪わない**。未来 mtime や取得失敗でロックを壊さない |
| `crates/budgetguard/src/main.rs:108-119` | panic barrier が `run_guarded` 経由 | panic は **fail closed（block）**。`stop_hook_active` で 2 回目は bounded に allow |

---

## 4. D — 意図的に許可へ解決する宣言済み仕様（permissive ではない）

| 位置 | 挙動 | 宣言箇所 / 固定テスト |
|---|---|---|
| 設定ファイル不在 | `Known(default)` ＝全上限 0.0 ＝ Allow | `crates/budgetguard/src/config.rs:1-5` の module header。`an_absent_config_is_known_default_not_undetermined` |
| 全上限 0.0 | 任意の費用で Allow | `zero_threshold_means_disabled` |
| transcript データ無し | 費用化できるデータが無い。exit 0 | `crates/budgetguard/src/gate.rs:104` `Determination::Known(None) => return None,` と `emit_and_exit` の None 分岐 |
| hook 入力の空フィールド | exit 0 | `crates/budgetguard/src/main.rs:127`、統合テスト `gate_with_empty_fields_exits_zero` |
| `stop_hook_active` 再入 | block しない | `crates/budgetguard/src/main.rs:134-136`（再 block で turn を閉じられなくなるのを防ぐ bounded escape） |
| `BUDGETGUARD_DISABLE=1` | exit 0 | `crates/budgetguard/src/main.rs:109-111`。panic guard の**外側**で評価され、常に到達可能 |

**D と P の違いは「許可するか」ではなく「許可を*決めた*か」である。** D は operator の意図か
宣言済み契約に基づく **KNOWN な答え**。P は**測れなかったものを測れたことにしていた**。

---

## 5. 散文と実挙動の食い違い（CLAUDE.md 第4節 — 同じコミットで是正）

監査中に、**docstring が実挙動と食い違う**箇所を 2 件検出した。第4節は
「散文が実装と食い違ったら、それは次のレビュアーを騙す仕掛けになる」としている。

1. **`crates/budgetguard/src/main.rs` の module header（監査前・現在は存在しない文）**:
   "Harness errors always exit 0 (never break the turn)."
   — **既に偽だった**。同ファイル `crates/budgetguard/src/main.rs:83-88` が panic barrier の fail-closed 化を説明しており、
   **同じファイルの中で自己矛盾**していた。本監査の変更でさらに 2 経路が block するようになった。
2. **`crates/budgetguard/src/gate.rs:5-6`（監査前）**: `//! Harness errors always exit 0 and allow the stop.`
   — 同文の直後に `undetermined_verdict` による block を説明しており、やはり自己矛盾。

いずれも**実挙動を列挙する形へ書き換えた**（`crates/budgetguard/src/main.rs:5-25` / `crates/budgetguard/src/gate.rs:3-18`）。
どの経路が exit 0 に残るかも明示した（残るのは §4 の D だけ）。

---

## 6. 検証（判断ではなく観測）

- **RED を先に観測**: 三値の入口を「旧挙動を保つ stub」として先に置き、テストが**コンパイルエラー
  ではなく挙動で**落ちることを確認した。5件中 3件 FAILED / 2件（対照）ok。
- **アンチ空虚の対照**: `a_valid_config_is_known_and_carries_its_limits`（正常系は Known）と
  `an_absent_config_is_known_default_not_undetermined`（不在は Known）が RED 時点で**通っていた**
  ため、失敗は fixture ではなく**欠陥に起因**すると確定できる。
- **負の対照**: `is_stale` の修正を旧挙動へ戻すと `an_unreadable_lock_is_not_assumed_stale` **だけ**
  が落ち、他 4 件は通ったままであることを実測（テストが当該欠陥に特異的である証拠）。
- **mutation testing**: 実装へ fail-open を 12 種類注入し、**全 12 件がその名前を主張するテストで
  kill された**。うち **3 件は overshoot 変異**（不在設定を undetermined にする／daily 上限なしでも
  block する／不在ロックを永久に取得不能にする）で、修正が**制限側へ行き過ぎていない**ことを担保する。
  1 件は**罠の変異**（config 未判定を `undetermined_verdict(&Config::default(), ..)` 経由にすると
  `Allow` に戻る）。
- `cargo test -p budgetguard` **36 件 green**（unit 31 + 統合 5。監査前は 21 + 5）。
- `cargo clippy -p budgetguard --all-targets` **clean**。

> **利害関係の非独立性（明示する）**: CLAUDE.md 第2節 (a) は「テストは利害のない Agent が書く」を
> 要求する。本セッションでは system prompt が Agent 起動を禁じているため、**実装者と同一の主体が
> テストを書いた**。これは規範からの逸脱であり、隠さずここに記す。代償として
> **mutation testing（12/12 kill、うち overshoot 3・罠 1）と負の対照**を置いたが、
> **同一主体の盲点を共有するリスクは残る**。independent agent による再監査が可能になった時点で、
> 本監査の対象範囲を再検査することを推奨する。

---

## 7. 未監査で残したもの（この監査の境界 — 隠さず記す）

| 項目 | 状態 | 理由 |
|---|---|---|
| `crates/budgetguard/src/install.rs:12-13` `dirs::home_dir().unwrap_or_else(\|\| PathBuf::from("."))` | **未是正・backlog 済み** | home 解決失敗時に `./.claude/settings.json` を書き、`Installed Stop hook` と**成功を報告**する。ゲートが設置されない＝fleet 規模の fail-open だが、verdict 経路ではなく**設置経路**なので本監査の完了条件の外。別項目として起票 |
| `crates/budgetguard/src/install.rs:19-22` `current_exe().ok()...unwrap_or_else(\|\| "budgetguard")` | **未是正・backlog 済み** | PATH 上の裸名へフォールバック。既存 backlog `1e783882`（`crates/harness-core/src/boundary.rs:186` の裸 `overwatch`）と**同一クラス** |
| `crates/fugu-router/src/budget.rs:27` | **別 crate・未変更** | `when budgetguard is absent/errors (soft dep` と宣言されている。**宣言済みの soft dependency** であり advisory な routing（block/allow 判定ではない）。本監査は budgetguard 側が `pressure:false` を**断定しない**ようにするに留めた |
| `crates/budgetguard/src/config.rs:161-182` の `try_exists` `Err` 経路 | **テスト無し** | 移植性のある fault injection を構成できなかった（権限依存で root 実行時に成立しない）。mutation では覆えていない**既知の穴**として記す |

---

## 8. 結論

**budgetguard の未監査 verdict 経路: 0本。** permissive 7件を是正、restrictive 6件を
「変更してはならない」として明記、deliberate 6件を仕様として分離、散文の自己矛盾 2件を是正した。

DoD9 の分子は **2 から 3**（schemaguard, autoflow, budgetguard / 分母 22）。

最も一般化する所見は、この crate 固有ではない:

> **fallback の行き先が「全許可」を意味する既定値であるとき、fallback は degrade ではなく
> ゲートの無効化である。** `Config::default()` は「安全な既定」に見えたが、
> 「上限 0.0 ＝ 上限なし」という別の宣言済み仕様と組み合わさった瞬間、
> **read 失敗を無条件 Allow へ変換する装置**になっていた。
> 他 crate の既定値も「その既定へ落ちたとき何を許可することになるか」で読み直す価値がある。
