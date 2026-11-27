# relay — 発生元 claude プロセスへの帰属（per-process attribution）

状態: **仕様のみ（未実装）**。ユーザー裁定 2026-11-17 により relay は blastguard から専用の crate/plugin
へ切り出し、本仕様はその新 crate 側で実装する。下記の env 名・サブコマンド名・exit code は現行
blastguard 実装（`crates/blastguard/src/relay.rs` / `main.rs` の `relay` サブコマンド）を前提に書いて
あり、切り出し後の名前に合わせて読み替える（読み替えは本ファイルを同じコミットで更新すること）。

## 経緯（観測 2026-11-17）

`claude --continue` の 2 プロセス pid 9173（name `harness-9e`）と pid 10092（name `harness-5a`）が
`~/.claude/sessions/<pid>.json` 上で同じ `sessionId`（06c2bd77…）を持っていた。relay queue は
session id だけで鍵付けされている（`~/.blastguard/relay/<session>.jsonl`、`session_of`）ため:

- (a) 各プロセスの Stop が**相手の** subagent relay で block する（FP）。
- (b) 一方の `relay dismiss` / mark-executed が**他方の**義務を消す（fail-open）。

## 裁定

session 共有は許容したまま、各 relay 行を**それを生んだ claude プロセス**に帰属させる。
各 main プロセスの Stop・dismiss・mark-executed / mark-relayed / mark-denied は自分の行だけに作用する。

## 仕様

### 自プロセスの特定（own pid）

- **session key**: payload の `session_id`（無ければ従来の `session_of` の fallback）。`relay dismiss`
  は payload を持たないので、**対象行の `session_id`** を session key とする。
- **祖先探索**: hook プロセスの祖先を `getppid` から pid 1 まで（上限 64 段）辿り、
  `<sessions_dir>/<pid>.json` が parse でき `sessionId` が session key と一致する**最も近い**祖先を
  own pid とする。別 `sessionId` の json を持つ祖先（入れ子の claude）は飛ばして続行する。
- **`sessions_dir`**: `BLASTGUARD_CLAUDE_SESSIONS_DIR`（空白は無視）、無ければ `$HOME/.claude/sessions`。
- **テスト seam `BLASTGUARD_CLAUDE_PID`**（10 進の pid、空白は無視）: 設定されていれば祖先探索をせず
  その pid を候補とするが、**同じ検証**（`<sessions_dir>/<pid>.json` が存在・parse 可能・`sessionId`
  一致）を通らなければ own pid は**判定不能**。祖先探索へは fallback しない。
- 祖先列挙の失敗・該当なし・数値でない override も**判定不能**。

### 行に保存するもの

`queued` event に次の 2 フィールドを追加する。どちらも serde default `null`（旧行には無い）。

- `claude_pid`: own pid。判定不能なら `null`。
- `claude_started_at`: その sessions json の `startedAt`。無ければ `null`（pid 再利用の検出用）。

### 所有者の有効性

行の所有者が**有効**なのは次をすべて満たすときだけ:

- `claude_pid` が非 null、
- `<sessions_dir>/<claude_pid>.json` が存在・parse 可能、
- その `sessionId` が行の session と一致、
- 行に `claude_started_at` があれば `startedAt` が一致。

それ以外 — `null`（旧行・探索失敗）、json 不在（プロセス終了。2026-11-17 の観測では生存中の
プロセスの json だけが残っていた）、不一致（pid 再利用）、読み取り不能 — はすべて**共有行**として扱い、
どのプロセスの Stop にも出す（現行挙動）。義務を誰にも見えなくする方向へは倒さない（判定不能は制限側）。

### 可視性（Stop）

- own pid が決まれば、**自分の有効所有行と共有行**を見る。**他プロセスの有効所有行は見ない・block
  しない**。
- own pid が判定不能なら**全行を見る**（現行挙動、制限側）。
- block 文の各行に `origin:` を付ける:
  - 有効所有行: `claude pid <pid> (<sessions json の name>)`（name 無しは pid のみ）
  - 共有行: `unknown (shared with every claude process of this session)`
- own pid 判定不能時は冒頭にその旨と、他プロセス所有行も表示している旨を出す。

### 作用（dismiss / mark-executed / mark-relayed / mark-denied）

- 対象にできるのは**自分の有効所有行と共有行だけ**。own pid 判定不能なら**共有行だけ**
  （判定不能で他者の義務を消さない）。
- 他プロセスの有効所有行を名指しした `relay dismiss` は**拒否**し、何も追記せず **exit 4**。stderr に
  `relay entry <id> is owned by claude pid <pid> (<name>)`（own pid 判定不能なら
  `this process's claude pid could not be determined`）と、所有プロセスで dismiss せよとの指示を出す。
- mark-executed / mark-relayed / mark-denied は他プロセスの有効所有行を一致候補から外す。その行は
  pending のまま所有者の Stop を block し続ける。

## テスト（書きかけ・未 commit）

`crates/blastguard/tests/backlog_20261117_relay_per_process_attribution.rs`（現行 blastguard binary 向け。
切り出し後に新 binary 向けへ書き直す）:

| # | テスト | 種別 | 現行 binary での結果 (2026-11-17) |
|---|---|---|---|
| i | `row_queued_by_process_a_is_not_shown_on_process_b_stop` | RED | FAIL |
| ii | `row_queued_by_process_a_is_shown_on_process_a_stop_with_origin` | RED | FAIL（`claude_pid` 未記録） |
| iii | `process_b_dismiss_of_a_row_is_refused_and_a_stop_still_blocks` | RED | FAIL（dismiss が成功する） |
| vii | `process_b_mark_executed_and_relayed_do_not_resolve_a_row` | RED | FAIL（B の mark で解決する） |
| iv | `legacy_row_without_pid_is_shown_to_both_processes` | control | ok |
| v | `own_pid_undeterminable_sees_all_rows` | control | ok |
| vi | `row_of_exited_process_becomes_shared` | control | ok |

テストが証明しないこと: 実際の祖先探索（getppid 連鎖）— テストは env seam を使う。実際の Claude Code
が全プラットフォームでこの形の sessions json を書くこと（darwin の観測のみ）。`startedAt` による
pid 再利用の検出。

## 未解決の論点

1. **`sessions_dir` の既定**: `CLAUDE_CONFIG_DIR` が設定されていたら `$CLAUDE_CONFIG_DIR/sessions` を
   使うべきか（現行仕様は `$HOME/.claude/sessions` 固定）。
2. **own pid 判定不能時の dismiss**: 共有行だけ dismiss 可とした。その結果、判定不能プロセスの Stop は
   他プロセス所有行を表示するのに解消できず、所有者が解決するまで block し続ける。これで良いか。
   人間が素の端末から `relay dismiss` する場合（claude 祖先なし）も同じ制約を受ける。
3. **本番での `BLASTGUARD_CLAUDE_PID`**: テスト seam を本番でも読む。hook の env は claude プロセスから
   継承されるので subagent の Bash からは注入できないが、ユーザーが global に設定すると全プロセスが同じ
   pid を名乗る（＝現行の共有挙動に戻る）。test-only にする（`cfg` や別名）かどうか。
4. **孤児行の共有化**（所有プロセス終了 → 共有行）は裁定の文言にはなく、義務の消失を避けるために
   本仕様で追加した解釈。sessions json が終了時に必ず消えるかは未観測（生存中の 2 件だけが残っていた
   という観測のみ）。
5. **拒否の exit code 4**: 既存は 2（使い方・未知 id）/ 3（IO）。新設の 4 で良いか。
6. **subagent が hook を起動するプロセス階層**: subagent の hook も main claude プロセスの子孫として
   起動される前提（未観測）。subagent が別プロセスで動く構成があれば祖先探索の前提が崩れる。
7. relay の forged-session 問題（`backlog_20261116_relay_forged_session_entry.rs`、session_id を偽装した
   外部 payload）とは別軸。本仕様は同一 session 内の正当なプロセス間の帰属だけを扱い、偽装 payload の
   `claude_pid` 詐称は防がない。
