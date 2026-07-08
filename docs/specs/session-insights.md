> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# session-insights 仕様

## 概要

`session-insights` は Claude Code の 1 セッション単位の作業メトリクス（ターン数・ツール
呼び出し数・触れたファイル数・ツール構成）を決定論的に集計するハーネス。`Record`（PostToolUse
hook）が各ツール呼び出しを、`Stop`（Stop hook）がターン完了を状態ファイルに刻み、`Report` が
サイズクラス（XS–XL）・作業カテゴリ・上位ツールへ集約する（API キー不要の Devin Session Insights
相当）。加えて opt-in で Obsidian vault へ 2 種のノートを書く: `Stop` 時の簡潔な `sessions/` ノート
（`obsidian::write_note`）と、SessionEnd / on-demand の AEGIS 風 record ノート（`record::write_record`）。
`RecordNow`（`/record` コマンドの裏）はセッションの record ノートを（再）生成しパスを印字する。数値
（コスト/トークン/ターン/ツール/ファイル）は本ツールが自動充填し、散文は後段のモデル駆動パスが埋める。

## 不変条件

- **machine-owned ブロック非改変**: record ノートの `## コスト`・`## 数値サマリ` は HTML コメント
  マーカー（`COST_START/COST_END`・`NUM_START/NUM_END`）で囲んだ機械所有ブロック。`record::merge`→
  `replace_block` は既存ノートに対しこの 2 ブロックの中身のみ差し替え、散文セクション（`<!-- fill: … -->`
  placeholder を含む）は決して上書きしない（append-merge, never overwrite）。マーカー不在なら
  `replace_block` はテキストを無改変で返す（モデルによる意図的な再構成を壊さない）。
- **fail-soft / 非ブロッキング**: hook（`record`/`stop`/`sessionend`）は `run_hook`/`run_hook` 経由で
  常に exit 0。stdin パース失敗（`HookInput::parse` が `None`）・空 `tool_name`・無効 config は静かに
  return し、ターンをブロックしない。ノート書き込みは全て `.ok()` 系で失敗を握り潰す（`write_record`・
  `write_note`）。書き込み先は自身の state dir と（opt-in の）vault のみ。
- **決定論的数値充填**: サイズクラス（`Session::size`）・カテゴリ（`Session::category`）・上位ツール
  （`Session::top_tools`）・数値サマリ（`record::numeric_body`）・コスト（`record::cost_body`）は
  記録済みカウントと transcript 集計のみから算出し、LLM を介さない。同一入力で同一出力。
- **vault を新規作成しない**: `write_record`・`write_note` は `obsidian_vault.is_dir()` が真のとき
  だけ書く。存在しなければ no-op。
- **kill switch / disable**: 環境変数 `SESSION_INSIGHTS_DISABLE`（`Config::disabled_env`）または
  `cfg.enabled == false` で全 hook が即 return。
- **session identity の pin**: `Session::ensure` は初回のみ `session_id`/`project`/`cwd`/`started_at`
  を確定し以後上書きしない（mid-session の `cd` でも record ノート名が揺れない）。更新は `last_at` のみ。
- **ignore_tools / 空 tool 除外**: `Config::is_ignored`（既定 `TodoWrite`）・空 `tool_name` は集計しない。
- **gauge 正典優先・transcript フォールバック**: `record::session_models`/`session_agents` は gauge の
  永続 `SessionRecord`（`session::load_one`）を優先し、無ければ `estimate_transcript_cost` で再集計
  （triple-parse 回避と hook 順序レースへの堅牢性）。

## 振る舞い

- `Record`（PostToolUse hook）: stdin から `HookInput` を読み、`cfg.enabled` かつ非空・非 ignore な
  `tool_name` のときのみ `Session::record_tool` で `tool_events`・per-tool カウント・distinct files
  （`target()` 由来）を加算し `metrics::save`。
- `Stop`（Stop hook）: `Session::record_turn` で turn を +1 し保存後、`obsidian::write_note`（`obsidian_log`
  有効かつ vault ありのとき `sessions/<date>-<short>.md` を YAML frontmatter 付きで毎回上書き）。
  `cfg.record` 有効なら transcript を探索し `record::write_from_session` で record ノートも更新。
- `Sessionend`（SessionEnd hook）: `cfg.record` のときのみ動作。`harness_core::hook::HookInput`
  （`transcript_path` を持つ）を `read_stdin_if_piped` で読み、永続 rollup を優先しつつ無ければ
  transcript 集計（`harness_core::usage::aggregate`）で turn を補完し `write_from_session`。
- `RecordNow { session }`（`/record` の裏）: `cfg.record` を強制 true にしセッション id を
  引数→`CLAUDE_CODE_SESSION_ID`→`_local` の順で解決。`find_transcript` で `~/.claude/projects/*/<id>.jsonl`
  を探し record ノートを（再）生成、成功時はパスを stdout、vault 不在時は説明を stderr へ。常に exit 0。
- `Report { session, all, context }`: `--all` は全 rollup を 1 行サマリ（`metrics::load_all`、`last_at`
  降順）。単一時は `--session` prefix 一致（`metrics::find`）または最新（`metrics::latest`）を
  `Session::render_report` で表示し、`--context` 指定時は context-governor 台帳を best-effort で合流
  （`context::summarize`/`render_block`、不在時「no context ledger」）。
- `Install`/`Uninstall { dry_run }`: `~/.claude/settings.json` に PostToolUse+Stop hook を merge / 除去。
- `Init`: state dir を作成。`Status`: 解決済み config（有効値・閾値・vault・record 設定・記録セッション数）を表示。
- 記録が無い場合の出力: `--all` は `(no sessions recorded yet)`、単一は `(no matching session …)`。
