> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# scout 仕様

## 概要

`scout` は **skill-only プラグイン**である。Cargo.toml も `src/` も `hooks/` も無く、正典は
`skills/scout/SKILL.md`（`/scout` コマンド）と `.claude-plugin/plugin.json`（`version 0.1.2`）の 2 ファイルのみ。
自前バイナリを持たず、状態も持たない（subscription-native, API キー不要）。

scout はプロジェクトを **5 つのレンズで並列偵察して「施策（タスク）」を生成する SOURCE** である。
決定論的にプロジェクト状態を収集し（Phase 1）、5 レンズ（現在の課題 L1 / セキュリティ L2 / 業界・他プロジェクト標準
L3 / 不足施策 L4 / 安全性・堅牢性 L5）に read-only sub-agent を並列展開し（Phase 2）、返ってきた逐語証拠つきの
施策候補を統合・重複排除・スコアリングし（Phase 3）、HOTL 合意を経て（Phase 4）承認された施策だけを backlog へ
書き出し（Phase 5）、`/flow` へ実行を引き渡す（Phase 6）。判断（監査・施策選別）は LLM が、保存は外部
`backlog` バイナリが、実行は `/flow`・`condukt` が担う。compass が単一ゴールの勾配（一手に絞り残りは parked）なら、
scout は広域偵察で複数の独立した施策を一度に挙げる相補的 SOURCE で、両者とも backlog→/flow→condukt の同じ
executor に合流する。

## 不変条件

- **監査は read-only** — scout も sub-agent もファイルを一切編集しない。Phase 2 は `Explore` 等の**編集権を持たない
  型**の read-only sub-agent を使い、施策の実行は flow/condukt に委ねる。`allowed-tools` も Read/Grep/Glob/WebSearch/
  WebFetch と read-only な `git`/`cargo`/`cargo-deny`/`ls`/`rg`/`grep`、および `backlog`/`compass`/`specguard`/`condukt`
  CLI に限られ、Edit/Write は含まれない。
- **証拠のない施策は採用しない（幻覚防止）** — 各施策は逐語引用 or `file:line` 参照 or Web ソース URL を必ず持つ。
  sub-agent には「実装提案ではなく課題の発見に徹し、証拠の無い項目は出さない」と厳命する。Phase 3 の証拠フィルタで
  evidence の弱い候補を落とす。
- **書き込みは `backlog add` のみ** — scout はループもロックも持たない。repo への書き込みは無い。直列化が要る実行は
  `/flow`（backlog ロック）に委ねる。並行セッションの重複回避は repo 外の discovery store 経由（下記）。
- **合意（HOTL）を経る** — backlog に積む施策の確定は既定で `AskUserQuestion`（multiSelect）。勝手に全件は積まない。
  例外は `condukt state autonomy-check` が autonomous を返したときのみで、その場合も採用 top-N をサマリで明示し
  （静かな全件採用の禁止）、`--dry-run` は autonomy でも Phase 4 で必ず停止する。
- **fail-soft / never break a turn** — Phase 1 の決定論収集は全て「失敗は無視」。`compass score` バイナリ欠落・
  非 0 終了・`score` 未対応なら LLM 手計算（`(severity × goal 近さ) ÷ effort`、L2/L5 重み上げ）へフォールバック。
  discovery store 呼び出しが失敗しても scout は続行する。`backlog` 不在なら施策を Markdown 提示に切替。WebSearch
  不可なら L3 をスキップし「業界標準は未調査」と明示。
- **スコアリングは決定論バイナリに委譲** — 算術ランキングを LLM が手計算せず `compass score --severity --effort
  --lens --goal-proximity` に委ね、LLM は `goal_proximity`（0.0〜1.0 の意味判断）だけを見積もる。L2/L5 の重み上げは
  スコアラー側に内蔵済み。

## 振る舞い

`/scout [任意: スコープ/レンズ絞り込み / --dry-run]` が固定パイプラインを回す。

- **Phase 0 — スコープ受領** — 引数無し→repo 全体・5 レンズ、`security のみ`→指定レンズ、`crates/condukt`→サブツリー、
  `--dry-run`→Phase 3 提示で停止し backlog に積まない。`condukt --version` でハーネス存在を確認。
- **Phase 0.5 — スコープ規模でレンズ縮約（コスト・ゲート）** — 追跡ファイル数 `N`（`git ls-files … | wc -l`）で
  既定レンズを決定論的に縮約する。**小**（`N ≤ 10`）→L1+L5・L3 省略、**中**（`10 < N ≤ 80`）→L1+L2+L4+L5・L3 省略、
  **大**（`N > 80`）→L1–L5 全開・L3 実施。severity 系の L1/L5 は常に残す。測定失敗時は「大」とみなす。明示レンズ絞りが
  最優先、依存更新/認証変更等では小・中でも L2 追加。縮約は Phase 3 サマリで理由を明示（静かな打ち切り禁止）。
- **Phase 1 — 決定論的レビュー** — `git log`/`git status`/`cargo test`/`compass gap`/`backlog list`/
  `specguard prompt --json`/`cargo deny check advisories`/`ls .deepwiki/*.md` を read-only 収集（失敗は無視）し、
  REVIEW コンテキストとして全 sub-agent へ渡す（各 agent の重い再収集を防ぐ）。compass の `measuring_stick` があれば
  スコアリング基準に最優先採用。
- **Phase 2 — 5 レンズ並列調査** — `Task` で read-only sub-agent を 1 メッセージで並列起動（Phase 0.5 で選ばれた
  レンズ分のみ）。各 agent は REVIEW コンテキスト＋レンズ定義を受け、施策候補 JSON 配列（`title`/`lens`/`rationale`/
  `evidence`/`severity`/`effort`/`suggested_done`）**のみ**を返す。L3 は他プロジェクトが現にやっている根拠 URL 必須。
- **Phase 3 — 統合・重複排除・スコアリング** — 別レンズが挙げた同一施策を 1 件に畳み `lens` 併記、証拠フィルタで
  evidence の無い候補を落とし、`compass score` で降順ランキング、`p0`/`p1`/`p2` を付与。
- **Phase 4 — 合意（HOTL）** — 選別ゲートを `condukt state autonomy-check`→`condukt policy answer` の shim に通す。
  非 autonomous（既定）・未対応（exit 127）は `AskUserQuestion`（multiSelect, 既定 8〜12 件）で選ばせる。autonomous は
  policy-answer verdict で分岐（auto=top-N 自動採用／escalate=Ask／block=停止）。`--dry-run` は必ずここで停止。
- **Phase 5 — backlog へ書き出し** — 承認施策を 1 件ずつ `backlog add --tag scout`（証拠と完了条件を `--notes` に記録、
  condukt interpreter が done_criteria を引ける）。同時に discovery store（`~/.compass/<project_key>/discovery.jsonl`、
  repo 外 side-channel）を確認し、他セッションが既に発見した同名施策を skip、本セッション分を record（fail-soft）。
- **Phase 6 — 実行引き渡し** — Phase 4 と同じ autonomy スイッチで分岐。非 autonomous は `/flow` を propose-then-confirm
  で提案、autonomous は 1 件以上積んだときのみ `/flow` を自動起動（0 件なら空ループ防止で起動せず）。いずれも実行ループ・
  backlog ロックは flow の責務で、scout は起動後に退く（併走しない）。

## 構成

- **`skills/scout/SKILL.md`** — 唯一の実行ロジック。`/scout` コマンド本体。frontmatter が `name`/`description`/
  `argument-hint`/`allowed-tools`（read-only ツール集合）を宣言し、本文が上記 Phase 0〜6 の固定パイプラインと不変条件・
  失敗モード表（証拠なし多発→レンズ絞り再調査 1 回／候補 0→健全と報告／`backlog` 不在→Markdown 提示／WebSearch 不可→
  L3 省略／`--dry-run`→Phase 4 停止）を規定する。判断は LLM、調査は read-only sub-agent が担う。
- **`.claude-plugin/plugin.json`** — プラグイン manifest（`name: scout`, `version: 0.1.2`, keywords）。フックも同梱
  バイナリも宣言しない（skill-only の裏付け）。
- **`README.ja.md` / `README.md`** — 目的・必要性・使い方の散文。scout が状態を持たず書き込みは backlog add のみである
  こと、PATH 上の外部 `backlog`/`compass`/`condukt` バイナリに依存すること（同梱しない）を明記する。
- **依存する外部バイナリ（scout は同梱しない）** — `backlog`（施策の保存）、`compass`（`score` でスコアリング、
  `gap`/`discovery` で REVIEW・重複回避）、`condukt`（`state autonomy-check`/`policy answer` の autonomy shim、実行）、
  `/flow`（source→executor ループ）。いずれも欠落時は fail-soft で縮退する。
