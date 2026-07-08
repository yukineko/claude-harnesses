> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# playbook 仕様

## 概要

`playbook` は「プロジェクト知識の蓄積＋注入」を担う単一バイナリ Rust CLI 兼 Claude Code の
**UserPromptSubmit** フックである（Devin の Knowledge をローカルフックとして再現）。プロジェクト固有の
事実を 1 件 1 件の**アトミックノート**（TOML frontmatter を `+++` フェンスで囲んだ Markdown）として
`<store>/<basename>-<hash8>/`（プロジェクト）と `<store>/_global/`（横断共有）に蓄える。プロンプトごとに
`inject` サブコマンド（`main::inject`）が可視ノートを読み、プロンプトに対して決定論的にスコアリングし
（`retrieve::score`）、上位ノートを char 予算内で追加文脈として stdout へ出す。埋め込みも API キーも使わず、
キーワード×トリガーのスコアリングのみでサブスクリプションで完結する。注入経路が背骨であり、他サブコマンド
（`add`/`list`/`search`/`rm`/`install`/`uninstall`/`init`/`status`）は同じ store と設定を扱うキュレーション
ツールにすぎない。プラグイン配線は `hooks/hooks.json` が `${CLAUDE_PLUGIN_ROOT}/bin/playbook inject` を
UserPromptSubmit に起動する（`timeout` 10 秒）。

## 不変条件

- **フックはターンを壊さない（ハード不変条件）** — `inject` は `harness_core::hook::run_hook` 配下で走り、
  panic を握りつぶし常に exit 0。壊れた/空 stdin、store 欠落、関連ノートなしのいずれでも黙って無出力
  （`main::inject` の early-return 群：`HookInput::parse` が None、`notes.is_empty()`、`chosen.is_empty()`）。
  「プロンプトを止める知識フックは黙るフックより悪い」という方針。
- **キルスイッチ** — `PLAYBOOK_DISABLE` が空でも `0` でもない値のとき `Config::disabled_env()` が真になり
  `inject` は即 return（no-op）。加えて config の `enabled=false` でも return。
- **決定論** — スコアリングは term-overlap のみで埋め込み・外部 API に非依存。`retrieve::select` は
  `b.score.cmp(&a.score).then(a.note.slug.cmp(&b.note.slug))` で **slug 安定ソート**するため、同一プロンプトに
  同一結果を返す。`store::read_dir_notes` も slug 昇順で読む。
- **char 予算** — 注入総量は `max_chars`（既定 1500、`Config::load` で下限 120 に clamp）を超えない。予算は
  `harness_core::inject::CharBudget` が管理し、各ノートの寄与は `Note::injected_len`（title + body の文字数 + 8）で計上。
- **`always` ノートの不可欠性** — `always` ノートは `min_score` と char 予算の**両方をバイパス**して全文注入される
  （規範ノートは決して切り詰め・脱落しない）。ただし予算は charge されるので、その overrun は後続の scored
  （非 always）ノートから削られる（`retrieve::push` の `force` 分岐、テスト `always_notes_exempt_from_budget` /
  `budget_drops_only_non_always`）。
- **ノートの scope は置き場所が正典** — `Meta::scope`/`created` frontmatter は情報用。実際の project/global 帰属は
  ファイルがどの store dir に在るかで決まる（`store::read_dir_notes` の `global` 引数）。
- **install の冪等・非破壊** — `install`/`uninstall` は `~/.claude/settings.json` の自グループ（command に
  `"playbook"` を含む＝`MARKERS`）だけを識別・置換し、他プラグインのフックグループは温存、書き込み前にバックアップ
  （`harness_core::install` に委譲）。プラグイン経路では `hooks/hooks.json` を使うので install は standalone
  `cargo install` 用。

## 振る舞い

サブコマンドは clap の `Command` enum（`main.rs`）で定義。`inject` 以外は cwd を root とし
`Config::load` → `Store::new` → `Store::load_visible` を土台にする。

- **`inject`（UserPromptSubmit フック）** — `Config::disabled_env` チェック → stdin を `read_stdin` で読み
  `HookInput::parse` → `cwd_or_current` で root 解決 → `Config::load`（`enabled` 判定）→ `Store::load_visible`
  → `retrieve::select` → `retrieve::render_injection` を stdout へ println。注入時は
  `harness_core::inject_metrics::record("playbook", session_id, prompt, 文字数)` で計測を記録。出力は
  日本語（`📒 playbook — このプロジェクトの関連ナレッジ …`）。
- **`add --title [--trigger] [--tags] [--body] [--global] [--always]`** — body は `--body` か stdin。空なら
  bail。`Meta` を組み `slugify(title)` で slug 化して `Store::write`。global 指定で `_global` へ、既定は
  project store へ書く。
- **`list`** — 可視ノートを scope/always/triggers 付きで一覧（空なら案内文）。
- **`search <query...>`** — `retrieve::scored_for` で各ノートのスコアを表示し、`score >= min_score` または
  `always` なら `✓`（何が注入されるかのデバッグ）。
- **`rm <slug>`** — `Store::remove`（project → global 順）。無ければ stderr + exit 1。
- **`install [--dry-run]` / `uninstall [--dry-run]`** — settings.json の UserPromptSubmit フックをマージ/除去。
- **`init`** — project/global store dir を作成しサンプルノート（日本語）を書く。
- **`status`** — 解決済み config ソース・各設定値・store パス・可視ノート数を表示。

スコアリング（`retrieve::score`）: `triggers` ×5 ＞ `tags` ×3 ＞ title 語 ×2 ＞ body 一致（`+4` で cap、
長文がノイズで勝てないよう抑制）。`tokenize` は ASCII 語（len≥2）を小文字化し、**CJK は 1 文字単位**で
索引化（`is_cjk` = ひらがな/カタカナ/CJK 統合/半角カナ）、英日の stopword（`STOP`）を除外するので日本語
プロンプトも一致する。

## module 責務

- **`main`** — clap CLI 定義と各サブコマンドの薄いディスパッチ。`inject` の背骨フローを保持。
- **`config`** — レイヤ設定 `Config`/`FileConfig`。project `./playbook.toml` ＞ `~/.playbook/config.toml` ＞
  組み込みデフォルトを `harness_core::inject::load_layered` で解決（マージせず最初に存在したものが勝つ）。
  `top_k`（既定 3）/`min_score`（5）/`max_chars`（1500）/`include_global`（true）/`enabled`、`store_dir`
  既定 `~/.playbook/store`。`disabled_env` キルスイッチ。
- **`store`** — ノート store。`Meta`（serde）/`Note`/`Store`。`+++` フェンス TOML frontmatter を `parse`
  （フェンス無しは全文 body 扱い、BOM 除去）、`render` で書き戻し。`slugify` は Unicode-aware（日本語 title も
  可読 slug、48 文字 cap）。`project_dir` は `slugify(basename)-hash8(絶対パス)` で同名 dir 衝突を回避。
  `hash8` は `DefaultHasher` の下位 32bit。
- **`retrieve`** — 決定論スコアリングと選択の核。`tokenize`/`score`/`select`/`push`/`render_injection`/
  `scored_for`。予算管理は `harness_core::inject::CharBudget` に委譲。
- **`model`** — UserPromptSubmit stdin ペイロード。実体は `harness_core::hook::HookInput` の re-export
  （`session_id`/`cwd`/`hook_event_name`/`prompt`、`#[serde(default)]`）で重複実装しない。
- **`install`** — settings.json への UserPromptSubmit フック merge/remove。`MARKERS=["playbook"]` で自グループを
  識別。settings 機構（load/backup/write/strip）は `harness_core::install` に委譲。standalone 経路用。
