> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# trajectoryeval 仕様

## 概要

`trajectoryeval` は worker が**たどった経路 (PATH)** を検証する軌跡照合ゲートである。出力検証器
（condukt のオンライン verifier が `done_criteria` = 何が出来たか を見る）の**兄弟**として、worker が
実行した**ツール呼び出しの順序付き列**を期待軌跡 spec と突き合わせ「どうやって出来たか」を見る。
[langchain-ai/agentevals](https://github.com/langchain-ai/agentevals) の trajectory matcher に着想を得た
**subscription-native**（同梱 Rust バイナリ 1 つで完結、API キー・ネットワーク不要）なツール。ライフサイクル
hook ではなく、ふつうの CLI **ゲート**として `extract`／`check` の 2 サブコマンドを提供する（`main.rs`）。
condukt Phase 6 から、タスクに `expected_trajectory` がある場合に fail-soft で呼び出され、出力検証と並行する
第 2 の経路面 verifier 次元として働く。

## 不変条件

- **純粋なマッチャコア** — `match_traj.rs` は IO も panic も持たず、すべて `(spec, actual)` の純関数
  （`evaluate` → `strict`/`unordered`/`subsequence`）。FS に触れずに網羅的にユニットテストできる（module doc）。
- **pass の定義は全モード共通** — `MatchResult::finalize` が `pass = missing.is_empty() &&
  unexpected.is_empty() && !out_of_order` を必ず再計算する。個別モードは `pass: false` で構築し finalize に
  委ねるため、`pass` フィールドを直接立てる経路は無い。
- **0/1/2 ゲートポリシー** — exit code は evalkit/schemaguard と同型（`main.rs` doc）: `0`=pass、
  `1`=逸脱（missing/unexpected/out_of_order）、`2`=harness エラー（入力が読めない・パースできない）。
  `check` は `result.pass` で 0/1 を返し、spec/actual の read/parse 失敗はすべて `2`。`extract` の IO エラーも `2`。
- **hook でラップしない** — `main` は `run_hook` を通さず、実エラーを exit 2 として素通しする（`main.rs` doc の明示ルール）。
- **transcript を全部メモリに載せない** — `extract` は JSONL を `BufReader::lines()` で**1 行ずつストリーム**処理する
  （ハーネスのハード規約）。1 行が JSON として壊れていても・想定フィールドが欠けていても panic せず skip する
  （`extract_tools`/`collect_from_event` の防御的パース）。
- **fail-soft な condukt 連携** — バイナリが `PATH` に無い・タスクに `expected_trajectory` が無い・worker の
  transcript が解決できない場合、Phase 6 のこのステップはまるごと skip される（README）。経路の逸脱（exit `1`）は
  出力の判定を**上書きしない**: 出力が `done_criteria` を満たせばタスクは `verified` のままで、逸脱は HOTL 可視化用に
  `reason` として記録されるにとどまる。

## 振る舞い

サブコマンドは `clap` の `Command` enum で定義（`main`）。

- **`extract --transcript <jsonl>`** — Claude Code トランスクリプトをストリームし、`type == "tool_use"` の
  content block の `name` を**出現順**に集めて JSON 配列として stdout に出す。assistant の
  `["message"]["content"]` 形と top-level `["content"]` 形の両方を許容する（`collect_from_event`）。出力はそのまま
  `check --actual` に流し込める。読めなければ exit 2。
- **`check --expected <spec.json> --actual <actual.json> [--json]`** — 実ツール列を期待 spec と照合し
  `MatchResult { pass, missing, unexpected, out_of_order }` を報告する。`--json` で serde 直列化、既定は
  `print_report` の人間向けレポート。**expected** は `{mode, steps:[{tool, optional?}]}`（`optional` 既定 false、
  `#[serde(default)]`）、**actual** はツール名文字列の配列。exit は pass で 0／逸脱で 1／read・parse 失敗で 2。

### モード（`Mode`: `strict`/`unordered`/`subsequence`、`serde rename_all = "lowercase"`）

- **strict**（`strict`）— 実列が期待**必須**ステップと順序まで一致すること。2 ポインタ走査で、optional は
  スロットで skip 可、required の不一致は「actual[j] が後続 spec に居れば現ステップが `missing`、居なければ
  actual が `unexpected`」で振り分ける。走査後、`lift_reordering` が missing と unexpected の**多重集合の共通部分**を
  両者から抜き出し `out_of_order` を立てる（正しい集合が揃ったが順序違反 = 集合問題でなく順序問題）。
- **unordered**（`unordered`）— 順序を無視し集合帰属のみ。`missing` = actual に無い必須期待ツール（重複排除）、
  `unexpected` = 期待集合に無い実ツール（重複排除）、`out_of_order` は常に false。
- **subsequence**（`subsequence`）— 必須ステップが actual 内に順序どおり（非連続可・他ツール interleave 可）現れればよい。
  actual カーソル `j` を前進させ、見つからない必須ステップだけ `missing`。余分は許容のため `unexpected` は常に空、
  `out_of_order` は常に false。

## module 責務

- **`main`** — CLI 定義（`Cli`/`Command`/`CheckArgs`/`ExtractArgs`）と 2 ハンドラ（`cmd_check`/`cmd_extract`）、
  人間向け整形（`print_report`）、exit code の写像。spec/actual の読み込み・パース・エラー時の exit 2 分岐を担う。
- **`extract`** — トランスクリプト → 順序付きツール名列の抽出器。`extract_tools`（ストリーム、`io::Result`）、
  `collect_from_event`（2 形の content を許容）、`collect_from_content`（`tool_use` block の `name` を push）。
  防御的で、壊れた行・欠損フィールドを skip する。
- **`match_traj`** — 純粋な軌跡マッチャコア。型（`Mode`/`Step`/`Spec`/`MatchResult`）と `evaluate`、
  3 モード関数（`strict`/`unordered`/`subsequence`）、順序違反抽出ヘルパ `lift_reordering`、
  不変式を担保する `MatchResult::finalize`。IO・panic なし。
