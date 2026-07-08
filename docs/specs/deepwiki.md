> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# deepwiki 仕様

## 概要

`deepwiki` は、リポジトリの**アーキテクチャ wiki** を `.deepwiki/*.md` に生成し、コードと一緒にコミットして
新鮮に保つハーネスである（Devin Wiki に着想）。責務を二分する: バンドルされた **Rust バイナリ**（`scan`/`status`/
`stamp`/`init`）が LLM を使わず決定論的にリポジトリを地図化し、git に対する鮮度を追跡する
一方、**生成側**は plugin の `/deepwiki` コマンドが `scan` の地図を `deepwiki-writer` サブエージェントに渡して
`.deepwiki/overview.md` とモジュールごとのページを書かせる。重いリポジトリ読み込みをサブエージェントに閉じ込め、
メインの会話を汚さないのが設計意図（`main.rs` の doc-comment が宣言）。サブスクリプションネイティブで API キー不要。
skill-only ではなく、`clap` ベースの CLI バイナリ + command + agent の三点で構成される。

## 不変条件

- **バイナリは決定論・非 LLM・非ネットワーク** — `scan::scan` はファイルシステムを走査するのみ
  （`scan.rs` doc-comment: "No LLM, no network: just the filesystem"）。言語判定（`lang_for`）・
  エントリポイント判定（`is_entry_point`）・key-file 判定（`KEY_FILES` 定数一致）はすべて純関数。
  `RepoMap` は `BTreeMap`（languages）と sort 済み `Vec`（`key_files`/`entry_points`/`readmes` を
  `sort` + `dedup`）で決定論的に serialize される。
- **走査境界** — `walk` は `SKIP_DIRS`（`.git`/`target`/`node_modules`/`.venv`/`vendor`/`.deepwiki` 等）を
  降下せず、`count_lines` は 2,000,000 バイト超のファイルを 0 行として扱う（巨大・生成物のスキップ）。
  top-level 集計は `.github` を除くドット始まりエントリを無視する。
- **鮮度は git commit 基準** — `stamp` が `.deepwiki/_meta.toml`（`Meta{sha, built_at, pages}`）に
  現在の HEAD を記録し（`meta::head_sha` = `git rev-parse HEAD`）、`status` がそれと HEAD を比較して
  `✅ fresh`（一致）/`⚠ stale`（不一致）を判定する。stale 時は `changed_since`（`git diff --name-only
  from..HEAD`）の変更ファイルのうち `is_sourceish` に該当する source-ish なものだけを最大 20 件表示する。
- **git 操作は read-only** — バイナリが呼ぶ git は `rev-parse HEAD` と `diff --name-only` のみ。commit/
  add 等の状態変更は行わない。git 外（`.deepwiki/` 配下）への書き込みは `stamp`/`init` のみ。
- **fail-soft** — `head_sha`/`changed_since` はコマンド失敗・非 git リポジトリで `None`/空 `Vec` を返し、
  `status` は「not a git repo (can't check freshness)」を出して継続する。`meta::load` は `_meta.toml`
  欠落・パース失敗で `None` を返し、`status` は「no wiki yet」を案内して exit 0。`walk`/`count_files` は
  `read_dir` 失敗を黙ってスキップする。
- **エラー時のみ非 0 exit** — `main` は各サブコマンドの `Result` が `Err` のときだけ stderr へ `error: …` を
  出し exit 1。それ以外は exit 0（`status` は「no wiki」「stale」でも 0）。
- **サブエージェントの捏造禁止** — `deepwiki-writer` は実在する `path:line` のみ引用し、存在しないファイル・
  シンボルを捏造しない（`agents/deepwiki-writer.md` の Rules）。生成物は「map であって mirror ではない」
  ——コードの再掲でなく責務・関係の要約に留める。secrets/vendored detail は書かない。

## 振る舞い

サブコマンドは `main.rs` の `Command` enum で定義。各サブコマンドは `--root`（既定 cwd）を取る。

- **`scan [--json] [--root]`** — `scan::scan` でリポジトリを地図化し、既定は `render_markdown`
  （languages を行数降順、top-level layout、entry points、key files、readmes のセクション）、`--json` は
  `serde_json` で `RepoMap` を pretty 出力する。`RepoMap` は `total_files`/`total_source_lines`/
  `languages`（言語→`LangStat{files,lines}`）/`top_level`（`DirEntry`、dir 優先→ファイル数降順）/
  `key_files`/`entry_points`/`readmes` を持つ。
- **`status [--root]`** — `_meta.toml` を `meta::load` で読み、build commit と pages 数を表示してから
  HEAD と比較。wiki が無ければ「no wiki yet」、fresh なら一致を報告、stale なら変更 source ファイル一覧と
  「run `/deepwiki` to refresh.」を出す。非 git は鮮度判定不能を告げる。
- **`stamp PAGES… [--root]`** — `.deepwiki/` を作成し、現在の HEAD（取れなければ空）・RFC3339 の
  `built_at`（`chrono::Local::now`）・書き出したページ名を `_meta.toml` に書く（`/deepwiki` が生成後に呼ぶ）。
- **`init [--root]`** — `.deepwiki/` ディレクトリを作成する（`create_dir_all`、冪等）。
- **`/deepwiki`（command）** — オーケストレーション: (1) `status` で fresh/stale/無を判定（fresh なら停止、
  無ければ初回フルビルド、stale なら差分更新）、(2) `scan` で地図取得、(3) `deepwiki-writer` サブエージェントへ
  scan 出力・repo root・既存ページ・変更ファイルを渡してページを書かせ、(4) 返ってきたページ名で `stamp`、
  (5) ユーザへ報告。`overview.md` は必ず含める。

### module 責務

- **`main`** — `clap` の CLI 定義（`Cli`/`Command`）とディスパッチ。`scan_cmd`/`status_cmd`/`init_cmd` の
  実装、`short`（SHA 先頭 8 文字）、`is_sourceish`（`.deepwiki/` 除外 + source/config 拡張子の allowlist）。
- **`scan`** — 決定論的リポジトリ地図化。`RepoMap`/`LangStat`/`DirEntry` 型、`SKIP_DIRS`/`KEY_FILES` 定数、
  `lang_for`/`is_entry_point`（純）、`scan`/`walk`/`count_lines`/`count_files`/`render_markdown`。ユニット
  テストで `lang_for`・`is_entry_point` を検証。
- **`meta`** — wiki 鮮度追跡。`Meta` 型、`WIKI_DIR`=`.deepwiki`/`META_FILE`=`_meta.toml`、`wiki_dir`/
  `meta_path`/`load`/`head_sha`/`changed_since`/`stamp`。git 呼び出しは best-effort で fail-soft。
- **agent `deepwiki-writer`** — repo map からページを書く／リフレッシュするサブエージェント（tools: Read,
  Grep, Glob, Write, Edit, Bash）。最終メッセージは書き出したページ名のリストのみを返す。
- **統合テスト（`tests/integration.rs`）** — 実バイナリを spawn し、`--help`・no-wiki `status`・
  `scan --json` の stdout と exit code を検証する（`--root` で隔離 temp dir を指す）。
