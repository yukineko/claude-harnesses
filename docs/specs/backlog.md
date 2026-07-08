> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# backlog 仕様

## 概要

`backlog` は、どのセッション・どのリポジトリよりも長く生き残る **クロスプロジェクト・タスクキュー** ハーネスである。
`~/.backlog/tasks.toml`（`config::Config::tasks_path`）に `[[task]]` 配列として作業項目を永続化し、追加・一覧・ピック・
完了/失敗マーク・編集を担う。加えて、複数セッションを直列化するための排他 run-lock（`~/.backlog/run.lock`,
`lock::LockInfo`）を所有し、**SessionStart** フック（`hooks::session_start::run`）でセッション開始時に cwd のリポジトリに
紐づく pending タスクを `additionalContext` として注入する。判定（次に何をやるか）は上位 driver（`/flow`）や人間に委ね、
本バイナリは queue+state+lock の決定論的な管理層に徹する。skill 1 つ・hook 1 つ・同梱バイナリ 1 つで動く subscription-native
（`ANTHROPIC_API_KEY` 不要）。lock→pick→`/condukt`→done のループ driver 自体は `/flow` へ統合され、`/backlog` skill は
その薄い queue/state 操作のエントリポイント。

## 不変条件

- **status 語彙は3値固定** — `task::STATUSES` = `["pending", "done", "failed"]`（`STATUS_PENDING`/`STATUS_DONE`/
  `STATUS_FAILED`）。lifecycle は `pending → done`（`mark_done`）または `pending → failed`（`mark_failed`）で、deferred は
  期限切れで `pending` に戻る（`requeue_expired`）。`open` は hypothesis の語彙であり backlog には無い。`--status` に未知値を
  渡すと `task::status_warning` が「空キューと区別できない silent 空マッチ」を防ぐため stderr に loud 警告を出す。
- **fail-soft**（ターンを壊さない）— SessionStart は best-effort。`Config::disabled_env`（`BACKLOG_DISABLE=1`）や
  `enabled=false` なら無出力。malformed stdin は `HookInput::parse` が `None` を返し stderr にログして skip、常に exit 0
  （`run_hook`）。`store::load` はファイル不在で空 `Vec`。`requeue_expired` の clock 失敗は `t=0`（requeue 0 件）に倒す。
- **重複コンテンツ拒否（content hashkey）** — `add` は `task::hashkey`（title を trim→NFKC→lowercase→空白圧縮→前後記号除去
  で正規化し、`\u{1f}` で project と連結、FNV-1a 64bit を 16 桁 hex 化）で内容重複を検出。既存の `pending`/`failed` タスクが
  同 hashkey を持つ、または `condukt state is-claimed --hashkey <h>` が exit 0 を返す（live cross-session claim）なら
  `check_duplicate` が拒否する。`done` 重複はブロックしない（再積みは正当）。`condukt` 不在・spawn 失敗・想定外 exit は
  fail-soft に「claim なし」扱い（`is_claimed_elsewhere`）。`--force` で両拒否を意図的にバイパス。
- **run-lock は atomic かつ liveness-reaped** — `lock::acquire_inner` は temp ファイルへ完全書き込み後 `hard_link`(2) で
  atomic publish（EEXIST で敗者は1人だけ）。既存 lock は owner pid が **確実に dead**（`pid_alive`: Linux は `/proc/<pid>`、
  それ以外は `kill -0` へフォールバック）のときだけ reap し、live holder は bail する。`--force`（強制奪取）のみ live holder を
  displace する。read 不能な partial lock は「書き込み中」扱いで削除しない。retry は `MAX_ATTEMPTS=8` で bound。
- **tasks-file scoped advisory lock**（run-lock とは別物）— mutator の load→modify→save 臨界区間を、tasks ファイルごとの
  sibling lockfile（`tasks.toml.lock`, `create_new`/O_EXCL）で直列化し lost-update を防ぐ（`with_tasks_lock`）。BLOCKING
  かつ bounded（`TASKS_LOCK_MAX_ATTEMPTS=50` × 3ms）、`TASKS_LOCK_STALE_SECS=5` 超で reap、取得失敗時は保護なしで body を
  実行する fail-soft（`Err` を返さない）。`RAII` の `TasksLockGuard`/`TmpGuard` が全 drop path で解放。
- **原子的書き込み** — `store::save` は同ディレクトリ temp（`.tasks.toml.tmp`）→`rename` で torn file を出さない。
- **決定論的キュー順** — `store::queue_order`: (1) `priority()` 昇順（p0<p1<p2<なし=3）、(2) `weight` 降順
  （`f64::total_cmp` で NaN panic なし）、(3) `created_at` 昇順。weight=0.0 既定なら legacy の (priority, created_at) 順に一致
  し、旧 tasks.toml と挙動不変。`list` も `next` と同じ順で表示する。

## 振る舞い

サブコマンドは `clap` の `Command` enum で定義（`main`）。全て `config::Config::load` → `tasks_path` を土台にする。

- **`add`** — `--title`/`--project`/`--tag`（複数可）/`--priority`（p0/p1/p2 を tag として追加）/`--notes`/`--weight`
  （既定 0.0）/`--force`。`store::add_with_weight` が hashkey 重複ガードを通し（`--force` で skip）、`new_id`（title+now を
  FNV-1a 32bit した 8 桁 hex）を採番して `pending` で永続化。`added: {id}` を出す。
- **`list [--tag] [--project] [--status] [--json]`** — フィルタ後 `queue_order` で整列。`--status` 未知値は warn。人間表
  は ID/PRIORITY/STATUS/TITLE で、deferred タスクは status 欄を `deferred` と表示。`--json` は各要素に計算値 `hashkey`
  （非保存）を足した配列を出し、`/flow` 等が `condukt state is-claimed` ゲートに使える。空なら `no tasks`。
- **`next [--tag] [--project]`** — `store::next` が pending/failed かつ非 deferred のうち `queue_order` 先頭を JSON pretty
  で出す。無ければ `no pending tasks`。`project_matches` は完全一致または `filter + "/"` 前方一致。
- **`done <id>`** — `store::mark_done` で `done` に更新。未知 id は Err。
- **`fail <id> [--reason]`** — `store::mark_failed` で `failed` に更新し reason を notes に追記、`defer_until = now + 172800`
  （2日）を設定して再実行を先送り。設定した期限を `format_unix_datetime`（std のみで実装、UTC）で人が読める形に整形し表示。
- **`edit <id> [--title] [--tag] [--notes] [--status]`** — `store::edit` で指定フィールドのみ更新（None は不変、`--tag` は
  置換）。`updated_at` を現在時刻に。
- **`session-start`** — SessionStart フック。`read_stdin`→`HookInput::parse`。`cwd_or_current` から `.git` を上へ辿って
  repo_root を解決し、期限切れ deferred を `requeue_expired` で復帰させてから、その repo の pending/failed タスクを
  priority→created_at 順で Markdown 化し `{"additionalContext": …}` として出力。cycle タグ（`cycle:test-fix`/`tdd`/
  `implement`/`review-fix`/`once`）ごとに完了までの指示文を差し込む。`inject_limit`（既定 4000B）超は UTF-8 境界を壊さず
  切り詰め `*(truncated)*` を付す。
- **`install` / `uninstall` [--dry-run]** — `~/.claude/settings.json` の `SessionStart` グループを冪等マージ/除去
  （`harness_core::install`、marker `"backlog"`）。`--dry-run` は書かず結果のみ表示。
- **`lock {acquire,release,status}`** — `acquire --session-id --project [--force]`（`--force` で live holder 奪取）、
  `release`（無ければ no-op）、`status`（`none` / `Active` は JSON、`Stale`（dead pid）は `stale:true` を付した JSON）。

エラー時は `main` が `Error: …` を stderr に出し exit 1。

### module 責務

- **`main`** — clap CLI 定義とディスパッチ。`now_unix`（秒）、`format_unix_datetime`（Fliegel-Van Flandern で std のみの
  UTC 変換）を持つ。
- **`task`** — `Task` 構造体（`id`/`title`/`project`/`tags`/`status`/`notes`/`created_at`/`updated_at`/`defer_until`/
  `weight`。後2つは `#[serde(default)]` で旧ファイル互換）と派生メソッド `priority`/`cycle_tag`/`is_pending`/`is_deferred`。
  status 語彙定数、`status_warning`、`new_id`（FNV-1a32）、`normalize_title`/`hashkey`（FNV-1a64、cross-session dedup 鍵）。
- **`store`** — tasks.toml の CRUD。`load`/`save`（atomic）、`add`(#[cfg(test)])/`add_with_weight`、`check_duplicate`/
  `is_claimed_elsewhere`、`next`/`list`/`queue_order`、`mark_done`/`mark_failed`/`edit`/`requeue_expired`、
  `project_matches`、及び tasks-file scoped advisory lock 一式（`with_tasks_lock`/`try_acquire_tasks_lock`/`TasksLockGuard`）。
- **`lock`** — グローバル run-lock。`LockInfo`/`LockStatus`（`Active`/`Stale`/`None`）、atomic な `acquire_inner`
  （`acquire`/`acquire_forced`/`*_at`）、`release`/`status`、`pid_alive`（cross-platform liveness）。tests は `*_at` に
  lock_dir を差し込んで検証。
- **`config`** — `~/.backlog/config.toml` から `enabled`/`store_dir`（`~` 展開）/`inject_limit`（既定 4000）を読む
  `Config::load`。`tasks_path`、`disabled_env`（`BACKLOG_DISABLE`）。
- **`install`** — settings.json への SessionStart hook 配線/除去（`harness_core::install` 委譲）。
- **`hooks::session_start`** — SessionStart の本体。requeue → repo スコープの pending 抽出 → cycle 指示付き Markdown 描画
  → inject_limit 切り詰め。`repo_root`/`cycle_tag_instruction`/`truncate_to_byte_boundary`。
