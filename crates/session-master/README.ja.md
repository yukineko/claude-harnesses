# session-master

同じ git repo で複数の Claude Code セッションが動くとき、**1 つを master に定め、他を member にする**
プラグイン。push・rollout（`scripts/rollout-plugins.sh`）・main への merge・relay の裁定は master
だけが行い、member の依頼は master の queue に積まれて直列に処理される。member 同士の relay
（SendMessage）は禁止する。

## 設計

- **グループ** = git common dir が同じセッション（worktree が違っても同じグループ）。
  cwd から上に `.git` を探し、ファイルなら `gitdir:` → `commondir` を辿る（git subprocess は使わない）。
  canonicalize した common dir の sha256 先頭 16 hex が group key。
- **状態**: `$SESSION_MASTER_HOME/groups/<key>/state.json`（既定 `~/.session-master`、repo 外）。
  同ディレクトリの `lock` に排他 flock（最大約 3 秒待つ）を取り、tmp + rename で atomic に書く。
- **選出**: master が heartbeat TTL（既定 900 秒、[30, 86400] に clamp）を超えたら、最古の live
  member が昇格する。heartbeat が未来（60 秒超）・欠落のセッションは生死が**判定不能**で、
  そのとき昇格しない（候補より古い判定不能セッションが居る場合も昇格しない）。
  役割の唯一の根拠は state.json の `master` フィールド。
- **queue**: `request` で pending に積み（同一 requester/kind/command/cwd の未完了は dedupe）、
  master が `next` で 1 件ずつ in_progress にし、`complete` で done/failed にする。
  in_progress は同時に最大 1 件。結果は `results` / 次の prompt で依頼元に届く。
- **Bash 検出**（`src/detect.rs`）: brush-parser 0.4.0 の AST で判定する。文字列・コメント・
  heredoc の data は実行コードとして扱わない。wrapper（前置代入・env・command・exec・builtin・
  nohup・nice・time・timeout・sudo・xargs）を剥がし、`$(...)`・バッククォート・プロセス置換・
  `sh -c`・`eval`・shell に渡る heredoc・関数本体・複合コマンドを再帰で読む（深さ上限 8）。
  main への merge は対象 worktree の `HEAD` が `SESSION_MASTER_MAIN_BRANCHES`（既定 `main`）の
  ブランチを指すときだけ。`condukt worktree merge` / `gh pr merge` は常に merge 扱い。
  `git send-pack` / `git http-push` は push 扱い、`ssh host <command>` のリモートコマンドも再帰で読む。
  git の subcommand 位置が非 literal（`git $X`・`git "$@"`）なら、文字列に push 等が無くても判定不能。
  0.1.4 から、判定不能は**本文に push 等の文字列があるかどうかに依存しない**。command 名・
  `eval` / `sh -c` / `ssh` リモート / `find -exec` / `source` / shell の script 引数が非 literal
  （変数・置換・glob）なら判定不能。git の未知の subcommand（alias の可能性）・`-c alias.*` も
  判定不能。shell は sh/bash/zsh/dash/ksh/mksh/fish/csh/tcsh/ash を扱い、`sh < file` の
  stdin script も名前で判定する。`--git-dir` / `GIT_DIR=` を伴う HEAD を動かす git 操作は
  その git dir の `HEAD` で判定する。main を動かす操作として merge/pull/reset/cherry-pick/
  revert/rebase/am（HEAD が main のとき）、`fetch <src>:main`、`update-ref refs/heads/main`、
  `branch -f/-m/-d main` を merge 扱いにする。
  未知のプログラムは、引数に git/gh/condukt/ssh/eval/shell/`session-master`/`rollout-plugins`
  が現れたら wrapper の可能性があるので判定不能（`setsid git push`・`xcrun git push` 等）。
  member の `session-master` 呼び出しのうち、master 専用 subcommand・他人の `--session`・
  `CLAUDE_CODE_SESSION_ID` の代入を伴うものは Deny（queue には積まない。暫定の扱い 6 を参照）。
- **SendMessage**（`src/relay.rs`）: master からは allow。`main` 宛て（subagent → 自 session）は allow。
  宛先が登録済みセッションの name / id（8 文字以上の prefix 可）なら、それが master のときだけ allow、
  member なら Deny。自 session の transcript に現れる subagent（agentId / Agent・Task tool_use の name）
  なら allow。それ以外は Deny。

## fail-closed の扱い

判定不能は **clean に潰さず、必ず制限側（Deny）に解決する**。

| 判定不能の原因 | 結果 |
|---|---|
| payload を読めない・parse できない | Deny |
| repo が見つからない・`.git` が読めない・canonicalize 失敗 | master 専用操作なら Deny（「判定不能」と原因を reason に書く） |
| state.json の読み書き失敗・lock 取得失敗 | 同上 |
| `SESSION_MASTER_NOW` が不正 | 同上（CLI は exit 2） |
| 構文解析に失敗・command 名や `-c`/`eval` 引数が非 literal・深さ上限超過・git の未知 subcommand・未知プログラムの引数に git 等 | 本文の文字列に関係なく master 専用扱い（kind `other` で queue へ） |
| merge 対象の HEAD が読めない・同じ program に cd/pushd/popd/checkout/switch | master 専用扱い |
| 分類器の panic | 判定不能 → master 専用扱い |
| gate 本体の panic | `run_pretooluse_guarded` が exit 2（block） |
| SendMessage の宛先が確かめられない | Deny |
| launcher が binary を見つけられない | `gate` は deny JSON、`hook *` は stderr + exit 1 |

master 専用でない Bash は、heartbeat に失敗しても allow のまま（判定がそこに依存しないため。
失敗は stderr に出す）。SessionStart / UserPromptSubmit で役割を判定できないときは、
additionalContext に「役割を判定できない（判定不能）。master 専用操作は Deny される」と出す
（沈黙にしない）。

## hook 配線（`hooks/hooks.json`）

| event | command | 動作 |
|---|---|---|
| SessionStart | `session-master hook session-start` | register。役割・master・（master なら）queue 件数を additionalContext に出す |
| UserPromptSubmit | `session-master hook prompt` | heartbeat。昇格/降格の明示、member には未配達 results（配達済みにする）、master には件数と先頭 item |
| PreToolUse (`Bash\|SendMessage`) | `session-master gate` | §設計の判定。呼ばれるたびに heartbeat |
| Stop | `session-master hook stop` | heartbeat。block しない。master で queue が残れば systemMessage |
| SessionEnd | `session-master hook session-end` | leave（master なら最古 live member に即交代） |

panic barrier: PreToolUse は `harness_core::hook::run_pretooluse_guarded`、Stop は
`harness_core::gate::run::run_guarded`（stdin 読み取り失敗は `read_stop_payload_guarded`）。
session-start / prompt / session-end は `catch_unwind` で包み、panic は stderr + exit 1。
`run_hook` はどこにも使わない。

## CLI

`register [--name N]` / `heartbeat` / `leave` / `name <NAME>` / `role` / `status` /
`request --kind push|rollout|merge|relay|other --command C [--note T]` / `next` /
`complete <ID> --outcome ok|failed [--result T]` / `results [--ack]` / `gate` /
`hook session-start|prompt|stop|session-end` / `--version`。
`--session` 省略時は `CLAUDE_CODE_SESSION_ID`、`--cwd` 省略時はカレント。
exit: 0 = ok（role は master）、1 = 拒否（member / in_progress でない。role は member）、
2 = 判定不能（stderr に理由）または usage error。

環境変数: `SESSION_MASTER_HOME` / `SESSION_MASTER_NOW`（テスト用の時刻注入）/
`SESSION_MASTER_TTL_SECS` / `SESSION_MASTER_MAIN_BRANCHES`。

## 有効化手順

この版では rollout も enabledPlugins の変更もしていない。有効化するときは:

1. `scripts/build-plugin-bin.sh session-master` で `bin/session-master-<os>-<arch>` を作る。
2. `scripts/rollout-plugins.sh` で稼働ハーネスへ反映する（手動 `cp` はしない）。
3. `~/.claude/settings.json` の `enabledPlugins` に session-master を加える。
4. 各セッションで `session-master name <NAME>` を実行し、SendMessage の宛先名を登録する。

## 暫定の扱い（coordinator 裁定 2026-11-22）

次の各点は **暫定** であり、運用を見て見直す:

1. PreToolUse gate は `harness_core::hook::run_pretooluse_guarded`、Stop は
   `harness_core::gate::run::run_guarded` で panic barrier を張る。`run_hook` は使わない。
2. askrelay（セッション内 subagent の Ask relay）は変更しない。この gate の対象は
   **セッションを越える** SendMessage。
3. SendMessage の宛先名は `session-master name` で登録する。宛先が master または
   自セッションの subagent と確かめられないときは Deny。
4. master の queue が残っていても Stop は block せず、systemMessage で知らせるだけ。
5. GATE_CRATES には登録しない。
6. **限界**: この gate は Claude Code の PreToolUse を通る呼び出しに掛かる**協調的な制御**であり、
   認証ではない。`session-master` CLI は `--session` / `CLAUDE_CODE_SESSION_ID` の値をそのまま
   自分の id として信じるので、gate を通らない経路（人間の端末、hook を無効にした session、
   gate が読めない形で実行されたプログラムの中など）から CLI を直接呼べば、他セッション
   （master を含む）の id を詐称して `next` / `complete` / `leave` 等を実行できる。
   gate 側は member の Bash で、master 専用 subcommand（`next` / `complete` / `gate` / `hook`）、
   自分以外の `--session`、`CLAUDE_CODE_SESSION_ID` の代入を含む `session-master` 呼び出し
   （絶対パス・wrapper・`bash -c` 経由を含む）を Deny する（queue には積まない）が、
   CLI 単体での id 詐称そのものは防げない。
7. **Bash 検出の残る限界（0.1.4）**:
   - script ファイルの**中身は読まない**。`bash x.sh` / `source x` / `python3 x.py` /
     `make` / `npm test` / cargo alias・build.rs などが内部で push しても検出しない
     （名前が `rollout-plugins.sh` の script だけは rollout 扱い）。
   - 未知のプログラムは、引数に git 等の名前が**現れない**限り allow する（全面 Deny にすると
     通常の member 作業が止まるため）。引数に名前を出さずにコマンドを実行するプログラム
     （設定ファイルから読むもの等）は検出しない。`sed` の `e` コマンドも読まない。
   - 既知の安全リスト（echo/cat/ls/grep/rg/sed/jq/cargo/rustc/rustfmt 等）は引数を見ない。
     `tdd --cmd`・`trap`・`alias`・`rustup run`・`tar`・`zip`・`fd`・`rg --pre` は安全リストから外し、
     未知プログラムとして引数を見る。
   - **過剰検出**: 非 literal の command 名（`$EDITOR a.rs`）、非 literal 引数の `rg`、
     `bash "$f"` のように最後の path 要素が展開される script 指定は Deny になる
     （`. "$HOME/.cargo/env"` のように最後の要素が literal なら名前で判定して allow）。
     引数のトークン（basename）が git 等と一致する未知プログラム（例: `docker exec c git status`）も
     Deny になる（`git-notes.md` のような部分一致は対象外）。
     上記はコードからの予測で、通常作業コマンド群に対する gate の観測による測定は未実施
     （計測用 script の実行が blastguard に止められたため）。
   - **`git commit` on main は allow のまま**: verifier の P7 は main 上の `git commit -m m` の
     Deny を求めるが、SPEC §5.1 と既存テスト（`gate_bash::member_non_push_commands_allowed`・
     `gate_bypass::control_member_benign_allowed`）は同じ cwd（main 上の repo）での
     `git commit -m ...` の allow を求めており、両立しない。裁定待ち。
