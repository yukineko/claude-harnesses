> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# mutategate 仕様

## 概要

`mutategate` はワークスペース向けの **ミューテーションテスト kill-rate ゲート**である。ミューテーション
エンジン自体は実装せず、標準ツール [`cargo-mutants`](https://mutants.rs) が `mutants.out/outcomes.json`
に残す機械可読な結果を消費する。役割は「ゲート」のみ — outcomes.json をパースし（`parse_outcomes`）、
生存可能な mutant のうち kill された割合（kill-rate = mutation score）を計算し（`MutationSummary::kill_rate`）、
閾値と突き合わせて pass/fail を判定し（`evaluate`）、閾値未満なら非ゼロ exit する。この parse→score→exit の
ロジックは純粋で `lib.rs` の固定サンプル JSON に対してユニットテスト済みのため、判定は決定論的であり
（遅い）エンジンを起動せずに走る。エンジン実走とゲートの結線は `scripts/mutation-gate.sh` と
`.github/workflows/mutation.yml` CI job の役割で、本 crate の外にある。

**重要（タスク前提の訂正）**: これは mutating/destructive なツール呼び出しをゲートする hook プラグインでは
**ない**。配布される Claude Code プラグインですらなく、`.claude-plugin/plugin.json` も `hooks/` も持たない
内部ツール（`Cargo.toml` の `publish = false`。CONTRIBUTING.md いわく shipped でない内部ツールは Cargo.toml
のみでよい）である。単一 bin（`src/main.rs`）の CLI として動く。

## 不変条件

- **エンジン非実装 / 純関数コア** — `mutategate` は mutant を注入しない。`parse_outcomes`（テキスト→
  `MutationSummary`）と `evaluate`（`MutationSummary`＋閾値→`GateOutcome`）は副作用のない純関数で、
  `lib.rs::tests` の固定 JSON でカバーされる。判定は入力 JSON に対して決定論的。
- **baseline は除外** — `outcomes.json` の `Baseline` シナリオ（未改変ビルド）は mutant ではなく、
  スコアから除く。`is_mutant` は scenario JSON が `"Mutant"` キーを持つオブジェクトのときだけ真を返す
  （Baseline はベア文字列 `"Baseline"` として serialize される）。トップレベルの集計値を信用せず
  `outcomes` 配列から直接数えるので、スコアは生レコードだけから再現可能。
- **kill-rate の定義（固定）** — `viable = caught + missed + timeout`（`unviable` はコンパイル不能で
  シグナルを持たないため分母から除外）、`killed = caught + timeout`（timeout はテストが露出させた
  観測可能な誤動作なので kill 扱い）、`kill_rate = killed / viable`。`success`/`failure` は帳簿目的で
  数えるがスコアには入れない。
- **viable ゼロは失敗** — 生存可能な mutant が 0 のとき kill-rate は未定義（`None`）で、ゲートは
  **失敗**とする（「何も測れていない run は何もゲートしない」）。`evaluate` は `None` 分岐で
  `passed: false` を返す。
- **閾値ちょうどは pass** — `evaluate` の比較は `kr + KILL_RATE_EPSILON >= threshold`（`KILL_RATE_EPSILON`
  = 1e-9）。二進浮動小数の丸め（例 0.7999999 が真値 0.8）で閾値ちょうどが落ちるのを防ぐ。
- **前方互換パース** — 未知/欠落の `summary` 値は無視し（`match` の `_ => {}`）、新しい `cargo-mutants`
  状態が来ても壊れない。`serde` の `#[serde(default)]` で `outcomes`/`scenario`/`summary` 欠落を許容。
- **exit code の分離** — `0` = kill-rate が閾値を満たしゲート成功、`1` = 閾値未満または viable な mutant 無し
  でゲート失敗、`2` = usage/IO/parse エラー（ゲートを評価できなかった）。この 3 値でパス・実質的失敗・
  評価不能を区別する。

## 振る舞い

CLI 引数は `clap` の `Cli` 構造体で定義（`main.rs`）:

- **`--outcomes <PATH>`** — `cargo-mutants` の `outcomes.json` パス。既定 `mutants.out/outcomes.json`
  （`DEFAULT_OUTCOMES`）。
- **`--min-kill-rate <F64>`** — 合格に要する最小 kill-rate（0.0..=1.0）。既定 0.80
  （`DEFAULT_MIN_KILL_RATE`。PIT や Meta ACH の実務上の堅牢性バーを反映し、パイロットではフレークで
  なくシグナルであるよう保守的に据える）。

実行フロー（`main`）:

1. `--min-kill-rate` が `0.0..=1.0` の範囲外なら stderr にエラーを出し `ExitCode::from(2)`。
2. `--outcomes` を `std::fs::read_to_string` で読む。IO エラーなら「先に `cargo mutants` を走らせろ」旨を
   添えて exit 2。
3. `parse_outcomes` でタリー化。パース失敗なら exit 2。
4. `evaluate(summary, min_kill_rate)` で `GateOutcome` を得る。
5. stdout に mutant 内訳（viable / caught / timeout / missed / unviable）と kill-rate・閾値を print。
   viable ゼロ時は kill-rate 行を `n/a` と表示。
6. `outcome.passed` が真なら `PASS: <reason>` を stdout に出し `ExitCode::SUCCESS`(0)。偽なら
   `FAIL: <reason>` を stderr に出し `ExitCode::from(1)`。

想定される呼び出し（README。本 crate 外）: `cargo run -p mutategate -- --outcomes … --min-kill-rate 0.80`
（既存 outcomes.json への決定論ゲート）、`scripts/mutation-gate.sh`（エンジン実走→ゲートの end-to-end）。
パイロットは 1 crate（既定 `harness-core`、`PILOT=<crate>` で上書き）に絞る。

### module 責務

- **`main.rs`** — CLI（`clap::Cli`）・引数検証・ファイル IO・出力整形・exit code 写像を担う薄い
  ドライバ。定数 `DEFAULT_MIN_KILL_RATE`(0.80)・`DEFAULT_OUTCOMES` を保持。スコアリングは持たず
  `lib` に委譲する。加えて gate 失敗時（kill-rate < 閾値 / viable ゼロ）に `emit_violation` で
  overwatch の fleet violation レジストリへ1件記録する（他の防御ゲートと同じ観測パターン）。この記録は
  **best-effort / fail-soft** — ストア書き込み失敗は握り潰し、gate の判定・exit code・出力整形には一切
  影響しない（観測は判定を変えない不変条件）。
- **`lib.rs`** — 純粋なパース／スコアリングコア。`MutationSummary`（`caught`/`missed`/`timeout`/`unviable`/
  `success`/`failure` のタリー、メソッド `viable`/`killed`/`kill_rate`）、`GateOutcome`（`summary`/
  `kill_rate`/`threshold`/`passed`/`reason`）、`parse_outcomes`（JSON テキスト→`MutationSummary`、
  baseline 除外・未知状態無視）、`is_mutant`（scenario が `"Mutant"` キー保持オブジェクトか）、
  `evaluate`（タリー＋閾値→判定、epsilon 込み `>=`、viable ゼロ＝失敗）、定数 `KILL_RATE_EPSILON`(1e-9)。
  `outcomes.json` の生表現は `RawLabOutcome`/`RawScenarioOutcome`（`serde::Deserialize`、`scenario` は
  未型付け `serde_json::Value`）で受ける。`#[cfg(test)]` にサンプル JSON ベースの回帰テスト群
  （baseline 無視・viable/killed 集計・閾値境界・viable ゼロ失敗・未知状態無視・malformed エラー）。
