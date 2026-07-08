> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# precommit-audit 仕様

## 概要

`precommit-audit` は、コミット成立前（あるいは Claude Code の停止前）に保留中のワーキングセットを
静的監査し、問題のある変更をブロックする config 駆動・クロスプラットフォームなハーネスである。元の
Windows 専用 PowerShell pre-commit フックを、Linux/macOS/Windows で同一に動く単一 Rust 静的バイナリに
書き直したもの。`git diff --name-only HEAD` と `git ls-files --others --exclude-standard`
（`git::changed_and_untracked`）で working set を解決し、各ファイルの追加行（`git::added_lines`）に
汎用チェックと config 由来の custom rule を走らせる。監査ロジックは二層: **汎用チェックはバイナリ組み込み**
（`checks` モジュール）、**プロジェクト固有ポリシーは `.precommit-audit.toml` の `[[rule]]` データ**
（`config::Rule`）でハードコードしない。バイナリを汎用に保ち複数リポジトリで使い回すのが要点。
サブスクリプションネイティブ（フック 1 本 + 同梱 Rust バイナリのみ、`ANTHROPIC_API_KEY` 不要）。plugin.json は
Claude Code の **Stop hook** として `bin/precommit-audit --mode stop`（timeout 30s）を配線する。

## 不変条件

- **dual-mode の exit コード契約** — `resolve_mode` が `--mode`／env `AUDIT_MODE`／既定 `stop` を解決。
  ブロッキング findings 時の exit code（`fail_exit`）は `precommit` モードで **1**（git/pre-commit
  フレームワーク規約）、`stop` モードで **2**（Claude Code Stop hook へ findings を戻す）。git フックは
  Claude の JSON `decision:block` プロトコルを話せないため、終了コード + ブロックマーカーという別契約を保つ
  （donegate/reviewgate/tdd の JSON Stop ゲート群とは意図的に別系統の兄弟）。
- **never-break-a-turn（SessionEnd advisory）** — `hook.event == "SessionEnd"` のとき `fail_exit` は **0**
  に固定される。Claude Code は非 0 な hook exit を "failed hook" として扱いターンを壊すため、SessionEnd では
  ブロッキング findings があっても exit を上げない。ただし **findings は握り潰さない**: `plan_emission` が
  stderr に advisory ヘッダ付きで目立たせ、監査ログに `verdict:"block"` + block marker を記録する
  （`advisory_emission_tests` が保証）。
- **パニック時 fail-open** — `main` は `std::panic::catch_unwind(run)` で包み、走査中の想定外パニック
  （不正バイト・linter サブプロセスの癖等）でコミットを中断せずターンも壊さない: 真のパニックは exit 0 に
  フォールバックする（`run` 内の実 `exit(...)` は直接終了するので影響を受けない）。
- **未信頼リポジトリの config 実行防止** — auto-discovery した `.precommit-audit.toml` は root を信頼
  するまで無視する（`project_config_blocked(explicit, exists, trusted)` = `!explicit && exists && !trusted`）。
  実行ベクタは `linters.node_projects` が repo-local な eslint/tsc を解決する経路。信頼は `precommit-audit trust`
  が `harness_core::trust`（donegate/reviewgate/tdd と同じ共有 workspace-trust list）へ root を追加して honor
  させる。明示 `--config` はオペレータの意図的選択として信頼有無に関わらず常に honor。未信頼時は組み込み
  既定チェックのみ走る（`trust_gate_tests` が真偽表を保証）。
- **git 非改変（read-only）** — `git.rs` は `diff`/`ls-files`/`grep`/`rev-parse` の read-only な git のみを
  shell out する（`GIT_TERMINAL_PROMPT=0`）。commit/add/checkout 等の状態変更コマンドは呼ばない。
- **再帰ガード** — stdin JSON の `stop_hook_active` が真（同一 stop サイクル内の再発火）なら即 exit 0。
- **理由必須の抑制** — 行単位 `# audit-ignore: <理由>`（JS/TS は `//`。理由=後続の非空白文字が必須で
  マーカーだけでは無効、`git::has_audit_ignore`）、ファイル単位は先頭 20 行以内の `audit-ignore-file: <理由>`
  （`checks::head_suppressed`）、一回限りは `<audit_dir>/.audit-skip`（読み取り時に消費）。
- **自己監査回避** — config ファイル自身（rule パターンを含む）は working set から除外し、導入コミットで
  self-trigger しないようにする（`run` 内の `strip_prefix` + `classify::norm` 比較）。
- **severity と exit の分離** — `Severity::Block` のみ exit code に影響し、`Severity::Warn`（例: file_length）は
  常に advisory で exit を上げない（`emit_and_exit` の `partition`）。

## 振る舞い

CLI: `precommit-audit [--mode stop|precommit] [--config <file>] [--root <dir>]` と `precommit-audit trust`。
`--root` 既定は env `CLAUDE_PROJECT_DIR`、無ければ `git rev-parse --show-toplevel`、更に無ければ cwd
（`resolve_root`）。`--config` 既定は `<root>/.precommit-audit.toml`。`-V/--version`・`-h/--help` あり。
未知引数は exit 64。

- **`run`（通常フロー）** — trust ハンドリング → stdin 読み（再帰ガード）→ root/mode/config 解決 →
  `.audit-skip` 消費 → working set 解決（config 自己除外）→ 空なら exit 0 → `Classifier` と `Ctx` 構築 →
  `run_static_checks` → `checks.linters` 真なら `linters::run` → `stop` モードなら `review::check` →
  `emit_and_exit`。
- **`emit_and_exit` / `plan_emission`** — issues を block/warn に partition。`plan_emission` は純関数として
  「何を stderr に出し・ログの verdict・marker を立てるか消すか・exit code」を IO 無しで決める
  （advisory 契約を unit-test 可能にするため）。marker は `<audit_dir>/.audit-blocked`
  （`hookio::set/clear_block_marker`）。監査ログは `<audit_dir>/audit-log.jsonl` へ JSONL 追記
  （`hookio::write_audit_log`: ts/mode/verdict/issueCount/issueCategories/warningCount/changedCount）。
  タイムスタンプは date crate 非依存の civil-from-days（`now_iso8601`）。
- **`trust`** — 解決済み root を `harness_core::trust::add` で共有 trust list に登録し exit 0。stdin 読取り
  前に処理し、素の手動コマンドとして機能する。

汎用チェック（`checks` モジュール。`[checks]` で個別に有効/無効化、既定値は各 `Default`）:

- **`missing_test`**（ON, block）— source 変更があり test 変更が皆無なら指摘（`Classifier::is_source`/`is_test`）。
- **`hardcoded_ip`/`hardcoded_secret`/`swallowed_error`**（ON, block）— 全ファイルの追加行を merge
  （`merged_added_lines`。suppression/exclude/非 scannable を除外）して走査。IP=benign prefix、secret=env-getter/
  placeholder、swallowed=`except: pass`/`|| true` 等 + `extra_patterns`。コメント行は除外。
- **`duplicate_function`**（**既定 OFF**、heuristic でノイジー、block）— py/js/sh の追加 def を `git::grep_files`
  で他ファイルの同名と照合。`common_names` allowlist で除外。
- **`local_capture`**（ON, block）— `set -e` な `.sh` の `local VAR=$(...)` silent-failure を検出。
- **`markdown_links`**（ON, block）— 変更 `.md` の相対リンク先の存在を FS で確認。
- **`line_endings`**（ON, block）— 拡張子ごと CRLF/LF 強制（既定 `.ps1/.cmd/.bat`=CRLF, `.sh`=LF）。
- **`file_length`**（ON, **warn のみ**）— `limit`（既定 500）行超で warning。ブロックしない。
- **`custom_rules`**（ON）— `[[rule]]` を compile し glob スコープ + `unless` allowlist + `skip_comments` で追加行に
  マッチ。severity は rule 指定（既定 block）。rule ごと最大 8 hits。
- **`linters`**（ON, block）— 外部ツールを best-effort orchestrate（`linters::run`）。バイナリ欠落は silent skip、
  各ツール `timeout_secs`（既定 25）で hard timeout。py_compile/ruff/bash -n/eslint/tsc/radon/semgrep/gitleaks。
  node ツールは `node_projects` 配下の `node_modules/.bin/` を OS shim 付きで解決。

- **`review::check`（review_contract、既定 OFF、`stop` モードのみ）** — opt-in。docs 以外の source 変更が
  あるとき、`review_contract.path`（既定 `.claude/last-review.json`）が現在の diff hash（`review::diff_hash`:
  `git diff HEAD` を LF 正規化 + `---UNTRACKED---` + sorted untracked を SHA-256）と `verdict:"pass"` を
  持つかを要求。欠落/陳腐なら block し `/precommit` の実行を促す。

### module 責務

- **`main`** — 引数 parse（`parse_args`/`Args`）、root/mode 解決、trust ハンドリング、config の trust ゲート
  （`project_config_blocked`/`warn_untrusted_once`）、SessionEnd advisory の `fail_exit` 決定、panic fail-open、
  `plan_emission`（純: 出力計画）と `emit_and_exit`（IO + exit）、`now_iso8601`。
- **`config`** — `Config` と全サブテーブル（`Classify`/`Checks`/`HardcodedIp`/`HardcodedSecret`/`SwallowedError`/
  `DuplicateFunction`/`LocalCapture`/`LineEndings`/`FileLength`/`Linters`/`ReviewContract`/`Rule`/`Severity`）を
  TOML から deserialize。全て `#[serde(default, deny_unknown_fields)]`。`Config::load` は欠落ファイルを
  全既定（Ok）として扱う。既定は PowerShell 原版の汎用挙動を再現。
- **`checks`** — 組み込み構造/パターンチェックと custom-rule エンジン。`Ctx`（root/cfg/cls/files）、各 `check_*`、
  `run_static_checks`（linters と review を除く有効チェックを駆動）、`merged_added_lines`、`CompiledRule`。
- **`checks::linters`** — 外部リンタ/スキャナの orchestration。`which`（cross-platform）、`run_bounded`
  （combined stdout+stderr + hard timeout、timeout 時 `<killed>`）、`node_bin`/`eslint_root`、radon/semgrep/
  gitleaks の出力要約。全ツール optional で欠落は silent skip。
- **`checks::review`** — subagent review contract（Claude Code 固有、opt-in）。`diff_hash`（`/precommit` が
  Python で計算する canonical hash と一致必須）、`check`（artifact 欠落/陳腐で Some(Issue)）。
- **`classify`** — 変更ファイルの source/test/excluded 分類。`Classifier`（config 由来 ext リスト + 固定
  test-path heuristic の regex）、`norm`（`\`→`/`）、`ext_of`。
- **`git`** — `git` CLI への read-only wrapper。`changed_and_untracked`/`untracked`/`diff_head`/`file_diff`
  （untracked は `--no-index /dev/null`）/`added_lines`（`+` 行から `audit-ignore` 除去）/`grep_files`/
  `toplevel`/`has_audit_ignore`。UTF-8 lossy デコードで CP932 mojibake を回避。
- **`hookio`** — hook 配管。`read_stdin`/`HookInput`（`stop_hook_active`/`event`。非 Claude 呼出しでは既定値）、
  `consume_skip`、block marker の set/clear、`write_audit_log`（JSONL best-effort）。
- **`model`** — 共有結果型 `Issue`（category/message/severity）と `Issue::block`/`warn`。
