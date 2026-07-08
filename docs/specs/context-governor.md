> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# context-governor 仕様

## 概要

`context-governor` は Claude Code 組み込みの compaction（自動要約圧縮）の**周りに被せる薄い制御層**で、
単一の hook-dispatch バイナリ（`src/bin/context-governor.rs`）として配線される。独自の lossy 要約器は
一切持たず、圧縮は組み込み compaction に委譲する。この層が足すのは4機能——**pin**（規範の常駐）・
**lossless-recall**（逐語情報の退避）・**retrieval**（参照本体のウィンドウ外への押し出し＋必要ターン注入）・
**tool-hygiene**（ツール結果の刈り込み）——だけである。設計の核心は触れる3軸を混同しないこと（`lib.rs`
doc-comment）——**size**（ウィンドウ占有）・**cost**（prefill/prompt cache）・**correctness**（規範の保全）。
アイテムは `types::Lane`（`Pinned`/`Verbatim`/`Evictable`）の3レーンのいずれかに属し、レーンが扱われ方の
唯一の真実になる。stdin でフックペイロードを受け取り `hook_event_name` で分岐、対応ハンドラを実行して
JSON エンベロープを stdout に書く（唯一の例外がアクション台帳を集計する read-only な `rollup` サブコマンド）。
API キー不要・subscription-native（hooks + binary）。

## 不変条件

- **I1 常駐（correctness）** — `Lane::Pinned` は常に最終コンテキストに存在すべき規範。compaction を越えて
  生き残らせるのは `StateRehydrator`（SessionStart で store から再注入）。
- **I2 逐語（correctness、型で表現不能化）** — `Lane::Verbatim` は決して lossy 圧縮されない。唯一の圧縮
  ハンドラ `ToolResultGroomer::groom` は `types::Evictable<'a>` しか受け取らず、`Evictable::new` は
  `Lane::Evictable` からしか構築できない（それ以外は `None`）。ゆえに `Pinned`/`Verbatim` を groomer に
  渡すコードはコンパイルが通らない。
- **I4 縮小のみ（size）** — groom の置換は入力より**厳密に小さい**。`DefaultGroomer::groom` は
  `trim_middle` 後に `trimmed.chars().count() >= body.chars().count()` なら `None` を返し窓を広げない
  （proptest `groom_never_grows_the_window` で網羅検証）。`Ref` body・under-budget は無改変。
- **ターンを壊さない** — ディスパッチ全体が `harness_core::hook::run_hook` の内側で走り panic を握り
  つぶして exit 0。空・不正ペイロードは `HookInput::parse` が `None` を返し無音 no-op（`{}`）。store
  オープン失敗（`open_store` が `None`）でも各イベントは silent no-op へ落ちる（fail-soft）。
- **ブロックできるのは PreCompact だけ** — `Dispatch::Block` は `PreCompact` の `CompactDecision::Block`
  のみが生み、bin が exit 2（Claude Code のブロック信号）。`Proceed` を含む他経路はエンベロープを書き
  exit 0。`Stop`/`SubagentStop` は出力を捨て決してブロックしない（`io::HookOutput` の doc: 連続8回
  ブロックでセッションが打ち切られるため checkpointer は副作用のみ）。
- **自前要約の不在** — `CompactionGuard` の既定は `CompactDecision::Proceed`。`handlers::DelegateToBuiltinCompaction`
  マーカ型の存在が「この経路は要約しない」を可読化する。`Block` はスナップショット確保が真に失敗した
  希少ケース用の予約（現状 `DefaultGuard` は常に `Proceed`）。
- **fail-soft な台帳** — `ledger` の全 I/O は best-effort でパニックしない。読み（`summarize_jsonl`/
  `was_injected`/`last_snapshotted_resident`）は欠落・破損で空・`false`・`None` を返し、書きは
  `harness_core::metrics::emit`（atomic）に委譲。`prune_jsonl` は sibling tempfile + atomic rename で
  過去分を安全に上限へ切り詰める。
- **決定論** — groom / inject / classify はトークナイザ非依存で決定論（~4 chars/token 見積り、ASCII
  語重複、モデル呼び出しなし）。台帳の `LedgerSummary.per_event` は `BTreeMap` でキー順安定。

## 振る舞い

hooks は `hooks/hooks.json` が6イベントを同一バイナリへ配線（`PostToolUse` は Read/Bash/Grep 等に
matcher、`SessionStart` は startup/resume/clear、各 timeout 10s）。`bin/context-governor.rs::dispatch` の分岐:

- **`PostToolUse` → `groomer()`（★主 size レバー）** — `DefaultGroomer::to_output` が `input.tool_response`
  を読み、live なウィンドウ圧（`harness_core::transcript::last_usage_tokens`）で `budget_for` により
  groom budget を縮め（圧が上がるほど厳しく、floor=default/4）、over-budget なら head/tail を残し中間を
  `…[context-governor: elided N chars of M]…` マーカで elide。groom 時のみ台帳へ `groomed{saved_tokens}` を
  1行追記し `updatedToolOutput` を返す。それ以外は `{}`。
- **`UserPromptSubmit` → `injector()`** — `DefaultInjector::inject` が `CONTEXT_GOVERNOR_REFERENCE_DOC`
  の参照 doc を読み、`inject_for` で heading×prompt の語重複最大セクションを選ぶ。score>0 なら該当
  セクション本体を、無ければ ToC（`Reference sections:` + heading 一覧）を `additionalContext` として
  プロンプトの**隣に**注入（置換しない）。`was_injected` による content-keyed dedup（同一テキストは
  再注入せず新行も書かない）。注入時 `injected` 行を追記。env 未設定・doc 空・見出し無しは no-op。
  加えて bin が `UserPromptSubmit` の注入サイズを `harness_core::inject_metrics::record` へ記録。
- **`SessionStart` → `rehydrator()`** — store を開けたら `DefaultRehydrator::rehydrate` が
  `SNAPSHOT_KEY` を recall し、`lane_aware_reinjection` で snapshot 本文を再 classify、resident レーン
  （`Pinned`/`Verbatim`）のセクションのみ `PIN_MARKER`（`[pinned]`）付きで再注入。`Evictable`
  （ReferenceBody）は resident 再注入から落とす。store 無し・snapshot 無しは `{}`。
- **`PreCompact` → `guard()`** — store を開けたら `DefaultGuard::on_pre_compact` が
  `snapshot_transcript` で transcript を確保、非空なら `snapshotted{to}` 行を追記して常に `Proceed`
  （store オープン失敗時もブロックしない）。
- **`Stop` / `SubagentStop` → `checkpointer()`** — store を開けたら `DefaultCheckpointer::checkpoint`。
  `stop_hook_active`（再入 Stop）は即 return。`last_usage_tokens >= threshold`（既定10_000、
  `CONTEXT_GOVERNOR_CHECKPOINT_THRESHOLD`）で snapshot を外部化。直前 snapshotted の resident から
  delta（既定5_000、`CONTEXT_GOVERNOR_SNAPSHOT_DELTA`）未満の成長なら同一バンドとみなし shed（無行）。
  出力は bin が破棄し常に `{}` exit 0。
- **`rollup`（サブコマンド、hook ではない）** — cwd の `ledger.jsonl` を `ledger::rollup` で集計し、
  `render_rollup` で rows / total_saved_tokens / per_event を read-only 表示。副作用なし。

## module 責務

- **`types`** — レーン/アイテム/spec 分類の型。`Lane`・`SpecClass`（`NormativeCore`/`ReferenceBody`）・
  `ContextItem`（`id`/`lane`/`tokens`/`body`）・`ItemBody`（`Inline`/`Ref`）・`Evictable`（I2 を表現不能
  にする capability トークン）・`StandingBudget`/`Overrun`（I3 常駐予算）。serde derive は持たず契約は
  serialization 非依存に保つ。
- **`handlers`** — 6ハンドラのトレイト seam（`ToolResultGroomer`/`ContextInjector`/`SpecClassifier`/
  `CompactionGuard`/`StateRehydrator`/`Checkpointer`）+ `BackingStore`（object-safe）+ `CompactDecision`。
  実装は `defaults/` に委譲し、bin は具象を名指ししない（Phase 3 で既存プラグイン wrapper へ差し替え可能な設計）。
- **`io`** — 出力エンベロープ `HookOutput`（`continue`/`systemMessage`/`hookSpecificOutput`）と
  `HookSpecific`（`additionalContext`/`updatedToolOutput`）。全 `None` は `{}` = proceed no-op。入力側は
  `harness_core::hook::HookInput` を再利用（結果フィールドは `tool_response`、`last_assistant_message` は
  無く transcript から読む——doc に明記）。
- **`ledger`** — append-only アクション台帳（`<state_dir>/ledger.jsonl`、budgetguard の日次台帳とは別物）。
  `Action`（`Injected`/`Groomed{saved_tokens}`/`Snapshotted`/`Pinned`/`Recalled`）を1行 JSONL で記録。
  `Ledger::append`（stat ゲート付き `prune_jsonl` GC、上限 `CONTEXT_GOVERNOR_LEDGER_MAX_ROWS` 既定
  50_000）・`rollup`/`was_injected`/`last_snapshotted_resident`（observe→act 用 read half）。
- **`backing`** — 既定 `BackingStore` = `TranscriptBackingStore`。`harness_core::store::context_state_dir`
  でセッションスコープの `state_dir` を導出（並列セッション安全）。`snapshot_transcript`（`recent_turns`
  で bounded 抜粋を単一 `SNAPSHOT_KEY`=`0x736e6170` slot へ）・`put`/`recall`（アイテム個別の lossless
  round-trip、private `ContextItemDto` 経由の serde）。全経路 fail-soft。
- **`defaults`** — 6 既定ハンドラ実装（`groomer`/`injector`/`classifier`/`guard`/`checkpointer`/
  `rehydrator`）+ bin が呼ぶコンストラクタ seam。`classifier` は ATX 見出しで spec を分割
  （`split_sections`、injector と共有）、見出しキーワード（`REFERENCE_KEYWORDS`、日英）で ReferenceBody を
  判定し `check_resident` で I3 常駐予算を照合。`guard`/`checkpointer` テスト用の env 直列化 mutex
  `acquire_env_lock` もここに置く。
