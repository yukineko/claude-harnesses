> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# flow 仕様

## 概要

`flow` は「課題の供給（source）→ 解決手段の実行（executor）」を1本のループで貫く統合 driver
（autopilot 層）である。ただし **ループの本体は `/flow` skill（`skills/flow/SKILL.md`）の中の LLM が担い**、
Rust バイナリ（`src/main.rs`、実質 123 行）は意図的に薄い scaffold である。バイナリの唯一の責務は
SessionStart hook（`flow propose`）で、開いている仕事があるセッションで `/flow` を能動的に提案する
propose-then-confirm ディレクティブを stdout へ注入することのみ。source（`compass` の次の一手 / `backlog`
キュー / `hypothesis` の PDO 仮説 / ユーザー直の課題文）から executor（`condukt`、`fugu-router` がモデル選択）
への束ね・止め時判断・自律ゲート・ロック取得はすべて skill 側の LLM 手順と既存バイナリに委ねられており、
flow バイナリはそれらの状態を一切持たない（新しい state store を作らない）。

## 不変条件

- **バイナリは薄い（single-subcommand scaffold）** — 公開サブコマンドは `flow propose` の1つだけ
  (`Command::Propose`)。main.rs の doc-comment が宣言するとおり「deterministic binary, LLM judges」。
  ループ制御・source 選択・executor 起動・検証・sink・ロックは一切バイナリに無く、skill 手順に存在する。
- **hook はターンを壊さない** — `propose` は stdout への書き込みと exit 0 のみ。backlog バイナリ欠落・
  非ゼロ終了・JSON parse 失敗はすべて fail-soft（`query_backlog_pending` が `None` を返す）で、
  driver hook として決してエラー終了しない。
- **backlog は soft dependency** — `query_backlog_pending` は `backlog list --project <cwd> --status
  pending --json` を叩き、成功時のみ結果を採用。`out.status.success()` が false なら `None`。backlog が
  無い環境でも静的 `DIRECTIVE` にフォールバックし、agent 自身の判断で `/flow` を surface できる。
- **open work が無ければ沈黙** — pending 件数が 0（`Some(items) if items.is_empty()`）のときは何も出さない。
  空キューのセッションで `/flow` の勧誘 chatter を出さないため。
- **propose-then-confirm（L2）** — hook はタスク数の再計算をせず、AskUserQuestion 1 回で `/flow` 起動可否を
  問うよう指示するだけ。「断られた／open work 無しなら再提案しない」ことをディレクティブ本文で明示する。
- **skill 側の直交・直列化不変条件（バイナリ外・SKILL.md が保証）** — source（compass/backlog/hypothesis）と
  executor（condukt）は state ディレクトリが独立。`/flow` は backlog ロックを共有してクロスセッション直列化し
  `/backlog` と同時に走らない（`/flow` は `/backlog` の上位互換）。build ≠ validate（仕様は出荷で
  `awaiting-measurement`、計測後に validate/reject）。どの早期脱出経路でもロック解放を必須とする。
- **PDO session anchor（skill 側・fail-soft）** — Step 3-1 で課題文を組み立てた直後、選んだ source 種別に
  よらず `overwatch begin --key <pdo-unit-id> --title <title> [--scope <csv>] [--done-criteria <dc>]` を
  呼び、セッションの現在の責務を project-wide レジストリ（`overwatch status`）に登録する（DESIGN §4.2）＝
  condukt run を起こさない measure step でも anchor が立つ。Step 4 で対応する `overwatch end --key <k>
  --status <done|abandoned>` を呼び anchor を閉じる（バッチは item ごとに begin/end）。両呼び出しは
  fail-soft — `overwatch` バイナリ欠落・呼び出し失敗時は skip して続行し、turn を壊さない（既存の
  condukt/backlog/compass 欠落時と同じ方針）。

## 振る舞い

### バイナリ（`src/main.rs`）

- **`flow propose`（唯一のサブコマンド）** — SessionStart（`startup|resume|clear`）hook 入口
  （`hooks/hooks.json` → `${CLAUDE_PLUGIN_ROOT}/bin/flow propose`, timeout 10）。`query_backlog_pending()`
  の3分岐で条件注入する:
  - `None`（backlog 欠落 or エラー）→ 静的 `DIRECTIVE`（英語・条件付き文言）を print。
  - `Some(empty)`（pending 0 件）→ 無出力。
  - `Some(N≥1)`→ `pending_directive(n, first_title)` で「pending N 件・最優先タイトル」を含む
    日本語サマリを print。agent はキューを再読せず AskUserQuestion 1 回で `/flow` を提案できる。
- **BacklogItem** — `backlog list --json` の要素を `title: String`（`#[serde(default)]`）だけ deserialize。
  件数と先頭タイトルのみ利用する最小構造。

### skill（`skills/flow/SKILL.md`、LLM が実行）

バイナリではなく skill 手順として存在する主ループ（Step 0〜4）。要点のみ:

- **Step 0 引数分岐** — `$ARGUMENTS` に課題文があれば source 選択を飛ばし condukt に1件だけ流して終了。
  空ならループ（source 自動ピック）へ。
- **Step 0.5 自律ゲート** — `condukt state autonomy-check`（既定 exit 1 = 非自律 = 全 Ask）。自律時は各
  human gate を `condukt policy answer` に risk×reversibility×confidence を添えて通し、auto/escalate/block の
  決定論的 verdict に従う（pivot 判断・deploy/push GATED・worker blocked は常に人間で止まる）。
- **Step 1〜4** — compass ゲート（charter 陳腐なら停止し `/compass` を促す）→ backlog ロック取得 →
  優先度順ピック（claim-skip + TOCTOU claim）→ **`overwatch begin`（PDO anchor 登録・source 種別によらず）**
  → `/condukt` → 検証 → sink（backlog done / compass outcome / hypothesis validate-reject / fugu-router
  record / claim release + heartbeat）→ ロック解放 + **`overwatch end`（anchor 解放）** + pivot-check。

### module 責務

- **`main` (`src/main.rs`)** — クレート全体が単一ファイル。`Cli`/`Command`（clap、`Propose` のみ）、
  `main`（parse → `propose()` へ dispatch）、`propose`（3分岐の条件注入）、`query_backlog_pending`
  （backlog サブプロセス起動＋fail-soft）、`pending_directive`（N≥1 用の日本語ディレクティブ生成）、
  `BacklogItem`（backlog JSON の最小デシリアライズ）、定数 `DIRECTIVE`（backlog 欠落時の静的英語
  フォールバック）で構成。専用モジュール分割は無い（scaffold ゆえ）。
- **skill / hook（非 Rust 資産）** — 実質のロジックは `skills/flow/SKILL.md`（source→executor ループ）と
  `hooks/hooks.json`（SessionStart→`flow propose`）に存在。plugin.json version は 0.1.9。
