# reviewgate 仕様

## 概要

`reviewgate` は Claude Code の **`Stop` フック**として動くコードレビュー・ゲートである。エージェントが
ターン完了を宣言する直前に、その diff をレビュー済みかどうかで停止をブロックする。「これは*動く*か」を
見る donegate に対し、「これは*良い*コードか」を補完する。1 本の `Stop` フック（`hooks/hooks.json` の
`${CLAUDE_PLUGIN_ROOT}/bin/reviewgate review`）＋同梱 Rust バイナリだけで動くサブスクリプションネイティブ
設計で、**API キー不要**。判断は LLM に置き（`inject` はエージェント自身、`subprocess` は独立レビュアー）、
バイナリは決定論的な収束制御（diff ハッシュ + `max_attempts` の round 上限）に徹する。レビュー対象の diff を
`hash_diff`（`std::hash::DefaultHasher`, 16 hex）でハッシュ化し、最後にレビューを強制した diff と一致する停止は
「既にレビュー済み」として通す。

## 不変条件

- **never-break-a-turn（最上位不変条件）** — reviewgate 自身のエラー（config 不正・git 無し・自身のバグ）は
  停止を必ず**許可**する。`review_command` は `harness_core::gate::run::run_guarded` で `review_run` を包み、
  hook モードでは panic を握りつぶして exit 0、manual CLI モードでのみ exit 1 にする。Claude 側へは常に exit 0 を
  返し、停止の可否は exit code ではなく stdout の `{"decision":"block","reason":…}` フィールドで表す（`main.rs`）。
- **フェイルクローズドだが有界** — 「レビュアー自体の失敗」は「レビュー結果クリーン」と**区別**する。subprocess の
  crash/timeout/解析不能出力（`decide_subprocess` の `ReviewerResult::Error`）と、diff が `max_diff_bytes` を
  超えて末尾が未レビューになる truncation（`decide_truncated`）は、無言で許可せず**ブロック**する（壊れた
  レビュアーや切り詰めた末尾が gate のバイパスにならないため）。ただし `max_attempts` 連続で解消しなければ
  警告を出して通過を許可し、それぞれ専用 tag（`reviewer-error-giveup` / `truncated-giveup`）で許可するので、
  ログ上クリーンと混同されず、ターンが永久に閉じ込められることもない。ブロック理由には常にすべての抜け道
  （`.reviewgate-skip`・`REVIEWGATE_DISABLE=1`・`max_diff_bytes` 引き上げ）を明示する。
- **truncation はハッシュを記録しない** — `decide_truncated` の `Block` は `last_hash` を空にする。diff ハッシュは
  欠落した末尾を覆えないため、後段の "already-reviewed" 短絡が未レビュー末尾を認証してしまうのを防ぐ。同様に
  reviewer error のブロックもハッシュを記録せず、次の停止でレビュアー回復を再チェックし続ける。
- **truncation は hash 短絡より前に判定** — `evaluate` は `truncated` チェックを diff ハッシュ照合の**前**に置く。
  そうしないと同一の truncated diff が "already-reviewed" 経路で無審査に通ってしまう。
- **trust 境界（任意コード実行の防止）** — project `./reviewgate.toml` の `reviewer_cmd` は `subprocess` モードで
  実行されるため、project root を `harness_core::trust` で **trust** して初めて project 設定が honored される
  （`Config::load`）。未 trust なら project ファイルは無視し home config → 組み込みデフォルトへフォールバック。
  `HARNESS_TRUST_ALL=1` は trust リストを上書きする。config precedence は project(trusted) > `~/.reviewgate/config.toml`
  > 組み込みデフォルト。パースエラーは無言でフォールバック（ゲートはターンをクラッシュさせない）。
- **safe by default** — git リポジトリでない（`changed_files` が `None`）・レビュー対象ファイルが
  `min_changed_files` 未満・diff が空、いずれも停止を許可する（`evaluate` の `allow(...)`）。lockfile・
  `node_modules`・`target`・生成物などは組み込み `default_exclude` で除外。
- **収束の有界性** — `inject`/`subprocess`（Issues）とも round は `prior_attempts + 1` で、`max_attempts`
  （既定 2）超過で `giveup` tag により許可。`reset_after_secs`（既定 600s）のアイドル gap を超えると
  round カウンタは 0 にリセットされ、新しいターンとして扱われる。

## 振る舞い

サブコマンドは clap の `Command` enum（`main.rs`）。

- **`review`** — `Stop` フック本体。stdin からフック JSON を読む（`HookInput::parse`）。stdin 無しの手実行は
  interactive（人間向けドライチェック）扱い。順に (1) `REVIEWGATE_DISABLE` env / `enabled=false` で即 exit 0、
  (2) `.reviewgate-skip`（1 行理由）を `consume_skip` で一度消費し許可、(3) `state::load` で prior session state を
  読み `review::evaluate` で判定、(4) `Decision::Allow`/`Block` を state 保存・`log_event`（JSONL 追記）とともに
  適用。Allow で `attempts==0 && last_hash 空` なら state をリセット。
- **`install [--dry-run]` / `uninstall [--dry-run]`** — `~/.claude/settings.json` に `Stop` フック
  （`reviewgate review`, timeout 600s）を冪等マージ／削除（`install.rs`。書き込み前にバックアップ、既存
  reviewgate グループを strip して再追加）。
- **`init [--force]`** — 雛形 `./reviewgate.toml`（`STARTER`）を書き出す。既存なら `--force` 無しでエラー。
- **`status`** — 解決済み config（source path・enabled・mode・`reviewer_cmd`(subprocess時)・`max_attempts`・
  state_dir）と、cwd で今レビュー対象になるファイル（`reviewable_files`）を表示。agent は呼ばない。
- **`trust`** — 現在の project root を `harness_core::trust::add` で trust に登録し、その `./reviewgate.toml`
  （`reviewer_cmd` 含む）を honored にする。

判定ロジック（`review::evaluate`）:

- **`inject` モード（既定）** — 新しい diff 状態ごとに一度だけ停止をブロックし、レビュー用ルーブリック
  （`inject_reason`, `cfg.rubric`）を注入する。実行中エージェントが自分の変更をレビューし完了前に直す。追加
  プロセス無し・コスト無料。
- **`subprocess` モード** — `run_reviewer` が `reviewer_cmd`（既定 `claude -p`）にレビュープロンプトを stdin で
  渡し stdout の findings を読む。`classify` は空 or 先頭 `LGTM` を `Clean`、それ以外を `Issues` と分類。crash/
  timeout（`wait_timeout`, `reviewer_timeout_secs` 既定 300s）/spawn 失敗は `Error`。Issues のときだけ、その指摘だけを
  注入してブロック（`subprocess_reason`）。`build_command` は shell metacharacter を含む cmdline のみ
  `harness_core::shell::command` 経由、それ以外は直接 `Command::new`。

### module 責務

- **`main`** — CLI dispatch と `review_run` の停止判定→state 保存→`log_event`（JSONL）フロー。never-break-a-turn
  panic ガード（`run_guarded`）・env/config disable・`.reviewgate-skip` 消費・`Decision` 適用を担う。
- **`config`** — `Config`（`Mode`/`max_attempts`/`reset_after_secs`/`min_changed_files`/`max_diff_bytes`/
  `include`/`exclude`/`rubric`/`reviewer_cmd`/`reviewer_timeout_secs`/`state_dir`）を TOML（`FileConfig`, 全
  optional）から3層 precedence でロード。**trust 境界**（`is_trusted` で project の `reviewer_cmd` を gate）と
  sanitize（0 値をデフォルトへ補正）を担う。`DEFAULT_RUBRIC`/`default_include`/`default_exclude`。
- **`review`** — ゲート本体。`evaluate`（no-git/未変更/空 diff → allow、truncated → `decide_truncated`、hash 一致 →
  already-reviewed、mode 別判定）、`decide_subprocess`/`decide_truncated`（フェイルクローズド判定、`evaluate` から
  分離して純関数として unit-test 可能）、`reviewable_files`（include/exclude glob フィルタ）、`run_reviewer`/
  `classify`/`build_command`、各 `*_reason`（LLM 向け注入文）、`hash_diff`（`DefaultHasher`）。
- **`git`** — `git` への read-only subprocess 呼び出しのみ。`changed_files`（unstaged+staged+untracked、`None`=非
  git repo）、`diff_text`（unstaged/staged hunk + untracked 全文、`max_bytes` で `truncate_on_boundary`。UTF-8
  char boundary へ後退して切る）、`DiffText{text, truncated}`。
- **`state`** — `harness_core::gate::state::{load,save,reset,SessionState}` の薄い re-export。`SessionState` は
  `attempts`/`last_hash`/`last_ts` を持ち、round カウンタは `evaluate` がインラインで駆動する。
- **`model`** — `harness_core::hook::HookInput` の re-export（`parse`/`cwd_or_current`/`session_key`/
  `stop_hook_active`）。
- **`install`** — `~/.claude/settings.json` の `Stop` フック配線（冪等マージ・strip・バックアップ）。settings-file
  機構は `harness_core::install` に委譲、reviewgate は `MARKERS=["reviewgate"]` の所有判定のみ担う。
