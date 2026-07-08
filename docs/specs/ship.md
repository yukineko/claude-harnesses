> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# ship 仕様

## 概要

`ship` は commit・merge・push・plugin-update の「出荷 ritual」を **検出して促す**（自ら実行はしない）
ハーネスである。git 側の未出荷状態（未コミット変更・未マージ `condukt/*` ブランチ・残置 worktree・
未push コミット）と plugin-cache 側の状態（committed バイナリが src より古い "stale crate"）を検出し、
チェックリストとして提示する。`main.rs` の doc-comment が宣言するとおり、この binary は commit/merge/
push/add/checkout/reset といった **状態変更を伴う git コマンドを一切呼ばない**。実行可能なのは
plugin-cache を触る2ステップ（`rebuild-plugins.sh --stage-repo` と `rollout-plugins.sh`）のみで、
git の出荷操作は常に人間承認 (GATED) に委ねられる。

## 不変条件

- **git 非改変（ハード不変条件）**: `git.rs` の検出関数は `git status --porcelain` /
  `git branch --no-merged` / `git worktree list` / `git rev-list --count` など **read-only な git
  のみ**を呼ぶ。`checklist::run_safe` / `checklist::rollout` は plugin スクリプトを実行するが、これらも
  `~/.claude/plugins/` にしか書かず git には触れない（`tests/integration.rs::binary_never_mutates_git`）。
- **commit/merge/push は GATED**: `checklist::render` は uncommitted → commit、unmerged branch →
  merge、unpushed → push の各行に定数 `GATED = "GATED — 要人間承認"` を付す。自動実行しない。
- **fail-soft**: `resolve_repo_root` は `git rev-parse --show-toplevel` 失敗時に cwd へフォールバック。
  `git::*` 各関数はコマンド失敗時に `false` / 空 `Vec` / `0` を返し、`stale::stale_crates` は
  crates ディレクトリや個別 crate の IO エラーを黙ってスキップする。
- **headless 抑止**: `session_end` は `harness_core::hook::is_headless()`（stdout が TTY でない）時に
  reminder を stdout へ出さない。piped stdin が無ければ `read_stdin_if_piped()` が即空文字列を返し
  ブロックを避ける。`HookInput::parse` が失敗しても、`SessionEnd` は常に exit 0。
- **stale の正しい是正先**: stale item は committed バイナリの古さであり、`--stage-repo`（committed bin
  更新）＋ GATED commit でのみ解消する。cache-only な `rebuild-plugins.sh` / `rollout` は解消しない旨を
  `render` が明示（`do NOT clear`）。
- **worktree は informational**: `ShipStatus::has_unshipped_work` は残置 worktree を「未出荷作業」に
  数えない（reminder を出さない）。checklist には表示する。

## 振る舞い

サブコマンドは `clap` の `Cmd` enum で定義（`main`）。全て `detect(repo) -> ShipStatus` を土台にする。

- **`check [--run-safe]`**: `detect` の結果を `checklist::render` でチェックリスト出力。`--run-safe` 指定時は
  `run_safe`（`scripts/rebuild-plugins.sh --stage-repo` を repo 相対で実行、無ければ Err）を走らせ
  summary を出し、再 detect して `render_gated_remaining`（commit/merge/push の残り GATED のみ、無ければ
  `all clear`）を出す。
- **`session-end`**: SessionEnd hook。`run_hook(session_end)` 経由。stdin を `read_stdin_if_piped` で読み
  `HookInput::parse` → `cwd_or_current` で repo 解決。`session_end_reminder`（未出荷が無ければ `None`）が
  `Some` で、かつ非 headless の時だけ `⚓ 未出荷の作業があります: …` を1行出す。常に exit 0。
- **`rollout`**: `checklist::rollout`（`scripts/rollout-plugins.sh` を repo 相対で実行、無ければ Err）で
  `/plugin update` 相当（cache の `<name>/<version>/` dir 作成＋`installed_plugins.json` repoint＋
  rebuild＋sync）を実行し summary 出力。cache-only で committed bin staleness は解消しない。

検出ロジック（`ShipStatus` の各フィールド）:

- `uncommitted` (`git::uncommitted_changes`): `git status --porcelain` が非空か（staged/unstaged/untracked）。
- `unmerged_branches` (`git::unmerged_condukt_branches`): `main` に未マージの `condukt/*` ローカルブランチ。
- `leftover_worktrees` (`git::leftover_worktrees`): main repo 以外の worktree パス（canonicalize 比較）。
- `unpushed_count` (`git::unpushed_count`): `@{upstream}..HEAD` の count、失敗時 `origin/main..HEAD`、両失敗で 0。
- `stale_crates` (`stale::stale_crates`): `crates/<name>` で `src/` と `bin/<name>-linux-x86_64` の両方が
  存在し、`src/` 配下の最新 mtime が bin の mtime より新しい crate 名。どちらか欠落・IO エラーはスキップ。
