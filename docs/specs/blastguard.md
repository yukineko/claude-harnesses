> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# blastguard 仕様

## 概要

`blastguard` は Claude Code の **PreToolUse** フックであり、エージェントが実行しようとしている
ツール呼び出しを stdin から受け取り、純粋関数 `detect::detect(tool_name, tool_input) -> Decision`
で allow / deny を判定し、**deny のときだけ** PreToolUse の `deny` JSON（`hookio::deny_json`）を
stdout に一行出力してその操作を実行前に握り潰す。判定対象は `Bash` と `Write` のみで、`Edit` /
`MultiEdit` / `NotebookEdit` は部分編集なので常に allow に倒す（`detect` の `_ => Allow`）。設計は
意図的に**非対称・保守的**で、「明らかに破壊的で不可逆」なパターンのみ deny し、曖昧なものはすべて
allow へフォールスルーする。slash command / agent を持たない **skill/command-less な hook+binary
プラグイン**であり、API キー不要で subscription-native（`hooks/hooks.json` の1エントリ + 同梱バイナリ
だけで完結）。加えて `src/lib.rs` が検出ロジックを純粋関数として他クレートへ公開する（specguard の
forge・condukt のスケジューラが再利用）。

## 不変条件

- **ターンを決して壊さない（ハード不変条件）** — 入力が空 / 不正（`HookInput::parse` が `None`）、
  対象外ツール、内部 panic のいずれでも黙って exit 0 する。panic の握り潰しは
  `harness_core::hook::run_hook` が保証し、`main::run` は parse 失敗時に無出力で `return` する。
  統合テスト `empty_stdin_is_silent` が空 stdin での exit 0・無出力を固定する。
- **allow は完全に無出力** — deny のときだけ JSON を出す。`main::run` は `Decision::Allow` で何も
  print しない（`config_file_edit_is_allowed_silently` / `benign_command_is_allowed_silently` が固定）。
- **PreToolUse deny JSON は単一行** — `hookio::deny_json` は
  `hookSpecificOutput.{hookEventName:"PreToolUse", permissionDecision:"deny", permissionDecisionReason}`
  を serialize し、`hookio` の `deny_json_has_required_shape` テストが改行を含まないことを保証する。
  `reason` は `Decision::Deny(String)` の中身がそのまま agent へ surface される。
- **判定は純粋・決定論** — `detect` / `classify` / `exclude` は I/O を持たず、`(入力) -> 出力` の
  純関数。config allowlist は `exclude::glob_set()`（`OnceLock<GlobSet>`）でメモ化するのみ。
- **detect と classify の lockstep** — `classify::classify` の tier-2「ローカル破壊」判定は
  `detect::detect("Bash", …)` を再利用するため、binary の deny 集合と classifier の High/irreversible
  集合が乖離しない（同一検出器を共有）。
- **偽陽性ゼロ・バイアス** — 曖昧な入力は allow。Rust の戻り型矢印 `->`（`prev == b'-'`）と山括弧
  プレースホルダ `<value>` / `<run-id>`（`is_angle_placeholder_close`。hyphen を含む kebab-case
  プレースホルダも placeholder と認識する）は truncating redirect と誤認しない。
  クォート内の破壊的テキスト（`echo 'rm -rf /'`）は実行されないので allow。opaque な
  `source <file>` / `bash <script>`（`-c` 無し）は中身を検査できないため allow。
- **`>` スキャンは UTF-8 char 境界安全** — truncating redirect スキャナの whitespace/delimiter 判定は
  ASCII バイトのみで行う（ASCII バイトは常に UTF-8 char 境界）。生バイトを `char` にキャストすると
  多バイト文字の継続バイト（例: 頻 = `E9 A0 BB` の `0xA0`）を no-break space 等の whitespace と
  誤読して char 境界の途中でスキャンを切り、`seg[start..j]` の slice で panic する。`>` に隣接する
  日本語等の多バイトテキストを含むコマンドで classification が crash しないことを 5 本の回帰テストが固定。
- **config ファイル除外** — `exclude::is_config_file`（`.claude/**`, `**/settings.local.json`,
  `**/package.json`, `*.toml`/`*.yaml`/`*.yml`/`*.lock`, `.config/**` 等の glob）に一致するパスは、
  破壊的な形（空内容 Write、再帰/ワイルドカード rm、truncating redirect target）でも allow。
  `exclude::normalize` がクォート除去・`\`→`/`・先頭 `./` 除去を行う。

## 振る舞い

`main` は stdin を触る前に `--version`/`-V` と `--help`/`-h` のみ短絡する最小 CLI。通常モードは
`hook::read_stdin` → `HookInput::parse` → `detect::detect` → deny 時のみ `deny_json`。

- **`Write` の deny（`detect_write`）** — path を `file_path`/`notebook_path`/`path` から抽出。
  config ファイルなら allow。`.git/**` 内部（`is_git_internal`）への Write は deny。それ以外で
  `content.trim()` が空なら「既存ファイルを空内容で抹消」として deny。content 有り or config なら allow。
- **`Bash` の deny（`detect_bash`）** — (1) fork bomb（whitespace 除去後 `:(){` かつ `:|:`）、
  (2) 単一 `>` truncating redirect（`single_redirect_target`。`>>`/`2>`/`&>`/`>&`/矢印/山括弧は除外、
  target が `/dev/null|/dev/stdout|/dev/stderr` か config なら safe）、(3) `split_segments` による
  quote-aware なコマンド分割（`;`/改行/`&&`/`||`/`|`/`&`）＋各 segment の `analyze_segment`。
- **`analyze_segment`** — `command_index` が先頭の `VAR=val` 代入と benign wrapper
  （`sudo`/`doas`/`nohup`/`env`/`command`/`time`/`nice`/`ionice`）を読み飛ばして実コマンドを特定。
  `--help`/`-h` を含む segment は allow。shell-eval ラッパは `MAX_SHELL_DEPTH`(8) まで再帰展開:
  `eval`/`exec`/`source`/`.` は残り語を、`sh|bash|zsh|ksh|dash -c <payload>` は payload を
  `detect_bash` へ再投入する。
- **コマンド別 deny 規則** — `rm`（`analyze_rm`: 再帰 `-r/-R/--recursive` またはワイルドカード
  operand。ただし全 operand が config なら allow。**保護パスは config 免除より先に判定され、
  免除を上書きする**＝`is_protected_path`/`protected_tree_deny` が `is_config_file` に優先）、
  `git`（`analyze_git`: `clean -f` + `-d`/`-x`、`clean -f` 単体でも追跡外ファイルを消すので deny、
  `reset --hard`、`checkout --force`/`-f`、`checkout -- .`（whole-tree）、`switch -f/--discard-changes`、
  `stash` の `-a/--all/-u`（追跡外・無視ファイルを working-tree から除去）、`git rm`（`--cached` 含む・
  保護パス target のみ）、および `checkout`/`restore` の operand が保護パスの場合。**通常の追跡ファイルへの
  `git checkout -- <path>` は allow**＝保護パス限定に絞り、日常操作を blanket deny しない）、
  `find`（`analyze_find`: `-delete`、`-exec`/`-execdir` の直後が shell または `rm`）、
  `truncate`/`shred`/`dd of=`/`mkfs*`、再帰 `chmod`/`chown`（`-R`/`--recursive`）、
  `sort`/`uniq`（read-only ではない: `sort -o <保護パス>`／`uniq <in> <保護パス>` は出力ファイルを
  truncate するので deny。純粋な読取・非保護 output は allow — `analyze_sort`/`analyze_uniq`）。
- **managed な使い捨て場所での working-tree 破棄の Allow（ユーザー裁定 2026-11-07、blastguard 0.2.287）** —
  `git -C <dir>`（または discovery で決まる作業ツリー）が worktree storage root 直下の **linked worktree
  checkout そのもの**だと観測できたときに限り、working-tree 破棄の arm（`clean -f`・`reset --hard`・
  `checkout --force`/`-f`・`restore`・`switch -f`・`stash -u/-a` 等）を allow する（`detect/git.rs` の
  `managed_worktree_git` → `scope/checkout.rs` の `SafeRoots::is_linked_worktree_checkout`）。
  top-level の単一 git コマンドで、subcommand 前の global option が `-C <dir>`（1 回まで）と
  `--work-tree`（同じ checkout を指すもの）だけのときに限る（`--git-dir`・`-c` 等があれば不成立。
  alias 本体や `bisect run git …` のような入れ子の git は対象外）。
  `Known(true)` は、セッションが refused でない・checkout probe がある・綴りに `..` が無い・storage root
  直下ちょうど 1 component（`.git` でない）・実ディレクトリに `gitdir:` で始まる `.git` **ファイル**がある
  （main checkout は `.git` ディレクトリなので `Known(false)`）・root 以下に symlink が無い、を**全て観測した**
  ときだけ返る。probe 失敗・refusal・symlink・置けない綴りは `Undetermined` で、呼び出し側は
  `Known(false)` と同じく**元の verdict を保持**する（Allow へ倒さない）。main の作業ツリー・`.git` 内部・
  保護パス・`git stash drop/clear`（全 worktree で共有）は対象外のまま。
- **未マージ branch の force-delete / dirty worktree の force-remove は Ask（`detect/git_unmerged.rs`、
  backlog 43034f75、blastguard 0.2.295）** — `git branch` で delete（`-d`/`-D`/`--delete`、短縮束を含む）と
  force（`-f`/`-D`/`--force`、順序・束ね方を問わない）が揃うとき、**全 operand** について、default branch
  の ref（`refs/remotes/origin/HEAD` の symbolic ref → 存在する `init.defaultBranch` → 存在する
  `main`/`master`/`trunk` がちょうど 1 つ、の順。最後の段は repo に記録された事実ではなく名前による
  heuristic で、前 2 段が何も与えないときだけ使う。HEAD では代用しない）から
  `merge-base --is-ancestor`（exit 0 = 祖先 / 1 = 非祖先）で到達可能なら Allow、非祖先・default branch
  自身の削除は Ask。APFS 等の folding する FS では `-D Main`・`-D maſter`（ſ→s）が `main`/`master` の
  loose ref file を開いて消すので、operand の `refs/heads/<op>` が `for-each-ref` の列挙と byte 一致
  しないときは、`--git-common-dir` 下の loose ref file の (device, inode) が一致する列挙 ref を「実際に
  消える ref」として判定する（文字列 folding では再現しない。一致 0 件・複数・stat/probe 失敗は Ask。
  blastguard 0.2.299）。存在しない branch（`rev-parse --verify` exit 1）は何も消えないので Allow。
  `git worktree … remove` に force（verb の前後どちらでも）があるとき、各 operand で
  `git status --porcelain --ignored` が exit 0 かつ空なら Allow、出力があれば Ask（ignored file も
  削除されるので数える。`target/` のような build 出力だけでも Ask）。probe は git が実際に作用する repo
  （累積 `-C`〈相対は segment の base 起点〉・`--git-dir`・`--work-tree`）で走らせ、相対 worktree path は
  その `-C` directory 起点で置く。判定不能 — operand 無し・base 不明（`cd "$VAR"` 後など）・置けない/
  未知の global option・default branch 解決不能・probe の想定外 exit / spawn 失敗 / timeout — は全て Ask。
- **whole-tree staging は Ask（`detect/git_stage.rs`、backlog e9325654、blastguard 0.2.300）** —
  他セッションの未 commit 作業を自分の commit に巻き込むため。argv だけで判定し、どの repo に作用するか
  （`-C`・`--git-dir`・`--work-tree`・`-c`）には依存しない（probe しない。linked worktree の免除も無い）。
  `git add` / `git stage` は `-A`・`--all`・`--no-ignore-removal`・`-u`・`--update`・`--renormalize` を
  **pathspec 無し**で使うと Ask（pathspec 付きならその pathspec を判定する）。`git commit` は `-a` /
  `--all`（短縮束 `-am`・`-va` を含む）で Ask（pathspec 併用でも Ask。git 自身が拒否するので無害）。
  短縮束は git の parse-options どおり読む: `-m`/`-F`/`-C`/`-c`/`-t` の後ろは値、`-u`/`-S` は stuck の
  optional 値なので、`-mabc`・`-Fa`・`-uall` の `a` は flag ではない。pathspec が whole-tree
  （字句正規化で `.`/`..` 成分だけ = cwd か祖先〈`.`・`./`・`..`・`a/..`・空〉、`:` 始まりの magic
  〈`:/`・`:!x`〉、glob 文字 `*`/`?`/`[` を含む）なら `add`・`commit` とも Ask。`--pathspec-from-file` も
  Ask。long option の省略形は git と同じく前方一致で読み、候補に sweep 系が含まれれば Ask（`--al`・`--up`）。
  判定不能 — 閉じない quote / substitution、空白前の backslash、未知の option・短縮 flag、flag / pathspec
  位置の runtime 展開（`$VAR`・`$(…)`・`` `…` ``・`$'…'`・先頭 `~`）、quote 外の展開・glob・brace list
  （shell が語を割りうる。`-m *` は全 file を pathspec にする）— は全て Ask。double quote 内の展開が
  値を取る option の値である場合（`-m "$(cat <<'EOF' … EOF)"`）は 1 語として値に消費されるので Allow。
  **残余（未対応）**: 明示的な directory pathspec（`git add src/`、repo root を指す絶対 path）は Allow の
  まま（file と区別するには filesystem を読む必要がある）。`add -A` / `commit -a` を本体に持つ git alias、
  `GIT_*` 環境変数。**here-document**: `$(…)` 内の here-document 本文は data として読む（本文中の
  apostrophe `don't` は quote ではなく、`)` も何も閉じない。終端は delimiter と完全一致する行、`<<-` は
  先頭 TAB のみ除去）。引数は改行込みの元 segment から読む。**過剰側（意図的に Ask）**: 終端行が無い
  （字下げ・長い行の部分文字列・欠落）、delimiter が読めない、quote なし delimiter の本文に `$`・
  `` ` ``・`\` がある（展開される）、backtick 内の here-document、引数 token を元 segment に対応付け
  られない（空白で再結合され here-document が常に未終端に見える）場合は語が判定不能になり Ask（reason に
  `git commit -F - <<'EOF'` を案内する）。
- **git の exec-capable 形は一律 Deny（backlog fb338793、ユーザー裁定 2026-11-07「git bisectでいろいろ
  構文を処理できてしまうくらいなら使用を禁止した方がいいのでは?」）** — git が呼び出し側の選んだコマンドを
  実行する形は、埋め込みコマンドの中身に関わらず Deny（`detect/git.rs` の `git_exec_ban`。
  `analyze_git_words` の先頭で判定し、末尾の `_ => Allow` に到達させない。`git bisect run true` も Deny）。
  対象: global `-c` で command-valued key（`core.pager`/`core.sshCommand`/`core.editor`/`sequence.editor`/
  `diff.external`/`pager.<cmd>`/`alias.<x>=!…`/`credential.helper` 等。`detect/executors.rs` の
  `git_config_command`）を設定、同じ key を `git config [scope] <key> <value>`/`git config set` で書く、
  `git clone -c`/`--config` で設定、`bisect run`、`rebase`/`pull` の `-x`/`--exec`、`submodule foreach`、
  `difftool`/`mergetool` の `-x`/`--extcmd`、値付きの `grep -O<cmd>`/`--open-files-in-pager=<cmd>`
  （値は glue された形だけ。値なしの `-O`/`--open-files-in-pager` は設定済み pager を起動するだけなので Allow）、
  `fetch`/`pull`/`clone`/`ls-remote`/`push`/`archive`/`fetch-pack`/`send-pack` の
  `--upload-pack`/`--receive-pack`/`--exec`（`clone`/`ls-remote` は `-u` も）、`send-email` の
  `--to-cmd`/`--cc-cmd`/`--header-cmd`/`--sendmail-cmd`。語は shell の quote を外してから見る。
  `filter-branch` は従来どおり `irreversible_git_subcommand` が全用途を Deny。埋め込みコマンドが spelled out
  されていてそれ自体が Deny のとき（`git bisect run rm -rf /usr`）はその Deny 理由を返す（system-harm
  marker を保つため。`detect.rs` の git arm）。非対象（Allow のまま）: `bisect start/good/bad`、
  `rebase <upstream>`、`-c pager.log=false`、空値（`-c credential.helper=`）、`!` で始まらない alias、
  値が環境変数にある `git --config-env <key>=VAR`（従来の Ask。既存 control が固定）、そして
  **既知の安全値**（ユーザー裁定 2026-11-07「Allow known-safe values」。`detect/executors.rs` の
  `is_safe_command_value`）: 先頭語が key ごとの固定リストにあり、以降が素の flag（`-R`・`--wait` 等。
  `=value`・展開・`;`・quote・operand を含まない）だけの値 — pager（`core.pager`/`pager.<cmd>`）は
  `less`/`more`/`cat`、editor（`core.editor`/`sequence.editor`）は `vim`/`nano`/`code`/`true`/`:`、
  `credential.helper`（`credential.<url>.helper` 含む）は `osxkeychain`/`store`/`cache`、`gpg.program` は `gpg`、
  `core.sshCommand` は `ssh`（flag の後の素の値語〈`-i key`〉は可。`o`/`F` を含む flag〈`-o ProxyCommand=…`・
  `-F <config>`〉は command を運ぶので不可）。`-c` と `git config` 書き込みの両方に適用。リスト外の値
  （`vim; rm -rf ~`・`vim $(…)`・`!…`・`ssh -o ProxyCommand=sh` 等）は ban のまま Deny。
- **system-harm（システムを壊すコマンド）は常に Deny、repeat でも降格しない（backlog fb338793、ユーザー裁定
  2026-11-07「いかなる理由であれシステムに害になるコマンドはdenyすべし」「1は絶対にdenyでいい」）** —
  `rm` の operand が `/` またはシステムディレクトリ（`scope::is_inside_system_dir`: `/bin`・`/etc`・`/usr`・
  `/System`・`/Library`・`/opt`・`/private/etc` 等。`/var`・`/dev` は含まない）の中を指すとき、非再帰の
  1 ファイル（`rm /etc/passwd`・`rm -f /bin/ls`）でも Deny（`detect/rm.rs` の `rm_system_harm_deny`。
  字句の絶対パスと、親ディレクトリを symlink 解決したパスの両方で判定。ワイルドカードは literal prefix で
  判定。セッションの safe root 内〈`/opt/myproj` の project 等〉は対象外）。この Deny、システムディレクトリへの
  書き込み Deny（`system_path_block`）、`crate::deletion` のシステムディレクトリ refusal は理由に
  `model::SYSTEM_HARM_MARK` を含み、`main::downgrade_on_repeat` は `Decision::is_system_harm` の Deny を
  2 度目以降も Ask に落とさない（他の Deny の repeat 降格〈運用者裁定 2026-09-18〉は不変）。
- **埋め込み shell 文字列の Ask と osascript の削除（backlog 0fefcd3b）** — interpreter program の
  文字列 literal（layer > 0）に対する shell pass の **Ask** は、program が shell に文字列を渡す API
  （`SHELL_STRING_EXEC_MARKERS`: `os.system`/`os.popen`/`subprocess`/`system(`/backtick/`child_process`/
  `do shell script` 等）を含むとき pending Ask として残す（従来は捨てられ、
  `os.system('echo x > .git/hooks/pre-commit')` が Allow だった。top-level の同じ redirect は保護パス Ask で、
  人間不在時は Deny）。「command word が展開」由来の Ask（`detect::EXPANSION_HEAD_TAIL`）は残さない（`os.system('echo "$HOME"')`・argv list `['echo','$HOME']` の literal は data。`interpreter::is_expansion_head_ask` で除外、blastguard 0.2.301）。
  `osascript` は、program が `"Finder"`/`"System Events"` を名指しし、かつ double-quote 文字列の外に
  `delete` か `empty … trash` があれば Deny（`detect/interpreter/osascript.rs` の `osascript_app_delete`。
  `display notification "x"` 等は従来どおり）。
- **disarm-by-non-write 規則（保護パスを『書かずに無力化』する経路）** — 保護ファイル
  （`.claude/settings.json`・`settings.local.json`・`hooks.json`・`hooks/**`、`.githooks/**`、
  `.git/hooks/**`、shell rc、gate config toml）に対する削除（`rm`〈再帰 `-r` に加え空ディレクトリ削除
  `-d`/`--dir` の保護コンテナも `protected_tree_deny`〉・`unlink`・`rmdir`）、退避
  （`mv` source）、実行・読取権剥奪（`chmod` で owner の exec **または** read bit を落とす mode。
  shell hook は shebang 実行のため read も必須で、`chmod -r`／`chmod 311` は exec を残しても hook を
  無力化する〈`mode_removes_exec`／`mode_removes_read`〉）、index からの復元
  （`git checkout -- <protected>`・`git restore`）、append（`tee -a`・`>>`）、in-place 置換
  （`sed -i` 等）、および保護パスを内包するディレクトリへの再帰 `cp`（landing 集合が静的に
  確定できないため **Ask**）。パスは `normalize`＋`resolve_parents` で `..`/`.`/`//` を字句解決して
  から判定する（`..` で allowlist 側に滑り込ませる bypass を塞ぐ。解決不能・曖昧は制限側）。
  それ以外は allow。
- **inert literal の Deny→Ask 降格（`detect/interpreter/inert_literal.rs`、backlog 27795e6e / b4c03877）** —
  interpreter program（`python3 -c` / stdin heredoc 等）の破壊的な綴り（`unlink(` 等）が、**全出現**とも
  「実行され得ない」と**積極的に証明できた**素の quoted literal（または `#` コメント）の中にしか無いときに限り、
  その綴り由来の Deny を Ask に落とす（Allow にはしない）。証明できない構文（prefix 付き literal・
  補間/escape を含む body・triple quote・backtick・regex/here-doc/他コメント様式など）は `None` を返し
  Deny を保つ。さらに program が動的実行の marker（`DYNAMIC_EXECUTION_MARKERS`: `exec`/`eval`/
  `compile`/`subprocess`/… および文字列引数を実行する `timeit`/`cprofile`/`profile.run`/`pdb.run`/
  `settimeout`/`setinterval`/`vm.run`/`new function`）を 1 つでも含めば降格しない（substring
  heuristic なので false hit は Deny 側に倒れる）。program 内の別の実呼び出しが出す Deny は降格に優先する。
- **stdin here-document の program text は body だけ（`detect/payload.rs` の
  `analyze_interpreter_stdin_exec`、`detect/heredoc.rs` の `first_line_quoted_here_document_body`、
  backlog aafeaa05）** — `python3 - <<'PY'` 等で stdin から program を受け取る stage の program text
  （user ruling R2 の「program text 中の `>` は判定不能な redirect → `RECOMMEND_NO` 付き Ask」を含む
  全 program 検査の対象）は、その stage が**1 行目で quoted delimiter の here-document を開き**、
  終端行が見つかるときに限り、opener 行と終端行の**間の行だけ**とする。終端行より後の行は stdin に
  渡らない普通の shell であり、`detect_bash` の他の全規則が通常どおり判定する（終端後の
  `echo '>'`／`grep '^>' x` の quoted `>` を program text と誤認しない。終端後の実 redirect
  `echo x > .git/hooks/pre-commit` は従来どおり Deny）。次のいずれかに当たれば従来どおり
  「最初の改行より後の全文」を program とする（制限側の過大近似）: space/tab/newline 以外の空白
  （`\r` 等）を含む、1 行目の quote 不均衡・行末 `\`・`<<` が（quote を見ずに数えて）ちょうど 1 個でない、
  delimiter が unquoted または空、stage が `<<` を含まないか 1 行目の部分文字列でない、終端行が無い、
  終端行より後に `<<` が（文字列として）1 つでもある。

### module 責務

- **`main`** — CLI エントリ。version/help 短絡 → `hook::run_hook(run)`。`run` が stdin parse →
  `detect` → deny 時のみ `deny_json` を print。ターン非破壊契約の実装点。
- **`detect`** — 純粋な破壊操作検出器。`detect`（tool 名で dispatch）、`detect_write`、`detect_bash`、
  segment 分割（`split_segments`）、redirect 解析（`single_redirect_target`/`is_angle_placeholder_close`/
  `redirect_target_is_safe`）、shell-eval 再帰展開（`command_index`/`is_shell`/`dash_c_payload`/
  `strip_wrapping_quotes`）、コマンド別 analyzer（`analyze_rm`/`analyze_git`/`analyze_find`）。I/O なし。
- **`exclude`** — 「決してブロックしない repo config ファイル」の allowlist。`ALLOW_GLOBS` 定数、
  `glob_set()`（`OnceLock<GlobSet>` メモ化）、`normalize`、`is_config_file`、`is_git_internal`。
- **`classify`** — planned action の graded risk/reversibility 分類器（`RiskAssessment{risk, reversible}`,
  `Risk::{Low,Medium,High}`, `requires_gate()` = High かつ not reversible）。precedence: (1) outward
  deploy/publish/push（tier-1 `DEPLOY_SIGNALS` 部分文字列 + tier-2 `DEPLOY_VERBS`×`OUTWARD_TARGETS` を
  `has_word` 単語境界で照合）→ High/irreversible、(2) ローカル破壊（`detect` 委譲）→ High/irreversible、
  (3) history rewrite（`HISTORY_SIGNALS`: rebase/`--amend`/filter-branch）→ Medium/reversible、
  (4) その他 → Low/reversible。`has_word` が `ownership`⊃`ship` 等の偽陽性を防ぐ。lib 経由で condukt が
  GATED ゲート強制に利用する。
- **`model`** — 判定型 `Decision::{Allow, Deny(String)}`（`deny()`/`is_deny()`）。system-harm の Deny は
  `deny_system_harm()` で理由に `SYSTEM_HARM_MARK` を付け、`is_system_harm()` で識別する。
- **`hookio`** — PreToolUse `deny` の単一行 JSON 生成（`deny_json`）。
- **`lib`** — 上記 module を公開し、検出ロジックを他クレート（specguard forge・condukt scheduler）へ
  再利用可能にする純粋 API（I/O なし）。

## 一般化リスクスコアリング (diff-level 意味的リスク)

> **REVIEW-NEEDED**: コードから逆算 (2026-07-09 セッション)。人間レビュー前は正典としない。

### 概要

`diffrisk` module は `classify` の command-text 分類（tier-1/2 deploy 語彙、`detect` 委譲のローカル
破壊、history rewrite）に加えて、**diff-level の意味的リスク**を同じ `RiskAssessment` 軸へ合流させる。
判定材料は2つ: (1) 変更パスが機微パス glob（auth/authn/authz/login/session/oauth/credential/secret、
payment/billing/checkout/stripe、pii/personal_data/gdpr の既定 `DEFAULT_SENSITIVE_GLOBS`。
`SensitiveConfig::with_extra_globs`/`from_globs` で呼び出し側が拡張・置換可能）に一致するか、
(2) unified diff テキストに Rust の公開シンボル追加/変更（`pub fn`/`pub struct`/`pub enum`/
`pub trait`/`pub const`/`pub static`/`pub type`/`pub mod`、`pub(crate)` 修飾も同様に扱う）を示す
`+`/`-` 行があるか。`classify::classify_change(text, paths, diff_text, sensitive)` が command 分類と
diff 分類を `RiskAssessment::merge` で統合し、condukt の `gate check` / force-gate エスカレーションは
1つの `requires_gate()` 軸だけを見ればよい（並行パス無し）。

### 不変条件

- **機微パス glob は設定可能** — 既定は `DEFAULT_SENSITIVE_GLOBS`（auth/payment/PII 系）。
  `SensitiveConfig::with_extra_globs` は既定に追加、`SensitiveConfig::from_globs` は既定を使わず
  呼び出し側リストのみを採用する（プロジェクト固有の機微パスを完全に所有したい場合向け）。
- **純関数** — `diffrisk` module は I/O を持たない。`classify_diff`/`changes_public_symbol`/
  `SensitiveConfig::any_sensitive` は `(入力) -> 出力` の決定論的写像（`SensitiveConfig::build` の
  `GlobSet` 構築もメモ化なしの純計算）。
- **既存の破壊コマンド分類は不変で追加的** — `classify::classify`（tier-1/2 deploy、`detect` 委譲の
  ローカル破壊、history rewrite、既定 Low）のロジック・precedence は一切変更しない。
  `classify_change` は `classify(text)` と `diffrisk::classify_diff(...)` を `merge` するだけで、
  diff signal が既存の deploy/破壊判定を弱める方向には作用しない
  （`classify_change_still_force_gates_deploy_text_regardless_of_diff_signals` テストが固定）。
- **公開シンボル変更検知は diff header `+++`/`---` を誤検知しない** — `changes_public_symbol` は
  行頭の `+`/`-` マーカーの直後が更に `+`/`-` で始まる場合（unified diff のファイルヘッダ行）を
  除外してから `pub …` プレフィックスを照合する。プレーンな context 行（マーカー無し）も対象外
  （`diff_file_headers_are_not_mistaken_for_pub_markers` テストが固定）。
- **merge は max risk かつ both-reversible のみ reversible** — `RiskAssessment::merge` は
  `risk` を2つの `Ord` 実装 (`Low < Medium < High`) の max、`reversible` は両方が true の場合のみ
  true（どちらかが irreversible なら結果も irreversible。希釈させない）。

### 振る舞い

機微パス glob 該当（auth/payment/PII）と公開シンボル変更を1つの `RiskAssessment{risk, reversible}`
へ合流させ、既存の `requires_gate()` エスカレーションに相乗りする:

- 機微パスのみ該当、または公開シンボル変更のみ検出 → `Risk::Medium`、reversible
  （人間はコミットを revert できるので undoability ではなく *review 姿勢*の格上げ）。
- 両方該当 → `Risk::High`、reversible（複合的な review-worthiness だが、diff signal 単体は
  irreversible 化しない — irreversible 化するのは command-text 側の deploy/ローカル破壊判定のみ）。
- どちらも該当しない → `Risk::Low`（`classify_change` は `classify(text)` 単体と同一結果になり、
  baseline 後方互換）。
- deploy/push テキスト（例: `git push origin main`）は diff signal の有無に関わらず、
  従来どおり `classify` 側の tier-1/2 判定で `High`/irreversible/`requires_gate()` に force-gate
  される（`merge` は risk を上げる方向にのみ作用するため、diff 側が Low でも既存の force-gate は
  マスクされない）。

## 安定 rule id（overwatch 署名の粒度確保）

> **REVIEW-NEEDED**: コードから逆算 (2026-07-10 セッション)。人間レビュー前は正典としない。

**概要**: `rule_id.rs` は `detect` が出す人間向けの自由文 deny 理由（一部は `format!` で可変の
path/target を埋め込む。例: `"Write would replace {path} with empty content, wiping the file"`）を、
埋め込み可変テキストに依存しない **小さく安定した rule id** へ写像する（`rule_id(reason: &str) ->
&'static str`）。

**不変条件**:
- **自由文は overwatch の violation 署名には不適** — 同一種類の失敗が path/command 差で無数の distinct
  署名に断片化してしまう。rule id を挟むことで overwatch の cross-task 再発検知が「同じ種類の denial」を
  毎回同一署名として見る。
- **マッチは `detect` の文言と lockstep の固定 prefix / 部分文字列判定** — `detect` の wording 変更が
  ここへ反映されない場合、未認識の理由は既定 id へ落ちる（将来の文言変更で署名が静かに壊れないよう、
  両者は意図的に同期して保守する）。
