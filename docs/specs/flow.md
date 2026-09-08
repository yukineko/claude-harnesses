> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# flow 仕様

## 概要

`flow` は「課題の供給（source）→ 解決手段の実行（executor）」を1本のループで貫く統合 driver
（autopilot 層）である。**ループの本体は `/flow` skill（`skills/flow/SKILL.md`）の中の LLM が担う。**
source（`compass` の次の一手 / `backlog` キュー / `hypothesis` の PDO 仮説 / ユーザー直の課題文）から
executor（`condukt`、`fugu-router` がモデル選択）への束ね・止め時判断・自律ゲート・ロック取得はすべて
skill 側の LLM 手順と既存バイナリに委ねられており、flow 自身は状態を一切持たない（新しい state store を
作らない）。

**0.2.7 以降、flow は skills-only plugin である**（`scout` / `daily-report` と同じ形）。Rust バイナリ・
launcher・hook・Cargo パッケージは存在しない。0.2.6 までは薄い scaffold バイナリが 1 サブコマンド
`flow propose` だけを持ち、SessionStart hook で「開いている仕事があれば `/flow` を propose-then-confirm で
提案せよ」という L2 ディレクティブを注入していたが、**2026-08-20 にユーザーの指示で廃止した**
（autoflow 側の同種の 2 経路 — SessionStart 提案と Stop hook の backlog arm — も同時に撤去）。
`propose` がバイナリの唯一のサブコマンドだったため、バイナリ・launcher・hooks.json・Cargo.toml は
すべてそれと共に消えた。

## 不変条件

- **hook を持たない — `/flow` はユーザーが明示的に起動したときだけ走る。** backlog に pending が
  積まれていること自体は起動理由にならない。「キューを流すか」は operator の判断であり、`/flow` と
  打つことがその判断である。
- **こちらから `/flow` を提案しない（skill 本文が明文で禁止）** — 廃止したディレクティブを推測で
  再現するのは撤去そのものの無効化になるため、`skills/flow/SKILL.md` は「自分から `/flow` を提案しては
  ならない」を明示的な指示として持つ。散文の注意書きではなく、skill が読ませるプロンプト本文に置く。
- **判定を持たない** — flow は block/allow を返す経路をひとつも持たない（hook が無いので入口が無い）。
  したがってゲート不変条件（判定不能→制限側）の対象外であり、CLAUDE.md §1 の carve-out 側に属する。
  廃止したのは *提案* であって *判定* ではない — `flow propose` の沈黙を「キューが空」と読む下流は
  存在しなかった（沈黙は「提案しない」だけを意味した）ので、撤去は fail-open を作らない。
- **skill 側の直交・直列化不変条件（SKILL.md が保証）** — source（compass/backlog/hypothesis）と
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

### skill（`skills/flow/SKILL.md`、LLM が実行）

plugin の実体はこの skill 手順だけである。主ループ（Step 0〜4）の要点:

- **Step 0 引数分岐** — `$ARGUMENTS` に課題文があれば source 選択を飛ばし condukt に1件だけ流して終了。
  空ならループ（source 自動ピック）へ。
- **Step 0.5 自律ゲート** — `condukt state autonomy-check`（既定 exit 1 = 非自律 = 全 Ask）。自律時は各
  human gate を `condukt policy answer` に risk×reversibility×confidence を添えて通し、auto/escalate/block の
  決定論的 verdict に従う（pivot 判断・deploy/push GATED・worker blocked は常に人間で止まる）。
- **Step 1〜4** — compass ゲート（charter 陳腐なら停止し `/compass` を促す）→ backlog ロック取得 →
  優先度順ピック（claim-skip + TOCTOU claim）→ **`overwatch begin`（PDO anchor 登録・source 種別によらず）**
  → `/condukt` → 検証 → sink（backlog done / compass outcome / hypothesis validate-reject / fugu-router
  record / claim release + heartbeat）→ ロック解放 + **`overwatch end`（anchor 解放）** + pivot-check。

### バイナリ（廃止済み・0.2.7 で削除）

存在しない。0.2.6 までの `flow propose` は SessionStart（`startup|resume|clear`）hook 入口として
`backlog list --project <cwd> --status pending --json` を叩き、3分岐（backlog 欠落/エラー → 静的英語
ディレクティブ、pending 0 件 → 無出力、N≥1 → 件数と最優先タイトル入りの日本語サマリ）で条件注入して
いた。廃止理由は「繰り返しは検出ではない」— 2 つの plugin が同じ依頼を、片方は毎ターン出しており、
10 回目の「N 件あります」は 1 回目に無い情報を何も運ばない。

## リポジトリ資産

skills-only plugin なので `src/` も `bin/` も `hooks/` も無い:

- `.claude-plugin/plugin.json` — plugin manifest（version 0.2.7）。
- `skills/flow/SKILL.md` — source→executor ループの実体。
- `README.ja.md` / `README.md` — 「The hook, retired」節に廃止の経緯と理由。
- `crates/integration-tests/tests/flow_skill_queue_contract.rs` — **crate 削除時に消さず移設した**唯一の
  テスト。SKILL.md をテキストとして読み、「ループが排他 project ロックを取らない」「ピックは読むだけで
  なく予約する」を pin し続ける（`crates/flow/tests/` から `git mv`）。
