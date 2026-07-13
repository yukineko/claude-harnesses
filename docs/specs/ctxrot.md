> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# ctxrot 仕様

## 概要

`ctxrot` は Claude Code の長いセッションで進む context-rot（序盤の指示・決定・未消化 todo の埋没、巨大
ダンプによる窓の希釈）を、フックに配線した単一 Rust バイナリで**検知・退避・復元・蒸留・制御**する
ハーネスである。設計の中核は役割分担: フックは「速くて決定論的・LLM 不使用の安全網」、`/distill` スキルと
async worker（`distill-bg`）は「必要時に走る LLM 品質の要約」。各フックは `ctxrot` のサブコマンドで、フック
JSON を stdin から読む（`harness_core::hook::run_hook`）。フックは3つの状態を操作する——note store（退避
テキスト）、per-project loadset（`pin`/`drop` の policy）、metrics.jsonl（計測）——で、状態はセッションタグ
/ project_key で並列セッション安全に分離される。フックは既に載ったトークンを evict できないため、制御は
「入口で止める/絞る」（preguard・`/ctx load`）と「再構成で実効化する」（drop は `/compact`・`/distill`・
新セッション carryover で効く）の2点に集約される。

## 不変条件

- **フックは決してターンを壊さない（ハード不変条件）** — hook 系（guard/rescue/restore/preguard/toolguard/
  stop/distill-bg/statusline）は parse 失敗・config 欠落・IO エラーのいずれでも無出力 exit 0。user 起動系
  （install/init/note/metrics/eval/ctx/usage）のみ通常のエラー報告と非零 exit を持つ。
- **LLM 非依存の決定論フック** — rescue/guard/preguard/toolguard/restore/stop は正規表現・サイズ・帯計算だけで
  動き LLM を呼ばない（PreCompact の 10s タイムアウト内で安全）。LLM 品質は `/distill` と detached worker のみ。
- **グローバル kill-switch** — `GUARD_DISABLE` 非空で全 hook が即 return（`Config::disabled()`）。distill child
  にも `GUARD_DISABLE=1` を渡し child 内でフックは再発火しない。
- **助言自体が rot 源にならない** — guard は帯をまたいだ時だけ注入し、per-turn 出力を `guard_inject_max_chars`
  （既定 1200・CJK-safe）で cap。超過時は Anchor→Advice→Safety の順にブロックを落とす（safety 警告は消さず
  truncate）。toolguard の nudge も per-key seen-state ＋ `toolguard_nudge_cap`（既定 3・0=無効）で上限。
- **予算メーターは実窓ではなく「目標 cap」** — 帯・% は `est_tokens / context_window`（`usage::pct_from_tokens`）
  で測る。`context_window`（既定 200000）は実 model 窓ではなく抑えたい目標値。stop hook の auto-compact 閾値も
  この予算メーターで判定し、生 `used_percentage` ではない（真の ~1M 窓でも正しく発火。0.5.0 で意味変更）。
- **並列セッション安全** — note は `session_tag` 付き名で書かれ、restore/anchor は `latest_note_for_session` で
  自セッション自身のノートに前方一致で戻る。loadset は project_key ごと1ファイル。state
  （`.band`/`.anchor`/`.session_anchor`/`.compact-band`/`.distilled`）は `safe_session` で名前安全化。
- **帯は escalate-only だが ratchet ではない** — 帯昇格・reanchor cadence・compact-band は同一帯で再発火せず、
  `/compact` で使用率が下がると緩められ再上昇で再発火する。stop は加えて `stop_hook_active` 再入ガードを持つ。
- **distill contract** — restore が引き継ぐ見出し（`決定事項/Decisions`・`残課題/Open todos`）を欠くノートを
  `note write --require-sections` は拒否（exit 1・未書き込み）。async worker の `finalize_note` も欠けた必須
  見出しを空節で補い carryover が黙って空になるのを防ぐ（残3節は warning のみの soft contract）。
- **detached worker の分離** — `distill-bg` は `nohup … &` で init に reparent、`distill_timeout_secs`
  （既定 180）で kill。secret 系 env（`ANTHROPIC_API_KEY` 等5種）を除去して spawn し API キーを漏らさない。
- **preguard は狭く設計** — サイズゲートは `limit` 無し `Read` かつ `gate_file_bytes`（既定 1MB）以上のみ、
  優先順位 deny→limit→allow→size。Bash ゲートは opt-in（`gate_bash` 既定 false）で下流バウンド無しの明白な
  ダンプだけ deny。

## 振る舞い

hook サブコマンド（hooks.json / `install` が配線する 6 フック）:

- **`guard`（UserPromptSubmit）** — 5 ブロックを cap して注入: (1) `.distilled` marker を消費して蒸留結果を
  一度だけ再注入（`/compact` 後の唯一の再注入路）、(2) 大きい参照検知（巨大ファイル・URL・「全文」系語）、
  (3) 予算帯 50/75/90% を帯またぎ時だけ escalate、(4) reanchor（`reanchor_min_band` 既定 2 以上・
  `reanchor_every_prompts` 既定 8 ごと）で自セッションの Decisions/todos を末尾に再浮上、(5) **PDO session-anchor**
  （§4.3・DESIGN-pdo-session-anchor.md）——決定事項 re-anchor とは**別トラック**で、`overwatch lease --session
  $CLAUDE_CODE_SESSION_ID --json` で読んだ live lease があれば「今の担当（title + done_criteria の 1〜2 行要約。
  scope の生 glob は注入しない）」を band ≥ 1 から `anchor_reinject_every`（既定 12。決定事項より疎）ごとに再浮上。
  専用クールダウンファイル（`<safe>.session_anchor`）で決定事項トラックと干渉しない。lease 無し・overwatch 欠落・
  JSON parse 不能なら無音 no-op（fail-soft）。両 anchor ブロックとも `Prio::Anchor` として `guard_inject_max_chars`
  （既定 1200）の per-turn cap を共有する。band ≥ 2 で先行 rescue を書き、最上位帯で `spawn_for_band` の
  async distill を先回り起動。
- **`rescue`（PreCompact）** — `/compact` 直前に直近 transcript（60 ターン×1200 字）から決定/todo/ファイル/
  リンク/生ターンを抽出し `rescue-<tag>-<ts>.md` を書く。`band-NN%` 先行退避は `rescue_coalesce_secs`
  （既定 120）で coalesce するが `precompact`/auto はしない。続けて `spawn_detached` を fire-and-forget。
- **`restore`（SessionStart: startup|resume|clear）** — 最新 rescue/distill ノート（`use-note` 固定優先）から
  簡潔な carryover（Decisions＋todo＋リンク）を注入。全文は inline せずポインタのみ。`source == "compact"` では
  注入しない。pinned loadset（最大 12）を併記。rescue-only なら `/distill` を1行促す。
  `inject_decisions/todos/pinned`・`restore_enabled`（`CTXROT_RESTORE_DISABLE=1` で切）で個別制御。
- **`preguard`（PreToolUse: Read|Bash）** — (1) `load_deny` 一致 `Read` をサイズ無関係に deny（既定で `limit`
  スライスも拒否）、(2) `load_allow` 一致はサイズゲート bypass、(3) `limit` 無し・`gate_file_bytes` 以上を
  deny。deny は `permissionDecision:"deny"` JSON で返す（理由が唯一のステアリング路）。opt-in Bash ゲートは
  無制限ダンプ形（`cat`/`journalctl`/recursive grep/`tail -n +K`/`dmesg`）かつ下流バウンド無しを deny。
- **`toolguard`（PostToolUse: Read|Bash|Grep|Glob|WebFetch|BashOutput|NotebookRead）** — 出力が
  `huge_tool_output_bytes`（既定 50KB）以上なら (1) head/tail 省略で truncate（常に発火）、(2) 次の重い読みを
  sub-agent 経由にする nudge（seen-state＋cap で抑制）。ツールはブロックしない。
- **`stop`（Stop）** — opt-in（`auto_compact_enabled` 既定 false）。予算メーター使用率が
  `auto_compact_at_percentage`（既定 0.90）超かつ新規上昇帯なら `{"decision":"block"}` で `/compact` を促す。
  `stop_hook_active` 再入と同一帯は block しない（ターンを恒久的に塞がない）。

user 起動サブコマンド: **`install/uninstall [--dry-run]`**（settings.json に 6 フックを冪等マージ／除去、legacy
`context-rot-guard.py` を strip、書込前 backup）／**`init`**（`~/.ctxrot/config.toml`＋store/state dir を
scaffold・既存は不上書き）／**`ctx <pin|unpin|drop|undrop|list|pinned|dropped|use-note|clear-note>`**（loadset
操作。pin/drop 排他、`/ctx` スキルの裏）／**`note <list|latest|dir|write|prune>`**（store 点検・書込・GC。
`write --require-sections` で contract 強制、`prune` は `keep_notes_per_project` 30＋`keep_distill_min` 10）／
**`usage`**（使用率メーター＋band hint、`/distill` の使用率連動判断用）／**`statusline`**（常時メーター。
プラグインで自動登録されず settings.json に手動追記が要る）／**`metrics <summary|path|compare|peak>`**／
**`eval <gen|score>`**（reanchor の recall 効果をオフライン測定・決定論）／**`distill-bg`**（hidden・内部。
rescue/band から fire される detached worker。pre-compaction transcript を `claude -p` で蒸留し `distill-*`
ノート＋`.distilled` marker を書く。直接呼ばない）。

### module 責務

- **`main`** — clap CLI/サブコマンド定義。hook を `run_hook` で包み `Config::disabled()` 確認→stdin 読み→
  `hooks::*::run` を呼び出力整形（plain 注入 / deny JSON / hookSpecificOutput）。`init`／`SAMPLE_CONFIG`／
  `ctx_mutate`／`statusline_from`。
- **`config`** — `Config`／on-disk `FileConfig`／defaults／`~/.ctxrot/config.toml`／env override
  （`GUARD_*`・`CLAUDE_CONTEXT_WINDOW`・`CTXROT_*`）。`band_for`／`disabled()`／bands の sanitize。
- **`hooks::guard`** — UserPromptSubmit 中核。`Prio`＋`cap_blocks`（injection cap）、`check_large_references`、
  `check_context_budget`（帯昇格＋先行 rescue＋band distill）、`check_reanchor`、`check_session_anchor`
  （§4.3 PDO anchor: `fetch_session_lease`／`render_session_anchor`／専用 `.session_anchor` クールダウン）、
  `check_distilled`、`safe_session`。
- **`hooks::rescue`** — PreCompact 決定論退避。`write`（coalesce）／`extract`／`render_note`。先行 rescue と共有。
- **`hooks::restore`** — SessionStart carryover。`note_carryover`／`pinned_carryover`／`extract_section`
  （anchor/distill と共有）、distill contract の single source（`REQUIRED_SECTIONS`／`missing_sections` 他）。
- **`hooks::preguard`** — PreToolUse 予防ゲート。`check_read`（deny→limit→allow→size）と opt-in `check_bash`
  （`unbounded_dump_kind`／`has_bound`／`is_full_tail`／`redirects_out` の保守的ヒューリスティック）。
- **`hooks::toolguard`** — PostToolUse。`truncate_response`（head/tail）／`response_text[_len]`（多形抽出）／
  seen-state＋cap の nudge ゲート。
- **`hooks::stop`** — Stop auto-compact nudge。予算メーター判定・`.compact-band` で bounded・再入回避。
- **`hooks::distill`** — feature ④ の detached distiller。`spawn_detached`／`spawn_for_band`／`fire`（nohup）／
  `run_bg`（モデル実行→`finalize_note`→note＋marker）／`run_model`（secret env 除去・timeout kill）／`marker_path`。
- **`loadset`** — per-project loadset（`pinned`/`dropped`/`preferred_note`）を `<state_dir>/
  loadset-<project_key>.json` に永続化。pin/drop 排他、best-effort read（欠落/破損で空）。
- **`usage`** — statusline/usage 共有の1行メーター（`line`／`bar`／`color`／`hint`）、`pct_from_tokens`、
  `find_transcript_for_session`。
- **`glob`** — load allow/deny 用の依存無し path-aware グロブ（`*`/`?`/`**`/`**/`）→正規表現。オフライン
  ビルド維持のため globset を避けた自前実装。
- **`metrics`** — append-only JSONL（budget/rescue/restore/gate/tooldump/inject/nudge/distill/anchor）。`emit`／
  `summarize`／`nudge_state`（toolguard seen-state）／`group_by_prefix`。injection budget（ADR 0001）の seed。
- **`install`** — settings.json への 6 フック冪等マージ／除去。legacy Python guard を strip。
- **`eval`** — reanchor の recall 効果をオフライン測定（`run_gen`／`run_score`／`Case`）。決定論・unit-tested。
